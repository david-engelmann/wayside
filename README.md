# wayside

Checkpointing for agent sessions that snapshots the full working state of files, env, agent memory, and declared database bundles, so any run can resume, fork, or rewind.

Wayside is one local binary backed by a Rust library, with a thin Python binding over the same library. You name a store and the directory holding a session's working state. Wayside records a checkpoint of that state in the store and later restores, forks, or rewinds from the record, verifying every payload against its recorded hash before placing a byte.

[![CI](https://github.com/david-engelmann/wayside/actions/workflows/ci.yml/badge.svg)](https://github.com/david-engelmann/wayside/actions/workflows/ci.yml)

```
{
  "created_at": "2026-11-07T10:45:35.797685+00:00",
  "databases": [
    {
      "bundle_sha256": "cf6f7f678940ee5a4d73c1beb4ae5e0c4c8821844f1a2752197d8e78fbfb353a",
      "members": [
        {
          "path": "data/app.db",
          "sha256": "41c1424c65739e60682daa49e3de58c009dc24007b89230b97f18f94ee6dc232",
          "size": 24
        },
        {
          "path": "data/app.db-wal",
          "sha256": "bc293021a8da2e2b2d61ea087543dfb70c8d00cc7c0919b13d02e3b62673aa2b",
          "size": 14
        }
      ],
      "path": "data/app.db"
    }
  ],
  "env": {
    "MODE": "one"
  },
  "files": [
    {
      "path": "plan.txt",
      "sha256": "206169b704a6c4dbbddbf078201bf005fe6d4986e45a0e4069db9a726833d623",
      "size": 11
    },
    {
      "path": "work/notes.txt",
      "sha256": "0b73e6b17eced328c4389f0849b97b5869e958f144ed27146bc225f5e96218da",
      "size": 20
    }
  ],
  "format": "wayside-checkpoint/2",
  "memory": {
    "sha256": "800a6ffe176e497c690ebc628c5d5476a26e173adbf1206659e2e4c596aa3835",
    "size": 11
  },
  "sequence": 1,
  "session": "demo"
}
```

## Install

Wayside builds from source with Cargo. Clone the repo and install the binary onto your path.

```
git clone https://github.com/david-engelmann/wayside.git
cd wayside
cargo install --path . --bin wayside
```

The install puts the wayside binary in the Cargo bin directory. On a default install that directory is ~/.cargo/bin, and it must stand on your PATH for the wayside command to resolve. The command names the wayside binary alone, because the repo also builds a codepoint_scan helper for its own gate and the install is for the tool itself.

## Quickstart

The quickstart records one checkpoint of a small state directory and restores it. Run the commands in an empty directory, with the wayside binary on your path from the install above.

```
mkdir -p state
printf 'first plan\n' > state/plan.txt
printf 'memory one\n' > memory.md
wayside --store store checkpoint --state state --memory memory.md --env MODE=one demo
wayside --store store restore --target restored demo 1
cat restored/plan.txt
```

```
session demo checkpoint 1
restored session demo checkpoint 1 to restored
first plan
```

The checkpoint is number 1 of the session named demo, and the restore materializes it into the directory named restored. The recorded env pairs land in a file named wayside.env in the target, and the memory document lands in wayside.memory.

## Usage

Every command names the store first, with --store. Run the examples below in a separate empty directory from the quickstart. The runs below share one store and one session named demo, and each output block is the output of the commands above it, captured as printed.

### Record a checkpoint with a database bundle

A database bundle is declared by the path of its main file inside the state directory. The bundle is that file together with the file named by appending -wal and the file named by appending -journal, when each stands. The file named by appending -shm is never part of a bundle and is never recorded. Capture reads and hashes every member, copies the members, and then reads and hashes every member again, and a bundle whose second reading differs from its first refuses the checkpoint and consumes no sequence. The capture takes no lock on the database files and freezes nothing, and a change a writer makes and undoes between the two readings is not detected.

The state directory below carries a plan, a notes file, and a database with its -wal and -shm companions standing.

```
mkdir -p state/work state/data
printf 'first plan\n' > state/plan.txt
printf 'notes stay the same\n' > state/work/notes.txt
printf 'demo database main bytes' > state/data/app.db
printf 'demo wal bytes' > state/data/app.db-wal
printf 'demo shm bytes' > state/data/app.db-shm
printf 'memory one\n' > memory.md
wayside --store store checkpoint --state state --memory memory.md --env MODE=one --database data/app.db demo
```

```
session demo checkpoint 1
```

### Show the manifest

Show prints the manifest of one checkpoint exactly as recorded. A checkpoint that declares at least one database or records at least one empty directory is written in the wayside-checkpoint/2 format, and a checkpoint that declares a database carries the databases field and a checkpoint that records an empty directory carries the directories field, and a checkpoint that does neither is written as wayside-checkpoint/1 with neither field. The manifest below records the bundle digest and each member's hash and size, and the -shm companion standing in the state directory appears nowhere in it.

```
wayside --store store show demo 1
```

```
{
  "created_at": "2026-11-07T10:45:35.797685+00:00",
  "databases": [
    {
      "bundle_sha256": "cf6f7f678940ee5a4d73c1beb4ae5e0c4c8821844f1a2752197d8e78fbfb353a",
      "members": [
        {
          "path": "data/app.db",
          "sha256": "41c1424c65739e60682daa49e3de58c009dc24007b89230b97f18f94ee6dc232",
          "size": 24
        },
        {
          "path": "data/app.db-wal",
          "sha256": "bc293021a8da2e2b2d61ea087543dfb70c8d00cc7c0919b13d02e3b62673aa2b",
          "size": 14
        }
      ],
      "path": "data/app.db"
    }
  ],
  "env": {
    "MODE": "one"
  },
  "files": [
    {
      "path": "plan.txt",
      "sha256": "206169b704a6c4dbbddbf078201bf005fe6d4986e45a0e4069db9a726833d623",
      "size": 11
    },
    {
      "path": "work/notes.txt",
      "sha256": "0b73e6b17eced328c4389f0849b97b5869e958f144ed27146bc225f5e96218da",
      "size": 20
    }
  ],
  "format": "wayside-checkpoint/2",
  "memory": {
    "sha256": "800a6ffe176e497c690ebc628c5d5476a26e173adbf1206659e2e4c596aa3835",
    "size": 11
  },
  "sequence": 1,
  "session": "demo"
}
```

### Verify a checkpoint

Verify reads the manifest and re-reads and re-hashes every recorded payload, the files, the memory document, and every database member. It writes nothing and creates nothing, and a payload whose bytes no longer match the record fails the run with a corrupt checkpoint error naming that payload.

```
wayside --store store verify demo 1
```

```
verified session demo checkpoint 1
```

### Checkpoint again and compare

Compare reports the standing of the databases of two checkpoints from their recorded digests alone, and it reads no payload bytes. Change the plan and the wal, record a second checkpoint, and compare the two.

```
printf 'second plan\n' > state/plan.txt
printf 'demo wal bytes, changed' > state/data/app.db-wal
wayside --store store checkpoint --state state --memory memory.md --env MODE=one --database data/app.db demo
wayside --store store compare demo 1 2
```

```
session demo checkpoint 2
data/app.db different
```

### List the checkpoints

List prints one line per checkpoint, its sequence and the instant it was recorded.

```
wayside --store store list demo
```

```
1 2026-11-07T10:45:35.797685+00:00
2 2026-11-07T10:45:35.829128+00:00
```

### Restore a checkpoint

Restore verifies every recorded hash and size before it places a single byte, and on any mismatch it places nothing and leaves the target as it was found. Restoring the first checkpoint brings back the first plan and the original wal bytes, and the recorded env pairs land in wayside.env beside the restored state files.

```
wayside --store store restore --target restored demo 1
cat restored/plan.txt restored/wayside.env
```

```
restored session demo checkpoint 1 to restored
first plan
# Written by wayside. Env recorded by the checkpoint, restored as a file.
MODE=one
```

### Fork a checkpoint

Fork starts a new session from a named checkpoint. The first checkpoint of the forked session carries the source payloads and a record of where it forked from, naming the source session and sequence.

```
wayside --store store fork demo 1 beta
wayside --store store list beta
```

```
forked session demo checkpoint 1 to session beta checkpoint 1
1 2026-11-07T10:45:36.026155+00:00
```

### Rewind to an earlier checkpoint

Rewind restores a checkpoint earlier than the newest one, and naming the newest checkpoint is refused. With two checkpoints standing, rewinding to the first materializes the first plan again under a new target.

```
wayside --store store rewind --target rewound demo 1
```

```
rewound session demo to checkpoint 1 at rewound
```

### The Python binding

The binding wraps the same library for Python callers. It exposes the same eight operations as the command line, checkpoint, restore, rewind, fork, list, show, verify, and compare, returning plain dictionaries of the manifest, list, and comparison facts, and a refusal raises a subclass of WaysideError carrying the same message text the command line prints. The binding holds no logic of its own, and every rule stays in the Rust library. Build it with maturin from a virtual environment at the repo root.

```
python3 -m venv .venv
.venv/bin/pip install maturin
.venv/bin/maturin develop --manifest-path bindings/Cargo.toml
```

The script below records a checkpoint of a small state directory through the library rather than the binary, lists the session, restores the checkpoint, and prints the restored plan. Run it from an empty directory with the environment above active.

```python
from pathlib import Path

import wayside

state = Path("state")
state.mkdir(exist_ok=True)
(state / "plan.txt").write_text("first plan\n")

first = wayside.checkpoint(
    "store", "alpha", "state", env={"MODE": "one"}, memory=b"memory one"
)
print(first["format"])
print(first["sequence"])
print(first["env"])
print([row["sequence"] for row in wayside.list("store", "alpha")])
wayside.restore("store", "alpha", 1, "restored")
print(Path("restored/plan.txt").read_text(), end="")
```

```
wayside-checkpoint/1
1
{'MODE': 'one'}
[1]
first plan
```

## Status

Every operation works today from the command line and from the Python binding. A checkpoint records the files and the empty directories of a session state directory with declared env pairs and a memory document, and a checkpoint that declares database bundles inside the state directory records each bundle as opaque bytes under a two reading consistency check that refuses a bundle whose second reading differs from its first. Restore, rewind, and fork verify every recorded payload against its recorded hash before placing a byte, verify re-reads a checkpoint without writing anything, and compare reports the standing of the databases of two checkpoints from their recorded digests alone.

The limits are stated in the contract and hold in the code. One bundle's standing members may not sum past 268435456 bytes. A bundle's -shm companion is never recorded. Wayside snapshots no database reached through a connection, no file outside the state directory, and no state that lives only inside a running process. It builds no dump or load tooling, no compression, no encryption, and no cross-machine sync, and it runs no daemon and no watcher. A reader written against the wayside-checkpoint/1 format refuses a /2 manifest as corrupt before any byte is placed.

The contract lives in CONTRACT.md. Obligations S1 to S5 stand as the setup record. Obligations WS-1 to WS-10 state the checkpoint store, and WS-11 to WS-19 state the database bundle slice, and WS-20 to WS-24 state the empty directories amendment.

The continuous integration workflow runs the full suite and the Python binding smoke on every push and pull request, and both checks are required on main.

Closed perfection rounds are 20 of 100. A round counts only when a full cycle of work and audit and review and red team and approval has closed, and seeding and setup are not rounds.

The remote seed was verified on 2026-11-02 against the staged seed, and its commit is a9bdc896322b1787d0719d6a76baaf1d91ef8860, a parentless commit holding exactly README.md and LICENSE and .gitignore.

## License

MIT. See [LICENSE](LICENSE).
