"""Smoke tests for the Python binding over the wayside library.

The suite drives the full lifecycle through the binding against a
real store the library builds on disk, checkpoints and payloads
and manifests included, and checks the facts each call returns
against the disk and against each other. The cargo suite remains
the gate for the library itself, and this file is the consumer
proof that a Python caller can drive the same lifecycle the
command line drives.
"""

import pytest

import wayside


def write_state(state, plan):
    state.mkdir(parents=True, exist_ok=True)
    (state / "plan.txt").write_text(plan)
    work = state / "work"
    work.mkdir(exist_ok=True)
    (work / "notes.txt").write_text("notes stay the same")


def files_by_path(manifest):
    return {entry["path"]: entry for entry in manifest["files"]}


def test_the_full_lifecycle_through_the_binding(tmp_path):
    store = str(tmp_path / "store")
    state = tmp_path / "state"

    write_state(state, "first plan")
    first = wayside.checkpoint(
        store, "alpha", str(state), env={"MODE": "one"}, memory=b"memory one"
    )
    assert first["format"] == "wayside-checkpoint/1"
    assert first["session"] == "alpha"
    assert first["sequence"] == 1
    assert first["env"] == {"MODE": "one"}
    assert first["memory"]["size"] == 10
    assert first["forked_from"] is None
    entries = files_by_path(first)
    assert entries["plan.txt"]["size"] == 10
    assert entries["work/notes.txt"]["size"] == 19

    write_state(state, "second plan")
    second = wayside.checkpoint(store, "alpha", str(state))
    assert second["sequence"] == 2
    assert second["env"] == {}
    assert second["memory"]["size"] == 0

    rows = wayside.list(store, "alpha")
    assert [row["sequence"] for row in rows] == [1, 2]
    assert rows[0]["created_at"] == first["created_at"]
    assert rows[1]["created_at"] == second["created_at"]

    shown = wayside.show(store, "alpha", 1)
    assert shown == first

    target = tmp_path / "restored"
    restored = wayside.restore(store, "alpha", 1, str(target))
    assert restored == first
    assert (target / "plan.txt").read_text() == "first plan"
    assert (target / "work" / "notes.txt").read_text() == "notes stay the same"
    assert "MODE=one" in (target / "wayside.env").read_text()
    assert (target / "wayside.memory").read_bytes() == b"memory one"

    forked = wayside.fork(store, "alpha", 1, "beta")
    assert forked["session"] == "beta"
    assert forked["sequence"] == 1
    assert forked["forked_from"] == {"session": "alpha", "sequence": 1}
    assert [row["sequence"] for row in wayside.list(store, "beta")] == [1]

    beta_target = tmp_path / "beta-restored"
    wayside.restore(store, "beta", 1, str(beta_target))
    assert (beta_target / "plan.txt").read_text() == "first plan"

    write_state(state, "third plan")
    third = wayside.checkpoint(store, "alpha", str(state))
    assert third["sequence"] == 3

    rewound_target = tmp_path / "rewound"
    rewound = wayside.rewind(store, "alpha", 2, str(rewound_target))
    assert rewound == second
    assert (rewound_target / "plan.txt").read_text() == "second plan"


def test_refusals_raise_with_the_command_line_messages(tmp_path):
    store = str(tmp_path / "store")
    state = tmp_path / "state"
    write_state(state, "first plan")
    wayside.checkpoint(store, "alpha", str(state))

    with pytest.raises(wayside.InvalidNameError) as invalid:
        wayside.checkpoint(store, "Not A Name", str(state))
    assert str(invalid.value) == (
        "invalid session id 'Not A Name', use one path safe component "
        "of lowercase letters and digits and hyphens and underscores, "
        "at most 64 characters long"
    )

    with pytest.raises(wayside.NotFoundError) as missing:
        wayside.show(store, "gamma", 1)
    assert str(missing.value) == (
        f"session gamma holds no checkpoint 1 in the store at {store}"
    )

    with pytest.raises(wayside.RefusalError) as newest:
        wayside.rewind(store, "alpha", 1, str(tmp_path / "target"))
    assert "is the newest checkpoint of session alpha" in str(newest.value)

    manifest = (
        tmp_path
        / "store"
        / "sessions"
        / "alpha"
        / "checkpoints"
        / "000001"
        / "manifest.json"
    )
    manifest.write_bytes(b"this is not a manifest")
    with pytest.raises(wayside.CorruptError) as corrupt:
        wayside.show(store, "alpha", 1)
    assert "is corrupt" in str(corrupt.value)

    with pytest.raises(wayside.WaysideError):
        wayside.show(store, "alpha", 1)


def test_the_database_lifecycle_through_the_binding(tmp_path):
    import hashlib

    store = str(tmp_path / "store")
    state = tmp_path / "state"
    data = state / "data"
    data.mkdir(parents=True)
    main_bytes = b"binding database main bytes"
    wal_bytes = b"binding wal bytes"
    (data / "app.db").write_bytes(main_bytes)
    (data / "app.db-wal").write_bytes(wal_bytes)

    first = wayside.checkpoint(
        store, "alpha", str(state), databases=["data/app.db"]
    )
    assert first["format"] == "wayside-checkpoint/2"
    assert first["files"] == []
    (database,) = first["databases"]
    assert database["path"] == "data/app.db"
    assert database["bundle_sha256"] == hashlib.sha256(
        main_bytes + wal_bytes
    ).hexdigest()
    members = database["members"]
    assert [member["path"] for member in members] == [
        "data/app.db",
        "data/app.db-wal",
    ]
    assert members[0]["sha256"] == hashlib.sha256(main_bytes).hexdigest()
    assert members[0]["size"] == len(main_bytes)
    assert members[1]["sha256"] == hashlib.sha256(wal_bytes).hexdigest()

    verified = wayside.verify(store, "alpha", 1)
    assert verified == first

    second = wayside.checkpoint(store, "alpha", str(state), databases=["data/app.db"])
    compared = wayside.compare(store, "alpha", 1, 2)
    assert compared == {
        "session": "alpha",
        "first": 1,
        "second": 2,
        "databases": [{"path": "data/app.db", "standing": "equal"}],
    }

    (data / "app.db-wal").write_bytes(b"binding wal bytes, changed")
    wayside.checkpoint(store, "alpha", str(state), databases=["data/app.db"])
    changed = wayside.compare(store, "alpha", 2, 3)
    assert changed["databases"] == [
        {"path": "data/app.db", "standing": "different"}
    ]

    plain = wayside.checkpoint(store, "alpha", str(state))
    assert plain["format"] == "wayside-checkpoint/1"
    assert plain["databases"] == []
    one_side = wayside.compare(store, "alpha", 4, 3)
    assert one_side["databases"] == [
        {"path": "data/app.db", "standing": "only_second"}
    ]

    target = tmp_path / "restored"
    wayside.restore(store, "alpha", 1, str(target))
    assert (target / "data" / "app.db").read_bytes() == main_bytes
    assert (target / "data" / "app.db-wal").read_bytes() == wal_bytes

    forked = wayside.fork(store, "alpha", 1, "beta")
    assert forked["format"] == "wayside-checkpoint/2"
    assert forked["databases"] == first["databases"]

    with pytest.raises(wayside.RefusalError) as refused:
        wayside.checkpoint(
            store,
            "alpha",
            str(state),
            databases=["data/app.db", "data/app.db"],
        )
    assert str(refused.value) == "the database path data/app.db is declared twice"


def test_sequence_refuses_booleans(tmp_path):
    store = str(tmp_path / "store")
    state = tmp_path / "state"
    write_state(state, "first plan")
    wayside.checkpoint(store, "alpha", str(state))

    with pytest.raises(TypeError) as shown:
        wayside.show(store, "alpha", True)
    assert "sequence" in str(shown.value)

    with pytest.raises(TypeError) as restored:
        wayside.restore(store, "alpha", False, str(tmp_path / "target"))
    assert "sequence" in str(restored.value)

    assert not (tmp_path / "target").exists()
    assert wayside.show(store, "alpha", 1)["sequence"] == 1


def test_the_directories_lifecycle_through_the_binding(tmp_path):
    store = str(tmp_path / "store")
    state = tmp_path / "state"
    state.mkdir(parents=True)
    (state / "plan.txt").write_text("first plan")
    (state / "spool").mkdir()
    (state / "cache" / "deep").mkdir(parents=True)

    first = wayside.checkpoint(store, "alpha", str(state))
    assert first["format"] == "wayside-checkpoint/2"
    assert first["directories"] == ["cache", "cache/deep", "spool"]
    assert first["databases"] == []

    shown = wayside.show(store, "alpha", 1)
    assert shown == first

    target = tmp_path / "restored"
    restored = wayside.restore(store, "alpha", 1, str(target))
    assert restored == first
    assert (target / "spool").is_dir()
    assert (target / "cache" / "deep").is_dir()
    assert (target / "plan.txt").read_text() == "first plan"

    forked = wayside.fork(store, "alpha", 1, "beta")
    assert forked["directories"] == ["cache", "cache/deep", "spool"]

    plain_state = tmp_path / "plain-state"
    plain_state.mkdir()
    (plain_state / "plan.txt").write_text("plain plan")
    plain = wayside.checkpoint(store, "alpha", str(plain_state))
    assert plain["format"] == "wayside-checkpoint/1"
    assert plain["directories"] == []
