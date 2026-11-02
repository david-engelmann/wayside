# wayside

Checkpointing for agent sessions that snapshots the full working state of files, env, and agent memory, so any run can resume, fork, or rewind.

## What this is

The flagship runtimes resume runs only inside the same session. Exit mid-run and the next session starts fresh, and there is no cross-session fleet state anywhere, which is how killed lanes strand their work in worktrees. Wayside snapshots the whole working state so a run survives restarts and can be rewound to a known point. Files, database state, and the agent's own context roll back together, not one at a time. The boundary with its siblings is one sentence. Mayfly makes the box, switchyard furnishes it, wayside remembers where everything was when the lights went out.

## Status

Seeded in October 2026. The build has not started. Part of the impatient programming portfolio, in the Fleet Operations arc.

## Licence

MIT. See LICENSE.
