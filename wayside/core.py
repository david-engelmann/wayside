"""Core checkpoint store logic for wayside.

A store is a plain directory. Inside it live a lock file and one
directory per session, and inside each session one directory per
published checkpoint named by its sequence number. A checkpoint
holds a manifest, the payload files it recorded, and the memory
document. Every mutating operation holds an exclusive flock on the
lock file across the whole read and assign and write and rename, so
racing processes cannot reuse a sequence number or interleave a
publication. A checkpoint is assembled under a temporary name and
renamed into place only when it is complete, and it is never
modified afterwards.
"""

import fcntl
import hashlib
import json
import os
import re
import shutil
import uuid
from contextlib import contextmanager
from datetime import datetime, timezone
from pathlib import Path

FORMAT_NAME = "wayside-checkpoint/1"
LOCK_NAME = "wayside.lock"
SESSIONS_NAME = "sessions"
CHECKPOINTS_NAME = "checkpoints"
MANIFEST_NAME = "manifest.json"
FILES_NAME = "files"
MEMORY_NAME = "memory.bin"
ENV_FILE_NAME = "wayside.env"
MEMORY_FILE_NAME = "wayside.memory"
RESERVED_TOP_LEVEL = {ENV_FILE_NAME, MEMORY_FILE_NAME}
ENV_HEADER = "# Written by wayside. Env recorded by the checkpoint, restored as a file."

SESSION_PATTERN = re.compile(r"^[a-z0-9]([a-z0-9_-]*[a-z0-9])?$")
SESSION_MAX = 64
ENV_KEY_PATTERN = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
SHA256_PATTERN = re.compile(r"^[0-9a-f]{64}$")
SEQUENCE_DIR_PATTERN = re.compile(r"^[0-9]{6}$")

MANIFEST_KEYS = {"format", "session", "sequence", "created_at", "env", "memory", "files"}
MEMORY_KEYS = {"sha256", "size"}
FILE_KEYS = {"path", "sha256", "size"}
FORK_KEYS = {"session", "sequence"}


class WaysideError(Exception):
    """An error the CLI reports with a nonzero exit and no traceback."""

    exit_code = 1


class InvalidNameError(WaysideError):
    """A session id or a sequence is not in the shape the store accepts."""

    exit_code = 2


class NotFoundError(WaysideError):
    """The named session or checkpoint is not in the store."""

    exit_code = 2


class CorruptCheckpointError(WaysideError):
    """A manifest or a payload cannot be read as it was recorded."""

    exit_code = 2


class TargetError(WaysideError):
    """The restore target is not a directory the store may write into."""

    exit_code = 2


def _is_int(value):
    return type(value) is int


def _sha256(data):
    return hashlib.sha256(data).hexdigest()


def validate_session_id(session):
    if (
        not isinstance(session, str)
        or len(session) > SESSION_MAX
        or not SESSION_PATTERN.fullmatch(session)
    ):
        raise InvalidNameError(
            f"invalid session id {session!r}, use one path safe component of "
            f"lowercase letters and digits and hyphens and underscores, at "
            f"most {SESSION_MAX} characters long"
        )


def _validate_sequence(sequence):
    if not _is_int(sequence) or sequence < 1:
        raise InvalidNameError(
            f"invalid sequence {sequence!r}, use a positive integer"
        )
    return sequence


def _validate_env(env, corrupt):
    """Check env pairs, raising the given error factory on any defect."""
    if not isinstance(env, dict):
        raise corrupt("the env is not an object of key and value pairs")
    for key, value in env.items():
        if not isinstance(key, str) or not ENV_KEY_PATTERN.fullmatch(key):
            raise corrupt(f"the env key {key!r} is not a sound env name")
        if not isinstance(value, str) or "\n" in value or "\x00" in value:
            raise corrupt(
                f"the env value for {key} is not a single line of text"
            )
    return env


def validate_env(env):
    def bad(why):
        return WaysideError(f"invalid env, {why}")

    return _validate_env(env, bad)


def _sessions_dir(root):
    return root / SESSIONS_NAME


def _session_dir(root, session):
    return _sessions_dir(root) / session


def _checkpoints_dir(root, session):
    return _session_dir(root, session) / CHECKPOINTS_NAME


def _sequence_name(sequence):
    return f"{sequence:06d}"


def _checkpoint_dir(root, session, sequence):
    return _checkpoints_dir(root, session) / _sequence_name(sequence)


@contextmanager
def _locked(root):
    try:
        root.mkdir(parents=True, exist_ok=True)
        fd = os.open(root / LOCK_NAME, os.O_RDWR | os.O_CREAT, 0o644)
    except OSError as exc:
        raise WaysideError(
            f"the store at {root} cannot be opened, {exc}"
        ) from exc
    try:
        fcntl.flock(fd, fcntl.LOCK_EX)
        yield
    finally:
        fcntl.flock(fd, fcntl.LOCK_UN)
        os.close(fd)


def _published_sequences(root, session):
    ck_dir = _checkpoints_dir(root, session)
    if not ck_dir.is_dir():
        return []
    sequences = []
    for entry in ck_dir.iterdir():
        if entry.is_dir() and SEQUENCE_DIR_PATTERN.fullmatch(entry.name):
            sequences.append(int(entry.name))
    return sorted(sequences)


def _require_session(root, session):
    if not _session_dir(root, session).is_dir():
        raise NotFoundError(
            f"session {session} is not in the store at {root}"
        )


class _DuplicateKeyError(ValueError):
    """A JSON object in a manifest repeated a key."""


def _object_without_duplicate_keys(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise _DuplicateKeyError(key)
        result[key] = value
    return result


def _corrupt(path, why):
    return CorruptCheckpointError(f"manifest at {path} is corrupt, {why}")


def _validate_recorded_path(rel, path):
    if (
        not isinstance(rel, str)
        or not rel
        or rel.startswith("/")
        or "\\" in rel
        or "\x00" in rel
    ):
        raise _corrupt(path, f"the recorded path {rel!r} is not a safe relative path")
    parts = rel.split("/")
    if any(part in ("", ".", "..") for part in parts):
        raise _corrupt(path, f"the recorded path {rel!r} is not a safe relative path")


def _validate_hash(value, path, what):
    if not isinstance(value, str) or not SHA256_PATTERN.fullmatch(value):
        raise _corrupt(path, f"the {what} carries no sound SHA-256")
    return value


def _validate_size(value, path, what):
    if not _is_int(value) or value < 0:
        raise _corrupt(path, f"the {what} carries no sound size")
    return value


def _validate_manifest(data, path, session, sequence):
    if not isinstance(data, dict):
        raise _corrupt(path, "the top level is not an object")
    keys = set(data)
    if not MANIFEST_KEYS.issubset(keys) or not keys.issubset(
        MANIFEST_KEYS | {"forked_from"}
    ):
        raise _corrupt(path, "the field set is not the recorded one")
    if data["format"] != FORMAT_NAME:
        raise _corrupt(path, f"the format is not {FORMAT_NAME}")
    if data["session"] != session:
        raise _corrupt(
            path, f"the manifest names session {data['session']!r} "
            f"where it sits in session {session}"
        )
    if not _is_int(data["sequence"]) or data["sequence"] != sequence:
        raise _corrupt(
            path, "the manifest sequence does not match the directory it sits in"
        )
    if not isinstance(data["created_at"], str):
        raise _corrupt(path, "the created_at is not text")
    try:
        datetime.fromisoformat(data["created_at"])
    except ValueError:
        raise _corrupt(
            path, "the created_at does not parse as a timestamp"
        ) from None

    def env_bad(why):
        return _corrupt(path, why)

    _validate_env(data["env"], env_bad)
    memory = data["memory"]
    if not isinstance(memory, dict) or set(memory) != MEMORY_KEYS:
        raise _corrupt(path, "the memory entry does not carry exactly sha256 and size")
    _validate_hash(memory["sha256"], path, "memory entry")
    _validate_size(memory["size"], path, "memory entry")
    files = data["files"]
    if not isinstance(files, list):
        raise _corrupt(path, "the files entry is not a list")
    seen = []
    for entry in files:
        if not isinstance(entry, dict) or set(entry) != FILE_KEYS:
            raise _corrupt(
                path, "a file entry does not carry exactly path and sha256 and size"
            )
        _validate_recorded_path(entry["path"], path)
        _validate_hash(entry["sha256"], path, f"file entry for {entry['path']}")
        _validate_size(entry["size"], path, f"file entry for {entry['path']}")
        seen.append(entry["path"])
    if seen != sorted(seen) or len(set(seen)) != len(seen):
        raise _corrupt(path, "the file entries are not in path order or repeat a path")
    if "forked_from" in data:
        fork = data["forked_from"]
        if not isinstance(fork, dict) or set(fork) != FORK_KEYS:
            raise _corrupt(
                path, "the forked_from entry does not carry exactly session and sequence"
            )
        try:
            validate_session_id(fork["session"])
        except InvalidNameError:
            raise _corrupt(
                path, "the forked_from session is not a safe session id"
            ) from None
        if not _is_int(fork["sequence"]) or fork["sequence"] < 1:
            raise _corrupt(path, "the forked_from sequence is not a positive integer")
    return data


def _load_manifest(root, session, sequence):
    """Read and strictly validate one manifest, changing nothing on disk."""
    ck = _checkpoint_dir(root, session, sequence)
    path = ck / MANIFEST_NAME
    if not path.is_file():
        raise NotFoundError(
            f"session {session} holds no checkpoint {sequence} in the store at {root}"
        )
    try:
        raw = path.read_text(encoding="utf-8")
    except UnicodeDecodeError as exc:
        raise _corrupt(path, f"it is not valid UTF-8 text, {exc}") from exc
    except OSError as exc:
        raise _corrupt(path, f"it cannot be read, {exc}") from exc
    try:
        data = json.loads(raw, object_pairs_hook=_object_without_duplicate_keys)
    except json.JSONDecodeError as exc:
        raise _corrupt(path, f"it is not valid JSON, {exc}") from exc
    except _DuplicateKeyError as exc:
        raise _corrupt(path, f"it repeats the key {exc}") from exc
    except RecursionError:
        raise _corrupt(path, "it nests too deeply to be read") from None
    return _validate_manifest(data, path, session, sequence)


def _read_verified(root, session, sequence, manifest):
    """Read every payload of a checkpoint and check it against the manifest.

    Returns the file entries as a list of relative path and bytes
    pairs in manifest order together with the memory bytes. Nothing
    is placed anywhere by this read, so a caller can verify a whole
    checkpoint before it writes a single byte of it.
    """
    ck = _checkpoint_dir(root, session, sequence)
    entries = []
    for entry in manifest["files"]:
        payload = ck / FILES_NAME / Path(*entry["path"].split("/"))
        if not payload.is_file():
            raise CorruptCheckpointError(
                f"checkpoint {sequence} of session {session} is corrupt, "
                f"the payload for {entry['path']} is missing"
            )
        try:
            data = payload.read_bytes()
        except OSError as exc:
            raise CorruptCheckpointError(
                f"checkpoint {sequence} of session {session} is corrupt, "
                f"the payload for {entry['path']} cannot be read, {exc}"
            ) from exc
        if len(data) != entry["size"] or _sha256(data) != entry["sha256"]:
            raise CorruptCheckpointError(
                f"checkpoint {sequence} of session {session} is corrupt, "
                f"the payload for {entry['path']} does not match its "
                "recorded hash and size"
            )
        entries.append((entry["path"], data))
    memory_path = ck / MEMORY_NAME
    record = manifest["memory"]
    if not memory_path.is_file():
        raise CorruptCheckpointError(
            f"checkpoint {sequence} of session {session} is corrupt, "
            "the memory document is missing"
        )
    try:
        memory = memory_path.read_bytes()
    except OSError as exc:
        raise CorruptCheckpointError(
            f"checkpoint {sequence} of session {session} is corrupt, "
            f"the memory document cannot be read, {exc}"
        ) from exc
    if len(memory) != record["size"] or _sha256(memory) != record["sha256"]:
        raise CorruptCheckpointError(
            f"checkpoint {sequence} of session {session} is corrupt, "
            "the memory document does not match its recorded hash and size"
        )
    return entries, memory


def _read_state_tree(state):
    """Read every regular file under the state directory, sorted by path.

    Symbolic links and non regular files are refused, because a link
    can name bytes outside the state directory and a checkpoint
    records only what the caller can see in the tree itself.
    """
    entries = []
    for dirpath, dirnames, filenames in os.walk(state):
        dirnames.sort()
        filenames.sort()
        for name in dirnames:
            path = Path(dirpath) / name
            if path.is_symlink():
                rel = path.relative_to(state).as_posix()
                raise WaysideError(
                    f"the state directory holds a symbolic link at {rel}, "
                    "which a checkpoint does not record"
                )
        for name in filenames:
            path = Path(dirpath) / name
            rel = path.relative_to(state).as_posix()
            if path.is_symlink():
                raise WaysideError(
                    f"the state directory holds a symbolic link at {rel}, "
                    "which a checkpoint does not record"
                )
            if not path.is_file():
                raise WaysideError(
                    f"the state directory holds a non regular file at {rel}, "
                    "which a checkpoint does not record"
                )
            if "/" not in rel and rel in RESERVED_TOP_LEVEL:
                raise WaysideError(
                    f"the state directory holds a top level file named {rel}, "
                    "which is a name restore writes, so a checkpoint does "
                    "not record it"
                )
            try:
                entries.append((rel, path.read_bytes()))
            except OSError as exc:
                raise WaysideError(
                    f"the state file at {path} cannot be read, {exc}"
                ) from exc
    entries.sort(key=lambda item: item[0])
    return entries


def _publish(root, session, sequence, manifest, entries, memory):
    """Assemble a checkpoint under a temporary name and rename it into place."""
    ck_dir = _checkpoints_dir(root, session)
    final = ck_dir / _sequence_name(sequence)
    if final.exists():
        raise WaysideError(
            f"checkpoint {sequence} of session {session} already stands "
            "in the store, which the lock should have prevented"
        )
    tmp = ck_dir / f".tmp-{uuid.uuid4().hex}"
    try:
        (tmp / FILES_NAME).mkdir(parents=True)
        for rel, data in entries:
            dest = tmp / FILES_NAME / Path(*rel.split("/"))
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_bytes(data)
        (tmp / MEMORY_NAME).write_bytes(memory)
        (tmp / MANIFEST_NAME).write_text(
            json.dumps(manifest, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        os.rename(tmp, final)
    except OSError as exc:
        shutil.rmtree(tmp, ignore_errors=True)
        raise WaysideError(
            f"the checkpoint cannot be written in the store at {root}, {exc}"
        ) from exc
    return manifest


def _new_manifest(session, sequence, env, entries, memory, forked_from=None):
    manifest = {
        "format": FORMAT_NAME,
        "session": session,
        "sequence": sequence,
        "created_at": datetime.now(timezone.utc).isoformat(),
        "env": dict(sorted(env.items())),
        "memory": {"sha256": _sha256(memory), "size": len(memory)},
        "files": [
            {"path": rel, "sha256": _sha256(data), "size": len(data)}
            for rel, data in entries
        ],
    }
    if forked_from is not None:
        manifest["forked_from"] = forked_from
    return manifest


def checkpoint(store, session, state_dir, env=None, memory=b""):
    """Record a new checkpoint of the session from the state directory.

    The state tree and the declared env pairs and the memory document
    are read and checked before the lock is taken, and the sequence
    number is then assigned and the checkpoint published while the
    store lock is held, so the read that decides the number can never
    be stale.
    """
    root = Path(store)
    validate_session_id(session)
    env = validate_env(dict(env) if env is not None else {})
    if not isinstance(memory, (bytes, bytearray)):
        raise WaysideError("the memory document must be bytes")
    memory = bytes(memory)
    state = Path(state_dir)
    if not state.is_dir():
        raise WaysideError(
            f"the state directory at {state} does not exist or is not a directory"
        )
    entries = _read_state_tree(state)
    with _locked(root):
        ck_dir = _checkpoints_dir(root, session)
        try:
            ck_dir.mkdir(parents=True, exist_ok=True)
        except OSError as exc:
            raise WaysideError(
                f"the store at {root} cannot be written, {exc}"
            ) from exc
        sequences = _published_sequences(root, session)
        sequence = sequences[-1] + 1 if sequences else 1
        manifest = _new_manifest(session, sequence, env, entries, memory)
        return _publish(root, session, sequence, manifest, entries, memory)


def _env_text(env):
    lines = [ENV_HEADER]
    for key in sorted(env):
        lines.append(f"{key}={env[key]}")
    return "\n".join(lines) + "\n"


def _place(target, manifest, entries, memory):
    target = Path(target)
    if target.exists() and not target.is_dir():
        raise TargetError(
            f"the restore target at {target} exists and is not a directory"
        )
    if target.is_dir() and any(target.iterdir()):
        raise TargetError(
            f"the restore target at {target} is not empty, "
            "and restore never overwrites a non empty directory"
        )
    try:
        target.mkdir(parents=True, exist_ok=True)
        for rel, data in entries:
            dest = target / Path(*rel.split("/"))
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_bytes(data)
        (target / MEMORY_FILE_NAME).write_bytes(memory)
        (target / ENV_FILE_NAME).write_text(
            _env_text(manifest["env"]), encoding="utf-8"
        )
    except OSError as exc:
        raise WaysideError(
            f"the restore target at {target} cannot be written, {exc}"
        ) from exc


def restore(store, session, sequence, target):
    """Materialize one checkpoint into the target directory.

    Every recorded hash is verified before anything is placed, so a
    corrupt checkpoint leaves the target exactly as it was found.
    """
    root = Path(store)
    validate_session_id(session)
    _validate_sequence(sequence)
    manifest = _load_manifest(root, session, sequence)
    entries, memory = _read_verified(root, session, sequence, manifest)
    _place(target, manifest, entries, memory)
    return manifest


def rewind(store, session, sequence, target):
    """Restore a checkpoint that stands earlier than the newest one."""
    root = Path(store)
    validate_session_id(session)
    _validate_sequence(sequence)
    _require_session(root, session)
    sequences = _published_sequences(root, session)
    if not sequences:
        raise NotFoundError(
            f"session {session} holds no checkpoints in the store at {root}"
        )
    if sequence == sequences[-1]:
        raise WaysideError(
            f"checkpoint {sequence} is the newest checkpoint of session "
            f"{session}, and rewind names an earlier one, so use restore "
            "for the newest"
        )
    if sequence not in sequences:
        raise NotFoundError(
            f"session {session} holds no checkpoint {sequence} in the store at {root}"
        )
    return restore(store, session, sequence, target)


def fork(store, source_session, sequence, new_session):
    """Start a new session whose first checkpoint copies a named one.

    The source checkpoint is verified against its own hashes before
    anything is copied, and the new manifest records the lineage.
    """
    root = Path(store)
    validate_session_id(source_session)
    validate_session_id(new_session)
    _validate_sequence(sequence)
    with _locked(root):
        manifest = _load_manifest(root, source_session, sequence)
        entries, memory = _read_verified(root, source_session, sequence, manifest)
        if _published_sequences(root, new_session):
            raise WaysideError(
                f"session {new_session} already holds a checkpoint in "
                f"the store at {root}, so fork will not overwrite it"
            )
        ck_dir = _checkpoints_dir(root, new_session)
        try:
            ck_dir.mkdir(parents=True, exist_ok=True)
        except OSError as exc:
            raise WaysideError(
                f"the store at {root} cannot be written, {exc}"
            ) from exc
        new_manifest = _new_manifest(
            new_session,
            1,
            manifest["env"],
            entries,
            memory,
            forked_from={"session": source_session, "sequence": sequence},
        )
        return _publish(root, new_session, 1, new_manifest, entries, memory)


def list_checkpoints(store, session):
    """Return sequence and created_at for each published checkpoint in order."""
    root = Path(store)
    validate_session_id(session)
    _require_session(root, session)
    result = []
    for sequence in _published_sequences(root, session):
        manifest = _load_manifest(root, session, sequence)
        result.append(
            {"sequence": sequence, "created_at": manifest["created_at"]}
        )
    return result


def show(store, session, sequence):
    """Return the validated manifest of one checkpoint."""
    root = Path(store)
    validate_session_id(session)
    _validate_sequence(sequence)
    return _load_manifest(root, session, sequence)
