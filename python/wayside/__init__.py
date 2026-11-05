"""wayside, checkpointing for agent sessions with a Rust core.

This package is the thin Python face of the wayside library.
Each function calls the library and returns plain dictionaries of
the facts the command line reports, and a refusal raises a
subclass of WaysideError carrying the same message text the
command line prints. RefusalError covers the plain refusals,
InvalidNameError a session id or sequence in the wrong shape,
NotFoundError a session or checkpoint missing from the store,
CorruptError a manifest or payload that cannot be read as it was
recorded, and TargetError a restore target the store may not
write, mirroring the exit classes the command line reports.
"""

from ._wayside import (
    CorruptError,
    InvalidNameError,
    NotFoundError,
    RefusalError,
    TargetError,
    WaysideError,
    checkpoint,
    compare,
    fork,
    list,
    restore,
    rewind,
    show,
    verify,
)

__all__ = [
    "CorruptError",
    "InvalidNameError",
    "NotFoundError",
    "RefusalError",
    "TargetError",
    "WaysideError",
    "checkpoint",
    "compare",
    "fork",
    "list",
    "restore",
    "rewind",
    "show",
    "verify",
]

__version__ = "0.1.0"
