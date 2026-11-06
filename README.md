# wayside

Checkpointing for agent sessions that snapshots the full working state of files, env, and agent memory, so any run can resume, fork, or rewind.

## What this is

The flagship runtimes resume runs only inside the same session. Exit mid-run and the next session starts fresh, and there is no cross-session fleet state anywhere, which is how killed lanes strand their work in worktrees. Wayside snapshots the whole working state so a run survives restarts and can be rewound to a known point. Files and the agent's own context roll back together, not one at a time, and declared database bundles inside the state directory roll back with them, captured under a proof that the bundle sat still while it was copied. The boundary with its siblings is one sentence. Mayfly makes the box, switchyard furnishes it, wayside remembers where everything was when the lights went out.

## Status

The remote seed was verified on 2026-11-02 against the staged seed and its commit is a9bdc896322b1787d0719d6a76baaf1d91ef8860, a parentless commit holding exactly README.md and LICENSE and .gitignore. Setup arrives through a pull request and no work lands on main directly.

The contract lives in CONTRACT.md. The full contract landed with Round 1 slice 1 beside the setup obligations S1 to S5, and it states the checkpoint obligations and the limits of the slice. Wayside now records a checkpoint of a session state directory with declared env pairs and a memory document, snapshots declared database bundles inside the state directory under a two reading consistency proof that refuses a bundle that moves while it is copied, restores and forks and rewinds checkpoints from a store, verifies a recorded checkpoint against its recorded hashes, and compares the recorded databases of two checkpoints from their recorded digests alone.

Closed perfection rounds are 3 of 100. A round counts only when a full cycle of work and audit and review and red team and approval has closed, and seeding and setup are not rounds.

The local gate is cargo test plus the codepoint scan run through cargo, run on the exact final head before any push, with the continuous integration workflow running the same commands as a backstop.

## Python binding

The crate carries a thin Python binding in the bindings directory, for the agent frameworks that call session checkpointing directly from Python. It exposes the same eight operations as the command line, checkpoint and restore and rewind and fork and list and show and verify and compare, with checkpoint taking declared database paths, returning plain dictionaries of the manifest and list and comparison facts, and a refusal raises a subclass of WaysideError carrying the same message text the command line prints. The binding holds no logic of its own and every rule stays in the Rust library, whose suite remains the gate. Build it with maturin develop --manifest-path bindings/Cargo.toml inside a virtual environment, and the smoke test in tests/test_binding_smoke.py drives the full lifecycle through it.

## Licence

MIT. See LICENSE.
