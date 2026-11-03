# Wayside port fixtures, recorded truth

These fixtures are the cross implementation equivalence proof for the
wayside Rust port. Every store tree here was written by the Python
implementation of wayside at main (merge
537a74f3340356a62d297285adffecb341de8d52), extracted fresh to
~/workspace/chat3-wayside-prep, and frozen byte for byte. All content
is synthetic and was written for these fixtures.

How each fixture was made: the fixture directory holds a produce.py
that drives the Python library, and build_fixtures.py in this folder
ran it and then recorded the truths. The exact production invocation
per fixture was:

    PYTHONPATH=~/workspace/chat3-wayside-prep python3 produce.py <store-dir>

followed, per session and checkpoint, by the Python command line:

    python3 -m wayside --store <store> list <session>
    python3 -m wayside --store <store> show <session> <sequence>
    python3 -m wayside --store <store> restore <session> <sequence> --target <dir>

Recorded truth per fixture, all in the fixture directory:

- store/ is the frozen store tree, including the wayside.lock file
  the implementation creates at the store root (zero bytes).
- inventory.sha256 lists every file under store/ with its SHA-256,
  its size in bytes, and its store relative path, so a reader can
  verify the frozen tree was copied intact.
- expected-list-<session>.txt is the verbatim stdout of the list
  command for that session.
- expected-show-<session>-<sequence>.txt is the verbatim stdout of
  the show command for that checkpoint. The show output is byte
  identical to the stored manifest.json, because both are the same
  sorted, two space indented JSON rendering of the manifest.
- expected-restore/<session>-<sequence>/ is the tree the restore
  command materialized for that checkpoint, payloads plus the
  generated wayside.env and wayside.memory files.

How the Rust suite uses these: the fixtures are copied into the port
repo under tests/fixtures/ unchanged. For every fixture the Rust
suite opens the frozen store and asserts that list and show return
exactly the recorded values, that a restore of every checkpoint
produces a tree byte identical to expected-restore/, and that a new
checkpoint taken by the Rust code inside a copy of the frozen store
takes the next sequence and reads back through show. The created_at
values are part of the frozen manifests and are asserted as recorded
for the frozen checkpoints; checkpoints the Rust code writes itself
carry its own timestamps and are asserted structurally, never against
a recorded clock.

## Fixture 1, single-small-tree

Production calls (produce.py): build a state tree holding notes.txt
with the bytes "synthetic notes" and work/plan.txt with the bytes
"synthetic plan", then one library call with no declared env and the
default empty memory document:

    checkpoint(store, "alpha", state)

The manifest at sessions/alpha/checkpoints/000001/manifest.json:

```json
{
  "created_at": "2026-11-03T06:18:06.087174+00:00",
  "env": {},
  "files": [
    {
      "path": "notes.txt",
      "sha256": "0a78ffa87ac7d819d7b0d2d636970135cc1b0f359334cae527230aaae9b4eb64",
      "size": 15
    },
    {
      "path": "work/plan.txt",
      "sha256": "c75bccf4839e3bcb90317713e0b7f3062a0e099013ca5f784457bd2b9d8c3354",
      "size": 14
    }
  ],
  "format": "wayside-checkpoint/1",
  "memory": {
    "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    "size": 0
  },
  "sequence": 1,
  "session": "alpha"
}
```

Expected list output for session alpha:

```text
1 2026-11-03T06:18:06.087174+00:00
```

## Fixture 2, sequence-of-three

Production calls (produce.py): build a state tree holding a.txt with
the bytes "version one", checkpoint it, rewrite a.txt to "version
two", checkpoint again, rewrite a.txt to "version three", checkpoint
a third time:

    checkpoint(store, "alpha", state)   # sequence 1
    checkpoint(store, "alpha", state)   # sequence 2
    checkpoint(store, "alpha", state)   # sequence 3

Manifests, in sequence order. Sequence 1:

```json
{
  "created_at": "2026-11-03T06:18:08.180638+00:00",
  "env": {},
  "files": [
    {
      "path": "a.txt",
      "sha256": "197c7c60ef8a8470a38d1a9212bdfde9cfe6fd4be910825fe6ac7880ac765d16",
      "size": 11
    }
  ],
  "format": "wayside-checkpoint/1",
  "memory": {
    "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    "size": 0
  },
  "sequence": 1,
  "session": "alpha"
}
```

Sequence 2:

```json
{
  "created_at": "2026-11-03T06:18:08.182806+00:00",
  "env": {},
  "files": [
    {
      "path": "a.txt",
      "sha256": "a67d5f46542514f52d658493fc9405829d2922cf1647574c838b6b86f125477a",
      "size": 11
    }
  ],
  "format": "wayside-checkpoint/1",
  "memory": {
    "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    "size": 0
  },
  "sequence": 2,
  "session": "alpha"
}
```

Sequence 3:

```json
{
  "created_at": "2026-11-03T06:18:08.186171+00:00",
  "env": {},
  "files": [
    {
      "path": "a.txt",
      "sha256": "e8025083e0baf1eaaba983df8337e8ecba0b6a673b9254ce8278bbcccd36117a",
      "size": 13
    }
  ],
  "format": "wayside-checkpoint/1",
  "memory": {
    "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    "size": 0
  },
  "sequence": 3,
  "session": "alpha"
}
```

Expected list output for session alpha:

```text
1 2026-11-03T06:18:08.180638+00:00
2 2026-11-03T06:18:08.182806+00:00
3 2026-11-03T06:18:08.186171+00:00
```

## Fixture 3, forked-session

Production calls (produce.py): build a state tree holding a.txt with
the bytes "alpha bytes" and dir/b.txt with the bytes "beta", then
checkpoint with declared env and a memory document, change a.txt to
"changed later" and checkpoint again with a second memory document,
then fork sequence 1 into a new session:

    checkpoint(store, "alpha", state, env={"K": "V"}, memory=b"alpha memory")
    checkpoint(store, "alpha", state, memory=b"later memory")
    fork(store, "alpha", 1, "beta")

Manifest of alpha sequence 1:

```json
{
  "created_at": "2026-11-03T06:18:12.797709+00:00",
  "env": {
    "K": "V"
  },
  "files": [
    {
      "path": "a.txt",
      "sha256": "561910d04683886f53ccd37a58e2b928094308822c0029470fe8bcc76cfa98c0",
      "size": 11
    },
    {
      "path": "dir/b.txt",
      "sha256": "f44e64e75f3948e9f73f8dfa94721c4ce8cbb4f265c4790c702b2d41cfbf2753",
      "size": 4
    }
  ],
  "format": "wayside-checkpoint/1",
  "memory": {
    "sha256": "487e4c665a33aa0d96283940b4785e3b65274199a7f43bd120c453b3f78fab68",
    "size": 12
  },
  "sequence": 1,
  "session": "alpha"
}
```

Manifest of alpha sequence 2:

```json
{
  "created_at": "2026-11-03T06:18:12.804385+00:00",
  "env": {},
  "files": [
    {
      "path": "a.txt",
      "sha256": "8f8733fe14bec2a9aab0ced028135bf45b9b00e21b661295e970a2eef68d7ec2",
      "size": 13
    },
    {
      "path": "dir/b.txt",
      "sha256": "f44e64e75f3948e9f73f8dfa94721c4ce8cbb4f265c4790c702b2d41cfbf2753",
      "size": 4
    }
  ],
  "format": "wayside-checkpoint/1",
  "memory": {
    "sha256": "792fdff4e67be2b2acf5d8292f75d0eb4b2dec39f321361778bddd8a9c14c18c",
    "size": 12
  },
  "sequence": 2,
  "session": "alpha"
}
```

Manifest of beta sequence 1, the fork, carrying the lineage field
and content byte identical to alpha sequence 1:

```json
{
  "created_at": "2026-11-03T06:18:12.864561+00:00",
  "env": {
    "K": "V"
  },
  "files": [
    {
      "path": "a.txt",
      "sha256": "561910d04683886f53ccd37a58e2b928094308822c0029470fe8bcc76cfa98c0",
      "size": 11
    },
    {
      "path": "dir/b.txt",
      "sha256": "f44e64e75f3948e9f73f8dfa94721c4ce8cbb4f265c4790c702b2d41cfbf2753",
      "size": 4
    }
  ],
  "forked_from": {
    "sequence": 1,
    "session": "alpha"
  },
  "format": "wayside-checkpoint/1",
  "memory": {
    "sha256": "487e4c665a33aa0d96283940b4785e3b65274199a7f43bd120c453b3f78fab68",
    "size": 12
  },
  "sequence": 1,
  "session": "beta"
}
```

Expected list output for session alpha:

```text
1 2026-11-03T06:18:12.797709+00:00
2 2026-11-03T06:18:12.804385+00:00
```

Expected list output for session beta:

```text
1 2026-11-03T06:18:12.864561+00:00
```

## Fixture 4, env-and-memory

Production calls (produce.py): build a state tree holding notes.txt
with the bytes "synthetic notes", then checkpoint with two declared
env pairs and a non empty memory document:

    checkpoint(store, "alpha", state,
               env={"MODE": "synthetic", "LEVEL": "3"},
               memory=b"synthetic memory document")

The manifest at sessions/alpha/checkpoints/000001/manifest.json:

```json
{
  "created_at": "2026-11-03T06:18:16.201337+00:00",
  "env": {
    "LEVEL": "3",
    "MODE": "synthetic"
  },
  "files": [
    {
      "path": "notes.txt",
      "sha256": "0a78ffa87ac7d819d7b0d2d636970135cc1b0f359334cae527230aaae9b4eb64",
      "size": 15
    }
  ],
  "format": "wayside-checkpoint/1",
  "memory": {
    "sha256": "169e8c2fd45cbb4fe5ffa3745b3b4aa56d6bd7f9fa43a5f0b3026de42daf3497",
    "size": 25
  },
  "sequence": 1,
  "session": "alpha"
}
```

Expected list output for session alpha:

```text
1 2026-11-03T06:18:16.201337+00:00
```

The restored wayside.env for this fixture, byte exact, since the env
file format is part of what the port must reproduce:

```text
# Written by wayside. Env recorded by the checkpoint, restored as a file.
LEVEL=3
MODE=synthetic
```

The restored wayside.memory holds the 25 bytes "synthetic memory
document" with no trailing newline.

## Fixture 5, nested-and-empty

Production calls (produce.py): build a state tree holding empty.txt
at zero bytes, deep/other.txt with the bytes "other", and
deep/nested/file.bin holding the 256 bytes 0 through 255 in order,
then one library call:

    checkpoint(store, "alpha", state)

The manifest at sessions/alpha/checkpoints/000001/manifest.json:

```json
{
  "created_at": "2026-11-03T06:18:17.017237+00:00",
  "env": {},
  "files": [
    {
      "path": "deep/nested/file.bin",
      "sha256": "40aff2e9d2d8922e47afd4648e6967497158785fbd1da870e7110266bf944880",
      "size": 256
    },
    {
      "path": "deep/other.txt",
      "sha256": "d9298a10d1b0735837dc4bd85dac641b0f3cef27a47e5d53a54f2f3f5b2fcffa",
      "size": 5
    },
    {
      "path": "empty.txt",
      "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
      "size": 0
    }
  ],
  "format": "wayside-checkpoint/1",
  "memory": {
    "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    "size": 0
  },
  "sequence": 1,
  "session": "alpha"
}
```

Expected list output for session alpha:

```text
1 2026-11-03T06:18:17.017237+00:00
```

One reading note for the port: the empty file and the empty memory
document share the empty input SHA-256 e3b0c442...b7852b855, which is
the published empty vector the Round 1 contract preamble records.
