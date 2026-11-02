"""Wayside, checkpointing for agent sessions.

The public surface is the store operations in wayside.core together
with the typed errors they raise. The command line in wayside.cli is
a thin shell over the same operations.
"""

from .core import (
    FORMAT_NAME,
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

__all__ = [
    "FORMAT_NAME",
    "CorruptCheckpointError",
    "InvalidNameError",
    "NotFoundError",
    "TargetError",
    "WaysideError",
    "checkpoint",
    "fork",
    "list_checkpoints",
    "restore",
    "rewind",
    "show",
]
