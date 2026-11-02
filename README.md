# wayside

Checkpointing for agent sessions that snapshots the full working state of files, env, and agent memory, so any run can resume, fork, or rewind.

## What this is

The flagship runtimes resume runs only inside the same session. Exit mid-run and the next session starts fresh, and there is no cross-session fleet state anywhere, which is how killed lanes strand their work in worktrees. Wayside snapshots the whole working state so a run survives restarts and can be rewound to a known point. Files, database state, and the agent's own context roll back together, not one at a time. The boundary with its siblings is one sentence. Mayfly makes the box, switchyard furnishes it, wayside remembers where everything was when the lights went out.

## Status

The remote seed was verified on 2026-11-02 against the staged seed and its commit is a9bdc896322b1787d0719d6a76baaf1d91ef8860, a parentless commit holding exactly README.md and LICENSE and .gitignore. Setup arrives through a pull request and no work lands on main directly.

The contract lives in CONTRACT.md. Setup obligations S1 to S5 are the contract state until the full contract lands as the first build act of this repo, and this setup slice claims no feature behaviour for wayside.

Closed perfection rounds are 0 of 100. A round counts only when a full cycle of work and audit and review and red team and approval has closed, and seeding and setup are not rounds.

The local gate is python -m pytest -q plus python scripts/codepoint_scan.py, run on the exact final head before any push, with the continuous integration workflow running the same commands as a backstop.

## Licence

MIT. See LICENSE.
