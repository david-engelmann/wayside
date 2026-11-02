"""Command line for wayside, a thin shell over wayside.core.

Every command names the store first. Errors the store raises are
printed as one line on stderr with the exit code the error carries,
and no traceback is ever printed for them.
"""

import argparse
import json
import sys
from pathlib import Path

from . import core
from .core import WaysideError


def _parse_env(pairs):
    env = {}
    for pair in pairs or []:
        if "=" not in pair:
            raise WaysideError(
                f"invalid env pair {pair!r}, use KEY=VALUE"
            )
        key, value = pair.split("=", 1)
        env[key] = value
    return env


def _read_memory(path):
    if path is None:
        return b""
    try:
        return Path(path).read_bytes()
    except OSError as exc:
        raise WaysideError(
            f"the memory document at {path} cannot be read, {exc}"
        ) from exc


def _sequence_arg(text):
    try:
        return int(text)
    except ValueError:
        raise WaysideError(
            f"invalid sequence {text!r}, use a positive integer"
        ) from None


def build_parser():
    parser = argparse.ArgumentParser(
        prog="wayside",
        description=(
            "Checkpoint the working state of an agent session so it "
            "can be resumed, forked, or rewound after the process is gone."
        ),
    )
    parser.add_argument(
        "--store",
        required=True,
        help="the store root directory, created by the first checkpoint",
    )
    sub = parser.add_subparsers(dest="command", required=True)

    p_checkpoint = sub.add_parser(
        "checkpoint", help="record a new checkpoint of a session"
    )
    p_checkpoint.add_argument("session")
    p_checkpoint.add_argument(
        "--state", required=True, help="the state directory to record"
    )
    p_checkpoint.add_argument(
        "--memory",
        help="a file holding the memory document to record, empty when omitted",
    )
    p_checkpoint.add_argument(
        "--env",
        action="append",
        metavar="KEY=VALUE",
        help="an env pair to declare, repeatable",
    )

    p_restore = sub.add_parser(
        "restore", help="materialize one checkpoint into a target directory"
    )
    p_restore.add_argument("session")
    p_restore.add_argument("sequence")
    p_restore.add_argument("--target", required=True)

    p_rewind = sub.add_parser(
        "rewind", help="restore a checkpoint earlier than the newest one"
    )
    p_rewind.add_argument("session")
    p_rewind.add_argument("sequence")
    p_rewind.add_argument("--target", required=True)

    p_fork = sub.add_parser(
        "fork", help="start a new session from a named checkpoint"
    )
    p_fork.add_argument("session")
    p_fork.add_argument("sequence")
    p_fork.add_argument("new_session")

    p_list = sub.add_parser("list", help="list the checkpoints of a session")
    p_list.add_argument("session")

    p_show = sub.add_parser("show", help="print the manifest of one checkpoint")
    p_show.add_argument("session")
    p_show.add_argument("sequence")

    return parser


def main(argv=None):
    args = build_parser().parse_args(argv)
    try:
        if args.command == "checkpoint":
            manifest = core.checkpoint(
                args.store,
                args.session,
                args.state,
                env=_parse_env(args.env),
                memory=_read_memory(args.memory),
            )
            print(
                f"session {manifest['session']} "
                f"checkpoint {manifest['sequence']}"
            )
        elif args.command == "restore":
            sequence = _sequence_arg(args.sequence)
            core.restore(args.store, args.session, sequence, args.target)
            print(
                f"restored session {args.session} "
                f"checkpoint {sequence} to {args.target}"
            )
        elif args.command == "rewind":
            sequence = _sequence_arg(args.sequence)
            core.rewind(args.store, args.session, sequence, args.target)
            print(
                f"rewound session {args.session} "
                f"to checkpoint {sequence} at {args.target}"
            )
        elif args.command == "fork":
            sequence = _sequence_arg(args.sequence)
            manifest = core.fork(
                args.store, args.session, sequence, args.new_session
            )
            print(
                f"forked session {args.session} checkpoint {sequence} "
                f"to session {manifest['session']} checkpoint 1"
            )
        elif args.command == "list":
            for entry in core.list_checkpoints(args.store, args.session):
                print(f"{entry['sequence']} {entry['created_at']}")
        elif args.command == "show":
            sequence = _sequence_arg(args.sequence)
            manifest = core.show(args.store, args.session, sequence)
            print(json.dumps(manifest, indent=2, sort_keys=True))
    except WaysideError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return exc.exit_code
    return 0
