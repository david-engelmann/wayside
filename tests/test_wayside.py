import hashlib
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from wayside import (
    CorruptCheckpointError,
    InvalidNameError,
    NotFoundError,
    TargetError,
    WaysideError,
    checkpoint,
    fork,
    list_checkpoints,
    restore,
    rewind,
    show,
)

EMPTY_SHA256 = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"


def make_state(base, files):
    state = Path(base)
    state.mkdir(parents=True, exist_ok=True)
    for rel, data in files.items():
        path = state / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
    return state


def checkpoint_dir(store, session, sequence):
    return (
        Path(store)
        / "sessions"
        / session
        / "checkpoints"
        / f"{sequence:06d}"
    )


def manifest_path(store, session, sequence):
    return checkpoint_dir(store, session, sequence) / "manifest.json"


def run_cli(store, *args):
    env = dict(os.environ)
    env["PYTHONPATH"] = str(ROOT)
    return subprocess.run(
        [sys.executable, "-m", "wayside", "--store", str(store), *args],
        capture_output=True,
        text=True,
        cwd=ROOT,
        env=env,
    )


def hash_tree(root):
    result = {}
    for path in sorted(Path(root).rglob("*")):
        if path.is_file():
            rel = path.relative_to(root).as_posix()
            result[rel] = hashlib.sha256(path.read_bytes()).hexdigest()
    return result


def test_checkpoint_records_files_env_and_memory(tmp_path):
    store = tmp_path / "store"
    state = make_state(
        tmp_path / "state",
        {"notes.txt": b"synthetic notes", "work/plan.txt": b"synthetic plan"},
    )
    manifest = checkpoint(
        store,
        "alpha",
        state,
        env={"MODE": "synthetic", "LEVEL": "3"},
        memory=b"synthetic memory document",
    )
    assert manifest["format"] == "wayside-checkpoint/1"
    assert manifest["session"] == "alpha"
    assert manifest["sequence"] == 1
    assert manifest["env"] == {"LEVEL": "3", "MODE": "synthetic"}
    assert [entry["path"] for entry in manifest["files"]] == [
        "notes.txt",
        "work/plan.txt",
    ]
    for entry in manifest["files"]:
        payload = (
            checkpoint_dir(store, "alpha", 1) / "files" / entry["path"]
        )
        assert payload.read_bytes() == (state / entry["path"]).read_bytes()
        assert entry["size"] == len(payload.read_bytes())
        assert (
            entry["sha256"]
            == hashlib.sha256(payload.read_bytes()).hexdigest()
        )
    assert manifest["memory"]["size"] == len(b"synthetic memory document")
    assert show(store, "alpha", 1) == manifest
    assert [entry["sequence"] for entry in list_checkpoints(store, "alpha")] == [1]


def test_checkpoint_never_records_the_live_environment(tmp_path, monkeypatch):
    monkeypatch.setenv("WAYSIDE_UNDECLARED_PROBE", "synthetic-undeclared-value")
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"a"})
    manifest = checkpoint(store, "alpha", state, env={"DECLARED": "yes"})
    assert manifest["env"] == {"DECLARED": "yes"}
    raw = manifest_path(store, "alpha", 1).read_text(encoding="utf-8")
    assert "WAYSIDE_UNDECLARED_PROBE" not in raw
    assert "synthetic-undeclared-value" not in raw


def test_checkpoint_refuses_a_missing_state_directory(tmp_path):
    with pytest.raises(WaysideError):
        checkpoint(tmp_path / "store", "alpha", tmp_path / "nope")


def test_checkpoint_refuses_a_symlink_in_the_state_tree(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"real.txt": b"real"})
    (state / "link.txt").symlink_to(state / "real.txt")
    with pytest.raises(WaysideError):
        checkpoint(store, "alpha", state)
    (state / "link.txt").unlink()
    (state / "linked-dir").symlink_to(state, target_is_directory=True)
    with pytest.raises(WaysideError):
        checkpoint(store, "alpha", state)


def test_checkpoint_refuses_a_non_regular_file_in_the_state_tree(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"real.txt": b"real"})
    os.mkfifo(state / "pipe")
    with pytest.raises(WaysideError):
        checkpoint(store, "alpha", state)


def test_checkpoint_refuses_a_reserved_top_level_name(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"wayside.env": b"planted"})
    with pytest.raises(WaysideError):
        checkpoint(store, "alpha", state)
    state = make_state(
        tmp_path / "state2", {"nested/wayside.env": b"nested is fine"}
    )
    manifest = checkpoint(store, "alpha", state)
    assert [entry["path"] for entry in manifest["files"]] == [
        "nested/wayside.env"
    ]


def test_checkpoint_of_an_empty_state_tree_records_no_files(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {})
    manifest = checkpoint(store, "alpha", state)
    assert manifest["files"] == []
    target = tmp_path / "target"
    restore(store, "alpha", 1, target)
    assert sorted(p.name for p in target.iterdir()) == [
        "wayside.env",
        "wayside.memory",
    ]


def test_memory_defaults_to_an_empty_document(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"a"})
    manifest = checkpoint(store, "alpha", state)
    assert manifest["memory"] == {"sha256": EMPTY_SHA256, "size": 0}
    target = tmp_path / "target"
    restore(store, "alpha", 1, target)
    assert (target / "wayside.memory").read_bytes() == b""


def test_env_values_with_a_newline_or_nul_are_refused(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"a"})
    with pytest.raises(WaysideError):
        checkpoint(store, "alpha", state, env={"BAD": "two\nlines"})
    with pytest.raises(WaysideError):
        checkpoint(store, "alpha", state, env={"BAD": "has\x00nul"})
    with pytest.raises(WaysideError):
        checkpoint(store, "alpha", state, env={"1BAD": "fine value"})


def test_sequences_increment_and_list_in_order(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"a"})
    for expected in (1, 2, 3):
        manifest = checkpoint(store, "alpha", state)
        assert manifest["sequence"] == expected
    entries = list_checkpoints(store, "alpha")
    assert [entry["sequence"] for entry in entries] == [1, 2, 3]


def test_restore_materializes_exactly(tmp_path):
    store = tmp_path / "store"
    state = make_state(
        tmp_path / "state",
        {"top.txt": b"top bytes", "deep/nested/file.bin": bytes(range(256))},
    )
    checkpoint(
        store,
        "alpha",
        state,
        env={"ZED": "last", "FIRST": "1"},
        memory=b"memory payload",
    )
    target = tmp_path / "target"
    restore(store, "alpha", 1, target)
    assert (target / "top.txt").read_bytes() == b"top bytes"
    assert (target / "deep/nested/file.bin").read_bytes() == bytes(range(256))
    assert (target / "wayside.memory").read_bytes() == b"memory payload"
    env_lines = (target / "wayside.env").read_text(encoding="utf-8").splitlines()
    assert env_lines[0].startswith("#")
    assert env_lines[1:] == ["FIRST=1", "ZED=last"]


def test_restore_into_an_empty_existing_directory(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"a"})
    checkpoint(store, "alpha", state)
    target = tmp_path / "target"
    target.mkdir()
    restore(store, "alpha", 1, target)
    assert (target / "a.txt").read_bytes() == b"a"


def test_restore_refuses_a_non_empty_target_and_leaves_it_untouched(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"a"})
    checkpoint(store, "alpha", state)
    target = tmp_path / "target"
    target.mkdir()
    (target / "sentinel.txt").write_bytes(b"do not touch")
    with pytest.raises(TargetError):
        restore(store, "alpha", 1, target)
    assert hash_tree(target) == {
        "sentinel.txt": hashlib.sha256(b"do not touch").hexdigest()
    }


def test_restore_refuses_a_file_at_the_target_path(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"a"})
    checkpoint(store, "alpha", state)
    target = tmp_path / "target"
    target.write_bytes(b"a file, not a directory")
    with pytest.raises(TargetError):
        restore(store, "alpha", 1, target)
    assert target.read_bytes() == b"a file, not a directory"


def test_flipped_payload_byte_fails_restore_and_places_nothing(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"original bytes"})
    checkpoint(store, "alpha", state)
    payload = checkpoint_dir(store, "alpha", 1) / "files" / "a.txt"
    damaged = bytearray(payload.read_bytes())
    damaged[0] ^= 0xFF
    payload.write_bytes(bytes(damaged))
    target = tmp_path / "target"
    with pytest.raises(CorruptCheckpointError):
        restore(store, "alpha", 1, target)
    assert not target.exists()


def test_flipped_memory_byte_fails_restore_and_places_nothing(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"a"})
    checkpoint(store, "alpha", state, memory=b"memory bytes")
    memory = checkpoint_dir(store, "alpha", 1) / "memory.bin"
    damaged = bytearray(memory.read_bytes())
    damaged[-1] ^= 0xFF
    memory.write_bytes(bytes(damaged))
    target = tmp_path / "target"
    with pytest.raises(CorruptCheckpointError):
        restore(store, "alpha", 1, target)
    assert not target.exists()


def test_a_checkpoint_never_changes_once_written(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"first"})
    checkpoint(store, "alpha", state, memory=b"one")
    before = hash_tree(checkpoint_dir(store, "alpha", 1))
    (state / "a.txt").write_bytes(b"second")
    checkpoint(store, "alpha", state, memory=b"two")
    checkpoint(store, "alpha", state, memory=b"three")
    assert hash_tree(checkpoint_dir(store, "alpha", 1)) == before


def damage_manifest(store, session, sequence, damage):
    path = manifest_path(store, session, sequence)
    text = path.read_text(encoding="utf-8")
    path.write_text(damage(text), encoding="utf-8")


def test_manifest_with_a_duplicate_top_level_key_is_refused(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"a"})
    checkpoint(store, "alpha", state)

    def damage(text):
        return text.replace(
            '"format": "wayside-checkpoint/1",',
            '"format": "wayside-checkpoint/1",\n  "format": "wayside-checkpoint/1",',
            1,
        )

    damage_manifest(store, "alpha", 1, damage)
    with pytest.raises(CorruptCheckpointError):
        show(store, "alpha", 1)


def test_manifest_with_a_duplicate_nested_key_is_refused(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"a"})
    checkpoint(store, "alpha", state)

    def damage(text):
        return text.replace(
            '"path": "a.txt",', '"path": "a.txt",\n      "path": "a.txt",', 1
        )

    damage_manifest(store, "alpha", 1, damage)
    with pytest.raises(CorruptCheckpointError):
        restore(store, "alpha", 1, tmp_path / "target")


def test_manifest_with_a_wrong_format_is_refused(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"a"})
    checkpoint(store, "alpha", state)
    damage_manifest(
        store,
        "alpha",
        1,
        lambda text: text.replace("wayside-checkpoint/1", "wayside-checkpoint/2"),
    )
    with pytest.raises(CorruptCheckpointError):
        show(store, "alpha", 1)


def test_manifest_with_a_missing_or_extra_field_is_refused(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"a"})
    checkpoint(store, "alpha", state)
    manifest = json.loads(manifest_path(store, "alpha", 1).read_text())
    del manifest["env"]
    manifest_path(store, "alpha", 1).write_text(json.dumps(manifest))
    with pytest.raises(CorruptCheckpointError):
        show(store, "alpha", 1)
    checkpoint(store, "beta", state)
    manifest = json.loads(manifest_path(store, "beta", 1).read_text())
    manifest["surprise"] = 1
    manifest_path(store, "beta", 1).write_text(json.dumps(manifest))
    with pytest.raises(CorruptCheckpointError):
        show(store, "beta", 1)


def test_manifest_with_undecodable_bytes_is_refused(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"a"})
    checkpoint(store, "alpha", state)
    manifest_path(store, "alpha", 1).write_bytes(b"\xff\xfe not a manifest")
    with pytest.raises(CorruptCheckpointError):
        show(store, "alpha", 1)


def test_manifest_naming_a_foreign_session_or_sequence_is_refused(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"a"})
    checkpoint(store, "alpha", state)
    manifest = json.loads(manifest_path(store, "alpha", 1).read_text())
    manifest["session"] = "other"
    manifest_path(store, "alpha", 1).write_text(json.dumps(manifest))
    with pytest.raises(CorruptCheckpointError):
        show(store, "alpha", 1)
    checkpoint(store, "beta", state)
    manifest = json.loads(manifest_path(store, "beta", 1).read_text())
    manifest["sequence"] = 99
    manifest_path(store, "beta", 1).write_text(json.dumps(manifest))
    with pytest.raises(CorruptCheckpointError):
        show(store, "beta", 1)


@pytest.mark.parametrize(
    "bad_path", ["../escape.txt", "/etc/passwd", "a/../../escape.txt", "a\\b.txt"]
)
def test_manifest_with_an_unsafe_recorded_path_is_refused(tmp_path, bad_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"a"})
    checkpoint(store, "alpha", state)
    manifest = json.loads(manifest_path(store, "alpha", 1).read_text())
    manifest["files"][0]["path"] = bad_path
    manifest_path(store, "alpha", 1).write_text(json.dumps(manifest))
    with pytest.raises(CorruptCheckpointError):
        show(store, "alpha", 1)
    target = tmp_path / "target"
    with pytest.raises(CorruptCheckpointError):
        restore(store, "alpha", 1, target)
    assert not target.exists()
    assert not (tmp_path / "escape.txt").exists()


@pytest.mark.parametrize(
    "bad_id",
    ["../x", "a/b", "a\\b", "", "UPPER", "-lead", "a b", "a..b", "x" * 65],
)
def test_unsafe_session_ids_are_refused(tmp_path, bad_id):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"a"})
    with pytest.raises(InvalidNameError):
        checkpoint(store, bad_id, state)
    with pytest.raises(InvalidNameError):
        list_checkpoints(store, bad_id)


def test_list_and_show_on_a_missing_session_are_clean_errors(tmp_path):
    store = tmp_path / "store"
    with pytest.raises(NotFoundError):
        list_checkpoints(store, "ghost")
    with pytest.raises(NotFoundError):
        show(store, "ghost", 1)
    state = make_state(tmp_path / "state", {"a.txt": b"a"})
    checkpoint(store, "alpha", state)
    with pytest.raises(NotFoundError):
        show(store, "alpha", 7)


def test_planted_residue_is_ignored_by_list_and_sequence(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"a"})
    checkpoint(store, "alpha", state)
    residue = (
        Path(store) / "sessions" / "alpha" / "checkpoints" / ".tmp-deadbeef"
    )
    residue.mkdir(parents=True)
    (residue / "manifest.json").write_text("{}", encoding="utf-8")
    assert [e["sequence"] for e in list_checkpoints(store, "alpha")] == [1]
    manifest = checkpoint(store, "alpha", state)
    assert manifest["sequence"] == 2
    ghost = Path(store) / "sessions" / "ghost" / "checkpoints" / ".tmp-cafe"
    ghost.mkdir(parents=True)
    (ghost / "manifest.json").write_text("{}", encoding="utf-8")
    assert list_checkpoints(store, "ghost") == []


def test_parallel_writers_publish_a_gapless_sequence(tmp_path):
    store = tmp_path / "store"
    driver = tmp_path / "driver.py"
    driver.write_text(
        "import sys\n"
        f"sys.path.insert(0, {str(ROOT)!r})\n"
        "from wayside import checkpoint\n"
        "store, session, state, count = sys.argv[1:5]\n"
        "for _ in range(int(count)):\n"
        "    checkpoint(store, session, state, env={'DRIVER': session}, "
        "memory=b'driver memory')\n",
        encoding="utf-8",
    )
    workers = 8
    each = 3
    processes = []
    for index in range(workers):
        state = make_state(
            tmp_path / f"state{index}", {"work.txt": f"worker {index}".encode()}
        )
        processes.append(
            subprocess.Popen(
                [
                    sys.executable,
                    str(driver),
                    str(store),
                    "alpha",
                    str(state),
                    str(each),
                ],
            )
        )
    for proc in processes:
        assert proc.wait() == 0
    entries = list_checkpoints(store, "alpha")
    assert [entry["sequence"] for entry in entries] == list(
        range(1, workers * each + 1)
    )
    for sequence in range(1, workers * each + 1):
        assert show(store, "alpha", sequence)["sequence"] == sequence


def test_killed_writer_leaves_no_visible_checkpoint(tmp_path):
    state = tmp_path / "big-state"
    state.mkdir()
    for index in range(2500):
        (state / f"file{index:04d}.bin").write_bytes(b"x" * 8192)
    driver = tmp_path / "driver.py"
    driver.write_text(
        "import sys\n"
        f"sys.path.insert(0, {str(ROOT)!r})\n"
        "from wayside import checkpoint\n"
        "checkpoint(sys.argv[1], sys.argv[2], sys.argv[3], memory=b'big')\n",
        encoding="utf-8",
    )
    killed_mid_write = False
    for attempt in range(5):
        store = tmp_path / f"store{attempt}"
        proc = subprocess.Popen(
            [sys.executable, str(driver), str(store), "victim", str(state)]
        )
        ck_dir = Path(store) / "sessions" / "victim" / "checkpoints"
        seen_tmp = False
        while proc.poll() is None:
            if ck_dir.is_dir():
                for entry in os.scandir(ck_dir):
                    if entry.name.startswith(".tmp-"):
                        seen_tmp = True
                        break
            if seen_tmp:
                break
            time.sleep(0.001)
        if proc.poll() is None:
            proc.kill()
            killed_mid_write = killed_mid_write or seen_tmp
        proc.wait()
        try:
            visible = list_checkpoints(store, "victim")
        except NotFoundError:
            visible = []
        assert visible == []
        if killed_mid_write:
            break
    assert killed_mid_write
    manifest = checkpoint(store, "victim", make_state(tmp_path / "small", {"a": b"a"}))
    assert manifest["sequence"] == 1


def test_fork_carries_content_and_records_lineage(tmp_path):
    store = tmp_path / "store"
    state = make_state(
        tmp_path / "state", {"a.txt": b"alpha bytes", "dir/b.txt": b"beta"}
    )
    checkpoint(store, "alpha", state, env={"K": "V"}, memory=b"alpha memory")
    (state / "a.txt").write_bytes(b"changed later")
    checkpoint(store, "alpha", state, memory=b"later memory")
    manifest = fork(store, "alpha", 1, "beta")
    assert manifest["session"] == "beta"
    assert manifest["sequence"] == 1
    assert manifest["forked_from"] == {"session": "alpha", "sequence": 1}
    assert [e["sequence"] for e in list_checkpoints(store, "beta")] == [1]
    from_alpha = tmp_path / "from-alpha"
    from_beta = tmp_path / "from-beta"
    restore(store, "alpha", 1, from_alpha)
    restore(store, "beta", 1, from_beta)
    assert hash_tree(from_alpha) == hash_tree(from_beta)
    assert (from_beta / "a.txt").read_bytes() == b"alpha bytes"


def test_fork_refuses_an_existing_session(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"a"})
    checkpoint(store, "alpha", state)
    checkpoint(store, "beta", state)
    with pytest.raises(WaysideError):
        fork(store, "alpha", 1, "beta")


def test_fork_refuses_a_corrupt_source_and_creates_nothing(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"original"})
    checkpoint(store, "alpha", state)
    payload = checkpoint_dir(store, "alpha", 1) / "files" / "a.txt"
    damaged = bytearray(payload.read_bytes())
    damaged[0] ^= 0xFF
    payload.write_bytes(bytes(damaged))
    with pytest.raises(CorruptCheckpointError):
        fork(store, "alpha", 1, "beta")
    assert not (Path(store) / "sessions" / "beta").exists()


def test_rewind_restores_an_earlier_state(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"version one"})
    checkpoint(store, "alpha", state)
    (state / "a.txt").write_bytes(b"version two")
    checkpoint(store, "alpha", state)
    (state / "a.txt").write_bytes(b"version three")
    checkpoint(store, "alpha", state)
    target = tmp_path / "target"
    rewind(store, "alpha", 1, target)
    assert (target / "a.txt").read_bytes() == b"version one"


def test_rewind_refuses_the_newest_and_a_missing_sequence(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"a"})
    checkpoint(store, "alpha", state)
    checkpoint(store, "alpha", state)
    with pytest.raises(WaysideError):
        rewind(store, "alpha", 2, tmp_path / "t1")
    with pytest.raises(NotFoundError):
        rewind(store, "alpha", 9, tmp_path / "t2")


def test_cli_checkpoint_restore_round_trip(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"cli bytes"})
    memory_file = tmp_path / "memory.txt"
    memory_file.write_bytes(b"cli memory")
    result = run_cli(
        store,
        "checkpoint",
        "alpha",
        "--state",
        str(state),
        "--memory",
        str(memory_file),
        "--env",
        "MODE=cli",
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == "session alpha checkpoint 1"
    result = run_cli(store, "list", "alpha")
    assert result.returncode == 0, result.stderr
    lines = result.stdout.strip().splitlines()
    assert len(lines) == 1
    assert lines[0].split(" ", 1)[0] == "1"
    result = run_cli(store, "show", "alpha", "1")
    assert result.returncode == 0, result.stderr
    shown = json.loads(result.stdout)
    assert shown["env"] == {"MODE": "cli"}
    target = tmp_path / "target"
    result = run_cli(store, "restore", "alpha", "1", "--target", str(target))
    assert result.returncode == 0, result.stderr
    assert (target / "a.txt").read_bytes() == b"cli bytes"
    assert (target / "wayside.memory").read_bytes() == b"cli memory"


def test_cli_reports_a_corrupt_manifest_without_a_traceback(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"a"})
    assert run_cli(store, "checkpoint", "alpha", "--state", str(state)).returncode == 0
    manifest_path(store, "alpha", 1).write_bytes(b"\xff\xfe broken")
    result = run_cli(store, "show", "alpha", "1")
    assert result.returncode == 2
    assert result.stderr.startswith("error:")
    assert "Traceback" not in result.stderr


def test_cli_refuses_an_unsafe_session_id_without_a_traceback(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"a"})
    result = run_cli(store, "checkpoint", "../escape", "--state", str(state))
    assert result.returncode == 2
    assert result.stderr.startswith("error:")
    assert "Traceback" not in result.stderr
    assert not (tmp_path / "escape").exists()


def test_checkpoint_refuses_a_symlinked_session_directory(tmp_path):
    store = tmp_path / "store"
    outside = tmp_path / "outside"
    outside.mkdir()
    (Path(store) / "sessions").mkdir(parents=True)
    (Path(store) / "sessions" / "ghost").symlink_to(
        outside, target_is_directory=True
    )
    state = make_state(tmp_path / "state", {"a.txt": b"a"})
    with pytest.raises(CorruptCheckpointError):
        checkpoint(store, "ghost", state)
    assert list(outside.iterdir()) == []


def test_reads_refuse_a_symlinked_session_directory(tmp_path):
    store = tmp_path / "store"
    planted = tmp_path / "planted"
    state = make_state(tmp_path / "state", {"planted.txt": b"outside-bytes"})
    checkpoint(planted, "ghost", state)
    (Path(store) / "sessions").mkdir(parents=True)
    (Path(store) / "sessions" / "ghost").symlink_to(
        planted / "sessions" / "ghost", target_is_directory=True
    )
    with pytest.raises(CorruptCheckpointError):
        list_checkpoints(store, "ghost")
    with pytest.raises(CorruptCheckpointError):
        show(store, "ghost", 1)
    target = tmp_path / "target"
    with pytest.raises(CorruptCheckpointError):
        restore(store, "ghost", 1, target)
    assert not target.exists()


def test_reads_and_writes_refuse_a_symlinked_checkpoints_directory(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"a"})
    checkpoint(store, "alpha", state)
    ckpts = Path(store) / "sessions" / "alpha" / "checkpoints"
    moved = tmp_path / "moved-checkpoints"
    ckpts.rename(moved)
    ckpts.symlink_to(moved, target_is_directory=True)
    with pytest.raises(CorruptCheckpointError):
        list_checkpoints(store, "alpha")
    with pytest.raises(CorruptCheckpointError):
        checkpoint(store, "alpha", state)
    assert sorted(p.name for p in moved.iterdir()) == ["000001"]


def test_reads_refuse_a_symlinked_sequence_directory(tmp_path):
    store = tmp_path / "store"
    planted = tmp_path / "planted"
    state = make_state(tmp_path / "state", {"a.txt": b"a"})
    checkpoint(store, "alpha", state)
    checkpoint(store, "alpha", state)
    checkpoint(planted, "alpha", state)
    link = checkpoint_dir(store, "alpha", 2)
    shutil.rmtree(link)
    link.symlink_to(
        checkpoint_dir(planted, "alpha", 1), target_is_directory=True
    )
    with pytest.raises(CorruptCheckpointError):
        show(store, "alpha", 2)
    with pytest.raises(CorruptCheckpointError):
        list_checkpoints(store, "alpha")
    target = tmp_path / "target"
    with pytest.raises(CorruptCheckpointError):
        restore(store, "alpha", 2, target)
    assert not target.exists()


def test_reads_refuse_a_symlinked_manifest(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"a"})
    checkpoint(store, "alpha", state)
    path = manifest_path(store, "alpha", 1)
    copy = tmp_path / "manifest-copy.json"
    copy.write_bytes(path.read_bytes())
    path.unlink()
    path.symlink_to(copy)
    with pytest.raises(CorruptCheckpointError):
        show(store, "alpha", 1)


def test_restore_refuses_a_symlinked_payload(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"same bytes"})
    checkpoint(store, "alpha", state)
    payload = checkpoint_dir(store, "alpha", 1) / "files" / "a.txt"
    twin = tmp_path / "twin.txt"
    twin.write_bytes(b"same bytes")
    payload.unlink()
    payload.symlink_to(twin)
    target = tmp_path / "target"
    with pytest.raises(CorruptCheckpointError):
        restore(store, "alpha", 1, target)
    assert not target.exists()


@pytest.mark.parametrize("reserved", ["wayside.env", "wayside.memory"])
def test_manifest_recording_a_reserved_name_is_refused(tmp_path, reserved):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"payload-bytes-here"})
    checkpoint(store, "alpha", state, env={"K": "V"}, memory=b"the memory")
    ck = checkpoint_dir(store, "alpha", 1)
    (ck / "files" / "a.txt").rename(ck / "files" / reserved)
    manifest = json.loads(manifest_path(store, "alpha", 1).read_text())
    manifest["files"][0]["path"] = reserved
    manifest_path(store, "alpha", 1).write_text(json.dumps(manifest))
    with pytest.raises(CorruptCheckpointError):
        show(store, "alpha", 1)
    target = tmp_path / "target"
    with pytest.raises(CorruptCheckpointError):
        restore(store, "alpha", 1, target)
    assert not target.exists()


def test_a_nested_reserved_name_restores_as_a_payload(tmp_path):
    store = tmp_path / "store"
    state = make_state(
        tmp_path / "state", {"sub/wayside.env": b"nested payload"}
    )
    checkpoint(store, "alpha", state, env={"K": "V"})
    target = tmp_path / "target"
    restore(store, "alpha", 1, target)
    assert (target / "sub" / "wayside.env").read_bytes() == b"nested payload"
    assert "K=V" in (target / "wayside.env").read_text(encoding="utf-8")


def test_sequences_past_a_million_stay_visible(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"a"})
    checkpoint(store, "alpha", state)
    shutil.copytree(
        checkpoint_dir(store, "alpha", 1), checkpoint_dir(store, "alpha", 999999)
    )
    manifest = json.loads(manifest_path(store, "alpha", 999999).read_text())
    manifest["sequence"] = 999999
    manifest_path(store, "alpha", 999999).write_text(json.dumps(manifest))
    first = checkpoint(store, "alpha", state)
    assert first["sequence"] == 1000000
    second = checkpoint(store, "alpha", state)
    assert second["sequence"] == 1000001
    assert [e["sequence"] for e in list_checkpoints(store, "alpha")] == [
        1,
        999999,
        1000000,
        1000001,
    ]
    assert show(store, "alpha", 1000000)["sequence"] == 1000000


def test_a_sequence_directory_without_a_manifest_is_corruption(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"a"})
    checkpoint(store, "alpha", state)
    checkpoint_dir(store, "alpha", 5).mkdir(parents=True)
    with pytest.raises(CorruptCheckpointError) as caught:
        list_checkpoints(store, "alpha")
    assert "manifest" in str(caught.value)
    with pytest.raises(CorruptCheckpointError):
        show(store, "alpha", 5)
    with pytest.raises(CorruptCheckpointError):
        restore(store, "alpha", 5, tmp_path / "target")
    manifest = checkpoint(store, "alpha", state)
    assert manifest["sequence"] == 6


def damage_manifest_json(store, session, sequence, damage):
    path = manifest_path(store, session, sequence)
    manifest = json.loads(path.read_text(encoding="utf-8"))
    damage(manifest)
    path.write_text(json.dumps(manifest), encoding="utf-8")


@pytest.mark.parametrize(
    "damage",
    [
        lambda m: m["files"][0].update(size="3"),
        lambda m: m.update(env=["K=V"]),
        lambda m: m.update(files={}),
        lambda m: m.update(sequence="1"),
        lambda m: m.update(created_at=12345),
        lambda m: m.update(created_at="not a timestamp"),
    ],
    ids=[
        "size-as-string",
        "env-as-list",
        "files-as-object",
        "sequence-as-string",
        "created-at-not-text",
        "created-at-unparseable",
    ],
)
def test_manifest_with_a_wrong_typed_value_is_refused(tmp_path, damage):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"a"})
    checkpoint(store, "alpha", state, env={"K": "V"})
    damage_manifest_json(store, "alpha", 1, damage)
    with pytest.raises(CorruptCheckpointError):
        show(store, "alpha", 1)


@pytest.mark.parametrize("good_id", ["a", "x" * 64, "a-b_2"])
def test_session_id_accepting_edges(tmp_path, good_id):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"a"})
    manifest = checkpoint(store, good_id, state)
    assert manifest["session"] == good_id


@pytest.mark.parametrize("bad_id", ["a-", "a_"])
def test_session_ids_ending_in_a_separator_are_refused(tmp_path, bad_id):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"a"})
    with pytest.raises(InvalidNameError):
        checkpoint(store, bad_id, state)


def run_cli_timed(store, *args, timeout=20):
    env = dict(os.environ)
    env["PYTHONPATH"] = str(ROOT)
    try:
        return subprocess.run(
            [sys.executable, "-m", "wayside", "--store", str(store), *args],
            capture_output=True,
            text=True,
            cwd=ROOT,
            env=env,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired:
        pytest.fail("the command did not finish, it hung")


def test_cli_memory_fifo_is_refused_promptly(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"a"})
    fifo = tmp_path / "memory.fifo"
    os.mkfifo(fifo)
    result = run_cli_timed(
        store,
        "checkpoint",
        "alpha",
        "--state",
        str(state),
        "--memory",
        str(fifo),
    )
    assert result.returncode == 1
    assert result.stderr.startswith("error:")
    assert "not a regular file" in result.stderr
    assert "Traceback" not in result.stderr


def test_cli_memory_directory_and_missing_file_are_refused(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"a"})
    result = run_cli_timed(
        store,
        "checkpoint",
        "alpha",
        "--state",
        str(state),
        "--memory",
        str(tmp_path),
    )
    assert result.returncode == 1
    assert result.stderr.startswith("error:")
    assert "Traceback" not in result.stderr
    result = run_cli_timed(
        store,
        "checkpoint",
        "alpha",
        "--state",
        str(state),
        "--memory",
        str(tmp_path / "no-memory-here"),
    )
    assert result.returncode == 1
    assert result.stderr.startswith("error:")
    assert "Traceback" not in result.stderr


def test_cli_sequence_errors_share_the_invalid_name_exit_code(tmp_path):
    store = tmp_path / "store"
    state = make_state(tmp_path / "state", {"a.txt": b"a"})
    assert (
        run_cli(store, "checkpoint", "alpha", "--state", str(state)).returncode
        == 0
    )
    target = tmp_path / "target"
    result = run_cli(store, "restore", "alpha", "abc", "--target", str(target))
    assert result.returncode == 2
    assert result.stderr.startswith("error:")
    assert "invalid sequence" in result.stderr
    result = run_cli(store, "restore", "alpha", "0", "--target", str(target))
    assert result.returncode == 2
    assert result.stderr.startswith("error:")
    assert "invalid sequence" in result.stderr
    assert not target.exists()
