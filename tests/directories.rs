//! The Round 5 pins: recorded empty directories under
//! WS-20 to WS-24 of CONTRACT.md.
//!
//! Every fixture is synthetic and built inside the test that
//! uses it, in the harness shape of the database pins. The
//! frozen directories store at the bottom was written by this
//! slice's own command line, and it stands beside the frozen
//! stores of the earlier rounds as the fixed record a later
//! reader change must keep reading.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use sha2::{Digest, Sha256};
use wayside::{
    checkpoint, checkpoint_with_databases, fork, list_checkpoints, render_manifest, restore,
    rewind, show, verify_checkpoint, Error,
};

const MEMORY: &[u8] = b"memory one\n";

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let count = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "wayside-dir-test-{}-{tag}-{count}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn make_state(base: &Path, files: &[(&str, &[u8])]) -> PathBuf {
    let state = base.to_path_buf();
    std::fs::create_dir_all(&state).unwrap();
    for (rel, data) in files {
        let path = state.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, data).unwrap();
    }
    state
}

fn make_dirs(state: &Path, dirs: &[&str]) {
    for rel in dirs {
        std::fs::create_dir_all(state.join(rel)).unwrap();
    }
}

fn checkpoint_dir(store: &Path, session: &str, sequence: i64) -> PathBuf {
    store
        .join("sessions")
        .join(session)
        .join("checkpoints")
        .join(format!("{sequence:06}"))
}

fn manifest_path(store: &Path, session: &str, sequence: i64) -> PathBuf {
    checkpoint_dir(store, session, sequence).join("manifest.json")
}

fn expect_error<T>(result: wayside::Result<T>) -> Error {
    match result {
        Ok(_) => panic!("the operation succeeded where the pin requires a refusal"),
        Err(error) => error,
    }
}

fn declare(paths: &[&str]) -> Vec<String> {
    paths.iter().map(|path| path.to_string()).collect()
}

/// A plain checkpoint with no env pairs and the standing
/// memory document.
fn plain_checkpoint(
    store: &Path,
    session: &str,
    state: &Path,
) -> wayside::Result<wayside::Manifest> {
    checkpoint(store, session, state, None, MEMORY)
}

fn db_checkpoint(
    store: &Path,
    session: &str,
    state: &Path,
    databases: &[&str],
) -> wayside::Result<wayside::Manifest> {
    checkpoint_with_databases(store, session, state, None, MEMORY, &declare(databases))
}

/// The relative paths of every directory standing in a tree,
/// the tree root itself excluded.
fn tree_dirs(root: &Path) -> BTreeSet<String> {
    let mut dirs = BTreeSet::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.insert(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
                stack.push(path);
            }
        }
    }
    dirs
}

/// Every file standing in a tree, as relative path to bytes.
fn tree_files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.insert(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                    std::fs::read(&path).unwrap(),
                );
            }
        }
    }
    files
}

fn damage_manifest_json(
    store: &Path,
    session: &str,
    sequence: i64,
    damage: &dyn Fn(&mut serde_json::Value),
) {
    let path = manifest_path(store, session, sequence);
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    damage(&mut value);
    std::fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();
}

struct CliOutput {
    status: i32,
    stdout: String,
    stderr: String,
}

fn run_cli(store: &Path, args: &[&str]) -> CliOutput {
    let output = Command::new(env!("CARGO_BIN_EXE_wayside"))
        .arg("--store")
        .arg(store)
        .args(args)
        .output()
        .unwrap();
    CliOutput {
        status: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

// WS-20 and WS-22, the record and its restore.

#[test]
fn empty_directory_round_trips_through_restore() {
    let base = temp_dir("round-trip");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("plan.txt", b"first plan\n")]);
    make_dirs(&state, &["spool"]);
    let manifest = plain_checkpoint(&store, "alpha", &state).unwrap();
    assert_eq!(manifest.format, "wayside-checkpoint/2");
    assert_eq!(manifest.directories, vec!["spool".to_string()]);
    let stored = std::fs::read_to_string(manifest_path(&store, "alpha", 1)).unwrap();
    assert!(stored.contains("\"directories\""), "{stored}");
    assert!(!stored.contains("\"databases\""), "{stored}");
    let target = base.join("restored");
    restore(&store, "alpha", 1, &target).unwrap();
    assert!(target.join("spool").is_dir());
    assert_eq!(
        std::fs::read(target.join("plan.txt")).unwrap(),
        b"first plan\n"
    );
    assert!(target.join("wayside.env").is_file());
    assert!(target.join("wayside.memory").is_file());
}

#[test]
fn empty_directory_nested_in_an_empty_directory_round_trips() {
    let base = temp_dir("nested");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("plan.txt", b"first plan\n")]);
    make_dirs(&state, &["cache/deep"]);
    let manifest = plain_checkpoint(&store, "alpha", &state).unwrap();
    assert_eq!(
        manifest.directories,
        vec!["cache".to_string(), "cache/deep".to_string()]
    );
    let target = base.join("restored");
    restore(&store, "alpha", 1, &target).unwrap();
    assert!(target.join("cache").is_dir());
    assert!(target.join("cache/deep").is_dir());
}

#[test]
fn directory_free_tree_renders_the_landed_manifest_bytes() {
    let base = temp_dir("landed-bytes");
    let store = base.join("store");
    // The /1 shape: a directory free tree with no declaration
    // renders the stored bytes exactly as the landed writer
    // rendered them, which the render of the manifest read
    // back reproduces byte for byte.
    let state = make_state(&base.join("state"), &[("plan.txt", b"first plan\n")]);
    let manifest = plain_checkpoint(&store, "alpha", &state).unwrap();
    assert_eq!(manifest.format, "wayside-checkpoint/1");
    assert!(manifest.directories.is_empty());
    let stored = std::fs::read(manifest_path(&store, "alpha", 1)).unwrap();
    let read_back = show(&store, "alpha", 1).unwrap();
    assert_eq!(
        stored,
        format!("{}\n", render_manifest(&read_back)).into_bytes()
    );
    // The /2 shape the Round 4 writer landed: declared bundle,
    // no directories, the databases field standing and no
    // directories field anywhere in the stored bytes.
    let state2 = make_state(
        &base.join("state2"),
        &[
            ("plan.txt", b"first plan\n"),
            ("data/app.db", b"synthetic database main bytes"),
        ],
    );
    let manifest2 = db_checkpoint(&store, "beta", &state2, &["data/app.db"]).unwrap();
    assert_eq!(manifest2.format, "wayside-checkpoint/2");
    assert!(manifest2.directories.is_empty());
    let stored2 = std::fs::read(manifest_path(&store, "beta", 1)).unwrap();
    let text2 = String::from_utf8(stored2.clone()).unwrap();
    assert!(text2.contains("\"databases\""), "{text2}");
    assert!(!text2.contains("\"directories\""), "{text2}");
    let read_back2 = show(&store, "beta", 1).unwrap();
    assert_eq!(
        stored2,
        format!("{}\n", render_manifest(&read_back2)).into_bytes()
    );
    // The amended shape: a directories-only /2 manifest renders
    // through the same discipline, directories standing and
    // databases nowhere.
    let state3 = make_state(&base.join("state3"), &[("plan.txt", b"first plan\n")]);
    make_dirs(&state3, &["spool"]);
    plain_checkpoint(&store, "gamma", &state3).unwrap();
    let stored3 = std::fs::read(manifest_path(&store, "gamma", 1)).unwrap();
    let read_back3 = show(&store, "gamma", 1).unwrap();
    assert_eq!(
        stored3,
        format!("{}\n", render_manifest(&read_back3)).into_bytes()
    );
}

#[test]
fn directory_records_stand_sorted_and_prefix_closed() {
    let base = temp_dir("sorted");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("plan.txt", b"first plan\n")]);
    make_dirs(&state, &["z-dir", "a-dir/deep/deeper", "m-dir"]);
    let manifest = plain_checkpoint(&store, "alpha", &state).unwrap();
    assert_eq!(
        manifest.directories,
        vec![
            "a-dir".to_string(),
            "a-dir/deep".to_string(),
            "a-dir/deep/deeper".to_string(),
            "m-dir".to_string(),
            "z-dir".to_string(),
        ]
    );
}

#[test]
fn directory_holding_a_recorded_payload_below_it_is_not_recorded() {
    let base = temp_dir("payload-below");
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[
            ("work/notes.txt", b"notes stay the same\n"),
            ("deep/nest/leaf.txt", b"leaf\n"),
        ],
    );
    make_dirs(&state, &["spool", "solo/chain"]);
    let manifest = plain_checkpoint(&store, "alpha", &state).unwrap();
    assert_eq!(
        manifest.directories,
        vec![
            "solo".to_string(),
            "solo/chain".to_string(),
            "spool".to_string(),
        ]
    );
}

#[test]
fn directory_holding_only_a_declared_databases_shm_companion_is_recorded() {
    // Staging note. The design's literal shape, a directory
    // holding only the -shm companion of a declared database,
    // cannot be staged: the companion's name carries the
    // database's own directory prefix, so the declared main
    // file necessarily stands in that same directory as a
    // member, and declaration validation refuses a main file
    // that does not stand. The two boundary cases that decide
    // the same rule are staged here in one tree instead. The
    // declared bundle's directory stays unrecorded because its
    // members stand in it, and the stray companion of an
    // undeclared database is an ordinary recorded file, so its
    // directory stays unrecorded as well, which together pin
    // that the companion exclusion is the exact companion path
    // of a declared database and nothing wider. The declared
    // companion itself appears nowhere in the record or in the
    // restored tree.
    let base = temp_dir("shm-only");
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[
            ("plan.txt", b"first plan\n"),
            ("data/app.db", b"synthetic database main bytes"),
            ("data/app.db-wal", b"synthetic wal bytes"),
            ("data/app.db-shm", b"synthetic shm bytes"),
            ("solo/app.db-shm", b"stray shm bytes"),
        ],
    );
    let manifest = db_checkpoint(&store, "alpha", &state, &["data/app.db"]).unwrap();
    assert_eq!(manifest.format, "wayside-checkpoint/2");
    assert!(
        manifest.directories.is_empty(),
        "{:?}",
        manifest.directories
    );
    let file_paths: Vec<&str> = manifest
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(file_paths, vec!["plan.txt", "solo/app.db-shm"]);
    let target = base.join("restored");
    restore(&store, "alpha", 1, &target).unwrap();
    assert!(target.join("data/app.db").is_file());
    assert!(target.join("data/app.db-wal").is_file());
    assert!(!target.join("data/app.db-shm").exists());
    assert_eq!(
        std::fs::read(target.join("solo/app.db-shm")).unwrap(),
        b"stray shm bytes"
    );
}

#[test]
fn checkpoint_refuses_a_reserved_top_level_directory_name() {
    let base = temp_dir("reserved-dir");
    let store = base.join("store");
    // The empty shape.
    let state = make_state(&base.join("state"), &[("plan.txt", b"first plan\n")]);
    make_dirs(&state, &["wayside.env"]);
    let error = expect_error(plain_checkpoint(&store, "alpha", &state));
    assert_eq!(error.exit_code(), 1, "{}", error.message());
    assert!(
        error
            .message()
            .contains("top level directory named wayside.env"),
        "{}",
        error.message()
    );
    assert!(!checkpoint_dir(&store, "alpha", 1).exists());
    // The shape holding files.
    let state2 = make_state(
        &base.join("state2"),
        &[("wayside.memory/inner.txt", b"inner\n")],
    );
    let error2 = expect_error(plain_checkpoint(&store, "alpha", &state2));
    assert!(
        error2
            .message()
            .contains("top level directory named wayside.memory"),
        "{}",
        error2.message()
    );
    assert!(!checkpoint_dir(&store, "alpha", 1).exists());
    // The refusals consumed no sequence: the next clean
    // checkpoint of the session takes sequence 1.
    let state3 = make_state(&base.join("state3"), &[("plan.txt", b"first plan\n")]);
    let manifest = plain_checkpoint(&store, "alpha", &state3).unwrap();
    assert_eq!(manifest.sequence, 1);
}

#[test]
fn fork_of_a_checkpoint_with_directories_carries_the_records() {
    let base = temp_dir("fork-dirs");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("plan.txt", b"first plan\n")]);
    make_dirs(&state, &["spool", "cache/deep"]);
    let source = plain_checkpoint(&store, "alpha", &state).unwrap();
    let forked = fork(&store, "alpha", 1, "beta").unwrap();
    assert_eq!(forked.directories, source.directories);
    assert_eq!(forked.format, "wayside-checkpoint/2");
    let target = base.join("restored");
    restore(&store, "beta", 1, &target).unwrap();
    assert!(target.join("spool").is_dir());
    assert!(target.join("cache/deep").is_dir());
}

#[test]
fn rewind_of_a_checkpoint_with_directories_creates_them() {
    let base = temp_dir("rewind-dirs");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("plan.txt", b"first plan\n")]);
    make_dirs(&state, &["spool"]);
    plain_checkpoint(&store, "alpha", &state).unwrap();
    let state2 = make_state(&base.join("state2"), &[("plan.txt", b"second plan\n")]);
    plain_checkpoint(&store, "alpha", &state2).unwrap();
    let target = base.join("restored");
    rewind(&store, "alpha", 1, &target).unwrap();
    assert!(target.join("spool").is_dir());
    assert_eq!(
        std::fs::read(target.join("plan.txt")).unwrap(),
        b"first plan\n"
    );
}

#[test]
fn verify_reports_a_checkpoint_with_directories_clean() {
    let base = temp_dir("verify-dirs");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("plan.txt", b"first plan\n")]);
    make_dirs(&state, &["spool", "cache/deep"]);
    plain_checkpoint(&store, "alpha", &state).unwrap();
    let before_files = tree_files(&store);
    let before_dirs = tree_dirs(&store);
    let verified = verify_checkpoint(&store, "alpha", 1).unwrap();
    assert_eq!(
        verified.directories,
        vec![
            "cache".to_string(),
            "cache/deep".to_string(),
            "spool".to_string()
        ]
    );
    assert_eq!(tree_files(&store), before_files, "verify changed the store");
    assert_eq!(tree_dirs(&store), before_dirs, "verify changed the store");
}

#[test]
fn directory_record_enters_no_digest() {
    let base = temp_dir("no-digest");
    let store = base.join("store");
    let files: &[(&str, &[u8])] = &[
        ("plan.txt", b"first plan\n"),
        ("data/app.db", b"synthetic database main bytes"),
        ("data/app.db-wal", b"synthetic wal bytes"),
    ];
    let bare = make_state(&base.join("bare"), files);
    let with_dirs = make_state(&base.join("with-dirs"), files);
    make_dirs(&with_dirs, &["spool"]);
    let first = db_checkpoint(&store, "alpha", &bare, &["data/app.db"]).unwrap();
    let second = db_checkpoint(&store, "beta", &with_dirs, &["data/app.db"]).unwrap();
    assert!(first.directories.is_empty());
    assert_eq!(second.directories, vec!["spool".to_string()]);
    assert_eq!(
        first.databases[0].bundle_sha256,
        second.databases[0].bundle_sha256
    );
    assert_eq!(first.files, second.files);
    assert_eq!(first.memory, second.memory);
}

#[test]
fn amended_manifest_reads_back_field_by_field() {
    let base = temp_dir("field-by-field");
    let store = base.join("store");
    // The directories-only shape: format /2, directories
    // standing, databases empty, and the rendered text carries
    // the directories field and no databases field.
    let state = make_state(&base.join("state"), &[("plan.txt", b"first plan\n")]);
    make_dirs(&state, &["spool"]);
    plain_checkpoint(&store, "alpha", &state).unwrap();
    let shown = show(&store, "alpha", 1).unwrap();
    assert_eq!(shown.format, "wayside-checkpoint/2");
    assert_eq!(shown.directories, vec!["spool".to_string()]);
    assert!(shown.databases.is_empty());
    assert_eq!(shown.files.len(), 1);
    assert_eq!(shown.files[0].path, "plan.txt");
    let rendered = render_manifest(&shown);
    assert!(rendered.contains("\"directories\": ["), "{rendered}");
    assert!(!rendered.contains("\"databases\""), "{rendered}");
    // The both-fields shape: each conditional field stands, in
    // the sorted key position, databases before directories.
    let state2 = make_state(
        &base.join("state2"),
        &[
            ("plan.txt", b"first plan\n"),
            ("data/app.db", b"synthetic database main bytes"),
        ],
    );
    make_dirs(&state2, &["cache"]);
    db_checkpoint(&store, "beta", &state2, &["data/app.db"]).unwrap();
    let shown2 = show(&store, "beta", 1).unwrap();
    assert_eq!(shown2.directories, vec!["cache".to_string()]);
    assert_eq!(shown2.databases.len(), 1);
    assert_eq!(shown2.databases[0].path, "data/app.db");
    let rendered2 = render_manifest(&shown2);
    let databases_at = rendered2.find("\"databases\"").unwrap();
    let directories_at = rendered2.find("\"directories\"").unwrap();
    assert!(databases_at < directories_at, "{rendered2}");
}

// WS-21, the reader refusals. Each damage pin starts from a
// manifest this slice's writer wrote and damages the record.

fn directories_store(tag: &str) -> (PathBuf, PathBuf) {
    let base = temp_dir(tag);
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[
            ("plan.txt", b"first plan\n"),
            ("work/notes.txt", b"notes stay the same\n"),
        ],
    );
    make_dirs(&state, &["cache", "spool"]);
    plain_checkpoint(&store, "alpha", &state).unwrap();
    (base, store)
}

fn expect_corrupt(store: &Path) {
    let error = expect_error(show(store, "alpha", 1));
    assert_eq!(error.exit_code(), 2, "{}", error.message());
    assert!(
        error.message().contains("is corrupt"),
        "{}",
        error.message()
    );
    let target = store.join("damage-target");
    let restore_error = expect_error(restore(store, "alpha", 1, &target));
    assert_eq!(restore_error.exit_code(), 2, "{}", restore_error.message());
    assert!(!target.exists(), "a refused restore placed something");
}

#[test]
fn directories_empty_array_is_refused() {
    let (_base, store) = directories_store("damage-empty");
    damage_manifest_json(&store, "alpha", 1, &|value| {
        value["directories"] = serde_json::json!([]);
    });
    expect_corrupt(&store);
}

#[test]
fn directories_out_of_order_is_refused() {
    let (_base, store) = directories_store("damage-order");
    damage_manifest_json(&store, "alpha", 1, &|value| {
        value["directories"] = serde_json::json!(["spool", "cache"]);
    });
    expect_corrupt(&store);
}

#[test]
fn directories_repeated_entry_is_refused() {
    let (_base, store) = directories_store("damage-repeat");
    damage_manifest_json(&store, "alpha", 1, &|value| {
        value["directories"] = serde_json::json!(["cache", "cache"]);
    });
    expect_corrupt(&store);
}

#[test]
fn directory_path_also_recorded_as_file_is_refused() {
    let (_base, store) = directories_store("damage-as-file");
    damage_manifest_json(&store, "alpha", 1, &|value| {
        value["directories"] = serde_json::json!(["plan.txt"]);
    });
    expect_corrupt(&store);
}

#[test]
fn directory_above_a_recorded_file_is_refused() {
    let (_base, store) = directories_store("damage-above");
    damage_manifest_json(&store, "alpha", 1, &|value| {
        value["directories"] = serde_json::json!(["work"]);
    });
    expect_corrupt(&store);
}

#[test]
fn directory_path_failing_the_recorded_path_rules_is_refused() {
    let (_base, store) = directories_store("damage-unsafe");
    damage_manifest_json(&store, "alpha", 1, &|value| {
        value["directories"] = serde_json::json!(["../escape"]);
    });
    expect_corrupt(&store);
}

#[test]
fn reserved_top_level_directory_path_is_refused() {
    let (_base, store) = directories_store("damage-reserved");
    damage_manifest_json(&store, "alpha", 1, &|value| {
        value["directories"] = serde_json::json!(["wayside.env"]);
    });
    expect_corrupt(&store);
}

#[test]
fn v1_manifest_carrying_directories_is_refused() {
    let base = temp_dir("damage-v1");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("plan.txt", b"first plan\n")]);
    let manifest = plain_checkpoint(&store, "alpha", &state).unwrap();
    assert_eq!(manifest.format, "wayside-checkpoint/1");
    damage_manifest_json(&store, "alpha", 1, &|value| {
        value["directories"] = serde_json::json!(["spool"]);
    });
    expect_corrupt(&store);
}

// The command line end to end.

#[test]
fn cli_checkpoint_restore_round_trip_with_directories() {
    let base = temp_dir("cli");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("plan.txt", b"first plan\n")]);
    make_dirs(&state, &["spool", "cache/deep"]);
    let memory_path = base.join("memory.md");
    std::fs::write(&memory_path, MEMORY).unwrap();
    let checkpointed = run_cli(
        &store,
        &[
            "checkpoint",
            "--state",
            state.to_str().unwrap(),
            "--memory",
            memory_path.to_str().unwrap(),
            "--env",
            "MODE=one",
            "alpha",
        ],
    );
    assert_eq!(checkpointed.status, 0, "{}", checkpointed.stderr);
    assert_eq!(checkpointed.stdout, "session alpha checkpoint 1\n");
    let shown = run_cli(&store, &["show", "alpha", "1"]);
    assert_eq!(shown.status, 0, "{}", shown.stderr);
    let value: serde_json::Value = serde_json::from_str(&shown.stdout).unwrap();
    assert_eq!(value["format"], "wayside-checkpoint/2");
    assert_eq!(
        value["directories"],
        serde_json::json!(["cache", "cache/deep", "spool"])
    );
    assert!(value.get("databases").is_none(), "{}", shown.stdout);
    let target = base.join("restored");
    let restored = run_cli(
        &store,
        &[
            "restore",
            "--target",
            target.to_str().unwrap(),
            "alpha",
            "1",
        ],
    );
    assert_eq!(restored.status, 0, "{}", restored.stderr);
    assert_eq!(
        restored.stdout,
        format!(
            "restored session alpha checkpoint 1 to {}\n",
            target.display()
        )
    );
    assert_eq!(tree_dirs(&target), tree_dirs(&state));
    assert_eq!(
        std::fs::read(target.join("plan.txt")).unwrap(),
        b"first plan\n"
    );
}

// The frozen directories store.

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let dest = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_tree(&path, &dest);
        } else {
            std::fs::copy(&path, &dest).unwrap();
        }
    }
}

#[test]
fn fixture_directories() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("directories");
    let store = fixture.join("store");
    // The inventory pins every byte of the fixture.
    let inventory = std::fs::read_to_string(fixture.join("inventory.sha256")).unwrap();
    for line in inventory.lines() {
        let (hash, rel) = line.split_once("  ").unwrap();
        let bytes = std::fs::read(fixture.join(rel)).unwrap();
        assert_eq!(sha256_hex(&bytes), hash, "inventory entry {rel}");
    }
    // The recorded list output stands against the live one.
    let expected_list = std::fs::read_to_string(fixture.join("expected-list-alpha.txt")).unwrap();
    let mut live_list = String::new();
    for info in list_checkpoints(&store, "alpha").unwrap() {
        live_list.push_str(&format!("{} {}\n", info.sequence, info.created_at));
    }
    assert_eq!(live_list, expected_list);
    // The three checkpoints read back in their three shapes,
    // and each renders exactly the recorded show output.
    let first = show(&store, "alpha", 1).unwrap();
    assert_eq!(first.format, "wayside-checkpoint/1");
    assert!(first.directories.is_empty());
    let second = show(&store, "alpha", 2).unwrap();
    assert_eq!(second.format, "wayside-checkpoint/2");
    assert_eq!(second.databases.len(), 1);
    assert!(second.directories.is_empty());
    let third = show(&store, "alpha", 3).unwrap();
    assert_eq!(third.format, "wayside-checkpoint/2");
    assert!(third.databases.is_empty());
    assert_eq!(
        third.directories,
        vec!["spool".to_string(), "spool/deep".to_string()]
    );
    for (sequence, manifest) in [(1, &first), (2, &second), (3, &third)] {
        let expected =
            std::fs::read_to_string(fixture.join(format!("expected-show-alpha-{sequence}.txt")))
                .unwrap();
        assert_eq!(format!("{}\n", render_manifest(manifest)), expected);
    }
    // The recorded restore trees stand against live restores.
    // The directories of the alpha-3 tree stand in a listing
    // file beside the tree, because git carries no empty
    // directory and the tree's directories are exactly the
    // empty ones this fixture exists to record.
    let base = temp_dir("fixture-restore");
    for sequence in [2, 3] {
        let target = base.join(format!("restored-{sequence}"));
        restore(&store, "alpha", sequence, &target).unwrap();
        let expected = fixture
            .join("expected-restore")
            .join(format!("alpha-{sequence}"));
        assert_eq!(tree_files(&target), tree_files(&expected));
        if sequence == 3 {
            let listing =
                std::fs::read_to_string(fixture.join("expected-restore-alpha-3-dirs.txt")).unwrap();
            let recorded: BTreeSet<String> = listing.lines().map(str::to_string).collect();
            assert_eq!(tree_dirs(&target), recorded);
        } else {
            assert_eq!(tree_dirs(&target), tree_dirs(&expected));
        }
    }
    // The frozen store grows: a copy takes a fourth checkpoint
    // carrying its own directories.
    let grown = base.join("grown-store");
    copy_tree(&store, &grown);
    let state = make_state(&base.join("state"), &[("plan.txt", b"fourth plan\n")]);
    make_dirs(&state, &["fresh"]);
    let fourth = plain_checkpoint(&grown, "alpha", &state).unwrap();
    assert_eq!(fourth.sequence, 4);
    assert_eq!(fourth.directories, vec!["fresh".to_string()]);
    assert_eq!(list_checkpoints(&grown, "alpha").unwrap().len(), 4);
}
