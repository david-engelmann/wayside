//! The Round 4 pins: declared database bundles under
//! WS-11 to WS-19 of CONTRACT.md.
//!
//! Every fixture is synthetic and built inside the test that
//! uses it. Bundle changes between the two readings of a
//! capture are staged through the state tree reader seam, so
//! every consistency refusal is deterministic. The frozen
//! database-snapshot store at the bottom was written by this
//! slice's own command line, and it stands beside the five
//! frozen Python stores of the port as the fixed record a
//! later reader change must keep reading.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicUsize, Ordering};

use sha2::{Digest, Sha256};
use wayside::{
    checkpoint, checkpoint_with_databases, checkpoint_with_databases_and_reader,
    compare_checkpoints, fork, list_checkpoints, restore, rewind, show, verify_checkpoint,
    DatabaseStanding, Error, StateTreeReader, BUNDLE_BOUND,
};

const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let count = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "wayside-db-test-{}-{tag}-{count}",
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

fn make_sparse_state(base: &Path, rel: &str, size: u64) -> PathBuf {
    let state = base.to_path_buf();
    std::fs::create_dir_all(&state).unwrap();
    let path = state.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(size).unwrap();
    drop(file);
    state
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

fn hash_tree(root: &Path) -> BTreeMap<String, String> {
    let mut result = BTreeMap::new();
    hash_tree_into(root, root, &mut result);
    result
}

fn hash_tree_into(root: &Path, dir: &Path, result: &mut BTreeMap<String, String>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            hash_tree_into(root, &path, result);
        } else if path.is_file() {
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            result.insert(rel, sha256_hex(&std::fs::read(&path).unwrap()));
        }
    }
}

fn tree_bytes(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut result = BTreeMap::new();
    tree_bytes_into(root, root, &mut result);
    result
}

fn tree_bytes_into(root: &Path, dir: &Path, result: &mut BTreeMap<String, Vec<u8>>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            tree_bytes_into(root, &path, result);
        } else {
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            result.insert(rel, std::fs::read(&path).unwrap());
        }
    }
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(from)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .collect();
    entries.sort();
    for path in entries {
        let dest = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_tree(&path, &dest);
        } else {
            std::fs::copy(&path, &dest).unwrap();
        }
    }
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

/// A database checkpoint with no env pairs and an empty
/// memory document, the shape most pins drive.
fn db_checkpoint(
    store: &Path,
    session: &str,
    state: &Path,
    databases: &[&str],
) -> wayside::Result<wayside::Manifest> {
    checkpoint_with_databases(store, session, state, None, b"", &declare(databases))
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

fn damage_manifest_text(
    store: &Path,
    session: &str,
    sequence: i64,
    damage: &dyn Fn(&str) -> String,
) {
    let path = manifest_path(store, session, sequence);
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, damage(&text)).unwrap();
}

fn flip_first_byte(path: &Path) {
    let mut bytes = std::fs::read(path).unwrap();
    bytes[0] ^= 0xff;
    std::fs::write(path, &bytes).unwrap();
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

// WS-11, the declaration.

#[test]
fn declared_database_is_recorded_with_its_bundle() {
    let base = temp_dir("declared");
    let store = base.join("store");
    let main_bytes: &[u8] = b"synthetic database main bytes";
    let wal_bytes: &[u8] = b"synthetic wal bytes";
    let state = make_state(
        &base.join("state"),
        &[
            ("notes.txt", b"synthetic notes"),
            ("data/app.db", main_bytes),
            ("data/app.db-wal", wal_bytes),
        ],
    );
    let manifest = db_checkpoint(&store, "alpha", &state, &["data/app.db"]).unwrap();
    assert_eq!(manifest.format, "wayside-checkpoint/2");
    let file_paths: Vec<&str> = manifest
        .files
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert_eq!(file_paths, ["notes.txt"]);
    assert_eq!(manifest.databases.len(), 1);
    let database = &manifest.databases[0];
    assert_eq!(database.path, "data/app.db");
    let member_paths: Vec<&str> = database
        .members
        .iter()
        .map(|member| member.path.as_str())
        .collect();
    assert_eq!(member_paths, ["data/app.db", "data/app.db-wal"]);
    assert_eq!(database.members[0].sha256, sha256_hex(main_bytes));
    assert_eq!(database.members[0].size, main_bytes.len() as i128);
    assert_eq!(database.members[1].sha256, sha256_hex(wal_bytes));
    assert_eq!(database.members[1].size, wal_bytes.len() as i128);
    let mut joined = main_bytes.to_vec();
    joined.extend_from_slice(wal_bytes);
    assert_eq!(database.bundle_sha256, sha256_hex(&joined));
    // The payload bytes stand once, in the files area at the
    // member's relative path.
    let stored_wal = std::fs::read(
        checkpoint_dir(&store, "alpha", 1)
            .join("files")
            .join("data/app.db-wal"),
    )
    .unwrap();
    assert_eq!(stored_wal, wal_bytes);
    // And the manifest reads back identical to the one returned.
    assert_eq!(show(&store, "alpha", 1).unwrap(), manifest);
}

#[test]
fn declaring_the_same_path_twice_is_refused() {
    let base = temp_dir("declared-twice");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("app.db", b"synthetic database")]);
    db_checkpoint(&store, "alpha", &state, &[]).unwrap();
    let error = expect_error(db_checkpoint(
        &store,
        "alpha",
        &state,
        &["app.db", "app.db"],
    ));
    assert!(matches!(error, Error::Wayside(_)), "{error:?}");
    assert!(
        error.message().contains("app.db is declared twice"),
        "{}",
        error.message()
    );
    // Nothing was published and the sequence stands where it
    // stood, so the next sound checkpoint takes sequence 2.
    let listed = list_checkpoints(&store, "alpha").unwrap();
    assert_eq!(listed.len(), 1);
    let next = db_checkpoint(&store, "alpha", &state, &["app.db"]).unwrap();
    assert_eq!(next.sequence, 2);
}

#[test]
fn declaring_a_sidecar_of_a_declared_path_is_refused() {
    let base = temp_dir("declared-sidecar");
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[
            ("data/app.db", b"synthetic database"),
            ("data/app.db-wal", b"synthetic wal"),
        ],
    );
    let error = expect_error(db_checkpoint(
        &store,
        "alpha",
        &state,
        &["data/app.db-wal", "data/app.db"],
    ));
    assert!(matches!(error, Error::Wayside(_)), "{error:?}");
    assert!(
        error.message().contains("data/app.db-wal")
            && error
                .message()
                .contains("sidecar of the declared database path data/app.db"),
        "{}",
        error.message()
    );
    assert_eq!(
        expect_error(list_checkpoints(&store, "alpha")).message(),
        format!("session alpha is not in the store at {}", store.display()).as_str()
    );
}

#[test]
fn declaring_a_reserved_name_is_refused() {
    let base = temp_dir("declared-reserved");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("app.db", b"synthetic database")]);
    let error = expect_error(db_checkpoint(&store, "alpha", &state, &["wayside.env"]));
    assert!(matches!(error, Error::Wayside(_)), "{error:?}");
    assert!(
        error.message().contains("wayside.env") && error.message().contains("restore writes"),
        "{}",
        error.message()
    );
}

#[test]
fn declaring_a_path_with_a_parent_reference_is_refused() {
    let base = temp_dir("declared-parent");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("app.db", b"synthetic database")]);
    let error = expect_error(db_checkpoint(&store, "alpha", &state, &["../outside.db"]));
    assert!(matches!(error, Error::Wayside(_)), "{error:?}");
    assert!(
        error.message().contains("not a safe relative path"),
        "{}",
        error.message()
    );
    let error = expect_error(db_checkpoint(&store, "alpha", &state, &["/abs.db"]));
    assert!(matches!(error, Error::Wayside(_)), "{error:?}");
    let error = expect_error(db_checkpoint(&store, "alpha", &state, &["a\\b.db"]));
    assert!(matches!(error, Error::Wayside(_)), "{error:?}");
}

#[test]
fn declaring_a_path_that_names_nothing_standing_is_refused() {
    let base = temp_dir("declared-missing");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("app.db", b"synthetic database")]);
    let error = expect_error(db_checkpoint(&store, "alpha", &state, &["data/missing.db"]));
    assert!(matches!(error, Error::Wayside(_)), "{error:?}");
    assert!(
        error.message().contains("data/missing.db")
            && error
                .message()
                .contains("does not name a regular file standing"),
        "{}",
        error.message()
    );
}

#[test]
fn declaring_a_directory_as_a_database_is_refused() {
    let base = temp_dir("declared-directory");
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[("data/inner.txt", b"synthetic inner")],
    );
    let error = expect_error(db_checkpoint(&store, "alpha", &state, &["data"]));
    assert!(matches!(error, Error::Wayside(_)), "{error:?}");
    assert!(
        error.message().contains("data")
            && error
                .message()
                .contains("does not name a regular file standing"),
        "{}",
        error.message()
    );
}

#[cfg(unix)]
#[test]
fn declaring_a_link_as_a_database_is_refused() {
    let base = temp_dir("declared-link");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("real.db", b"synthetic database")]);
    std::os::unix::fs::symlink(state.join("real.db"), state.join("linked.db")).unwrap();
    let error = expect_error(db_checkpoint(&store, "alpha", &state, &["linked.db"]));
    assert!(matches!(error, Error::Wayside(_)), "{error:?}");
    assert!(
        error.message().contains("symbolic link at linked.db"),
        "{}",
        error.message()
    );
}

// WS-12, the bundle and the opacity of the bytes.

#[test]
fn bundle_with_both_companions_records_three_members() {
    let base = temp_dir("bundle-both");
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[
            ("app.db", b"main"),
            ("app.db-wal", b"wal"),
            ("app.db-journal", b"journal"),
        ],
    );
    let manifest = db_checkpoint(&store, "alpha", &state, &["app.db"]).unwrap();
    let member_paths: Vec<&str> = manifest.databases[0]
        .members
        .iter()
        .map(|member| member.path.as_str())
        .collect();
    assert_eq!(member_paths, ["app.db", "app.db-wal", "app.db-journal"]);
    assert!(manifest.files.is_empty());
}

#[test]
fn bundle_without_companions_records_the_main_file_alone() {
    let base = temp_dir("bundle-alone");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("app.db", b"main")]);
    let manifest = db_checkpoint(&store, "alpha", &state, &["app.db"]).unwrap();
    assert_eq!(manifest.databases[0].members.len(), 1);
    assert_eq!(manifest.databases[0].members[0].path, "app.db");
    assert_eq!(manifest.databases[0].bundle_sha256, sha256_hex(b"main"));
}

#[test]
fn bundle_with_each_companion_alone() {
    let base = temp_dir("bundle-each");
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[
            ("one.db", b"one main"),
            ("one.db-wal", b"one wal"),
            ("two.db", b"two main"),
            ("two.db-journal", b"two journal"),
        ],
    );
    // Declared out of order; the record stands in path order.
    let manifest = db_checkpoint(&store, "alpha", &state, &["two.db", "one.db"]).unwrap();
    assert_eq!(manifest.databases.len(), 2);
    assert_eq!(manifest.databases[0].path, "one.db");
    let one_paths: Vec<&str> = manifest.databases[0]
        .members
        .iter()
        .map(|member| member.path.as_str())
        .collect();
    assert_eq!(one_paths, ["one.db", "one.db-wal"]);
    assert_eq!(manifest.databases[1].path, "two.db");
    let two_paths: Vec<&str> = manifest.databases[1]
        .members
        .iter()
        .map(|member| member.path.as_str())
        .collect();
    assert_eq!(two_paths, ["two.db", "two.db-journal"]);
}

#[test]
fn journal_only_bundle_reads_back_through_show_verify_and_restore() {
    let base = temp_dir("bundle-journal-only");
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[
            ("app.db", b"main bytes"),
            ("app.db-journal", b"journal bytes"),
        ],
    );
    let manifest = db_checkpoint(&store, "alpha", &state, &["app.db"]).unwrap();
    let member_paths: Vec<&str> = manifest.databases[0]
        .members
        .iter()
        .map(|member| member.path.as_str())
        .collect();
    assert_eq!(member_paths, ["app.db", "app.db-journal"]);
    assert_eq!(show(&store, "alpha", 1).unwrap(), manifest);
    verify_checkpoint(&store, "alpha", 1).unwrap();
    let target = base.join("target");
    restore(&store, "alpha", 1, &target).unwrap();
    assert_eq!(std::fs::read(target.join("app.db")).unwrap(), b"main bytes");
    assert_eq!(
        std::fs::read(target.join("app.db-journal")).unwrap(),
        b"journal bytes"
    );
    assert!(!target.join("app.db-wal").exists());
}

#[test]
fn shm_companion_appears_nowhere_in_the_recording() {
    let base = temp_dir("bundle-shm");
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[
            ("app.db", b"main"),
            ("app.db-wal", b"wal"),
            ("app.db-shm", b"shared memory index bytes"),
            ("plain.txt", b"plain"),
        ],
    );
    let manifest = db_checkpoint(&store, "alpha", &state, &["app.db"]).unwrap();
    let recorded: Vec<&str> = manifest
        .files
        .iter()
        .map(|entry| entry.path.as_str())
        .chain(
            manifest.databases[0]
                .members
                .iter()
                .map(|member| member.path.as_str()),
        )
        .collect();
    assert!(!recorded.contains(&"app.db-shm"), "{recorded:?}");
    let stored_text = std::fs::read_to_string(manifest_path(&store, "alpha", 1)).unwrap();
    assert!(!stored_text.contains("app.db-shm"), "{stored_text}");
    let target = base.join("target");
    restore(&store, "alpha", 1, &target).unwrap();
    assert!(!target.join("app.db-shm").exists());
    assert_eq!(std::fs::read(target.join("app.db")).unwrap(), b"main");
    assert_eq!(std::fs::read(target.join("app.db-wal")).unwrap(), b"wal");
    assert_eq!(std::fs::read(target.join("plain.txt")).unwrap(), b"plain");
}

#[test]
fn sidecar_of_an_undeclared_main_file_rides_as_an_ordinary_file() {
    let base = temp_dir("undeclared-sidecar");
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[("other.db-wal", b"orphan wal bytes")],
    );
    let manifest = checkpoint(&store, "alpha", &state, None, b"").unwrap();
    assert_eq!(manifest.format, "wayside-checkpoint/1");
    let file_paths: Vec<&str> = manifest
        .files
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert_eq!(file_paths, ["other.db-wal"]);
    let target = base.join("target");
    restore(&store, "alpha", 1, &target).unwrap();
    assert_eq!(
        std::fs::read(target.join("other.db-wal")).unwrap(),
        b"orphan wal bytes"
    );
}

#[test]
fn arbitrary_bytes_are_captured_and_restored_byte_for_byte() {
    let base = temp_dir("opaque-pattern");
    let store = base.join("store");
    let pattern: Vec<u8> = (0..4096).map(|index| (index % 251) as u8).collect();
    let state = make_state(&base.join("state"), &[("blob.db", &pattern)]);
    let manifest = db_checkpoint(&store, "alpha", &state, &["blob.db"]).unwrap();
    assert_eq!(
        manifest.databases[0].members[0].sha256,
        sha256_hex(&pattern)
    );
    let target = base.join("target");
    restore(&store, "alpha", 1, &target).unwrap();
    assert_eq!(std::fs::read(target.join("blob.db")).unwrap(), pattern);
}

#[test]
fn database_header_bytes_are_captured_and_restored_byte_for_byte() {
    let base = temp_dir("opaque-header");
    let store = base.join("store");
    let mut bytes = b"SQLite format 3\0".to_vec();
    bytes.extend((0..1024).map(|index| (index % 253) as u8));
    let state = make_state(&base.join("state"), &[("real.db", &bytes)]);
    let manifest = db_checkpoint(&store, "alpha", &state, &["real.db"]).unwrap();
    assert_eq!(manifest.databases[0].members[0].sha256, sha256_hex(&bytes));
    let target = base.join("target");
    restore(&store, "alpha", 1, &target).unwrap();
    assert_eq!(std::fs::read(target.join("real.db")).unwrap(), bytes);
}

// WS-13, the consistency proof at capture.

#[test]
fn still_bundle_publishes_with_manifest_hashes_of_the_source_bytes() {
    let base = temp_dir("still-bundle");
    let store = base.join("store");
    let main_bytes: &[u8] = b"still main bytes";
    let wal_bytes: &[u8] = b"still wal bytes";
    let journal_bytes: &[u8] = b"still journal bytes";
    let state = make_state(
        &base.join("state"),
        &[
            ("app.db", main_bytes),
            ("app.db-wal", wal_bytes),
            ("app.db-journal", journal_bytes),
        ],
    );
    let manifest = db_checkpoint(&store, "alpha", &state, &["app.db"]).unwrap();
    let database = &manifest.databases[0];
    for (member, bytes) in database
        .members
        .iter()
        .zip([main_bytes, wal_bytes, journal_bytes])
    {
        assert_eq!(member.sha256, sha256_hex(bytes));
        assert_eq!(member.size, bytes.len() as i128);
        let stored = std::fs::read(
            checkpoint_dir(&store, "alpha", 1)
                .join("files")
                .join(&member.path),
        )
        .unwrap();
        assert_eq!(stored, bytes);
    }
    let mut joined = main_bytes.to_vec();
    joined.extend_from_slice(wal_bytes);
    joined.extend_from_slice(journal_bytes);
    assert_eq!(database.bundle_sha256, sha256_hex(&joined));
}

fn changed_refusal(store: &Path, state: &Path, reader: StateTreeReader) -> Error {
    expect_error(checkpoint_with_databases_and_reader(
        store,
        "alpha",
        state,
        None,
        b"",
        &declare(&["data/app.db"]),
        &reader,
    ))
}

fn staged_state(base: &Path) -> PathBuf {
    make_state(
        &base.join("state"),
        &[
            ("data/app.db", b"staged main bytes"),
            ("data/app.db-wal", b"staged wal bytes"),
        ],
    )
}

#[test]
fn member_bytes_changing_between_readings_is_refused() {
    let base = temp_dir("changed-bytes");
    let store = base.join("store");
    let state = staged_state(&base);
    let reader = StateTreeReader::altering_bytes_on_second_read(
        &state.join("data/app.db-wal"),
        b"changed wal bytes".to_vec(),
    );
    let error = changed_refusal(&store, &state, reader);
    assert!(matches!(error, Error::Wayside(_)), "{error:?}");
    assert!(
        error.message().contains("data/app.db")
            && error.message().contains("changed during capture"),
        "{}",
        error.message()
    );
    // Nothing was published and no sequence was consumed.
    let next = db_checkpoint(&store, "alpha", &state, &["data/app.db"]).unwrap();
    assert_eq!(next.sequence, 1);
}

#[test]
fn member_size_changing_between_readings_is_refused() {
    let base = temp_dir("changed-size");
    let store = base.join("store");
    let state = staged_state(&base);
    let reader = StateTreeReader::altering_bytes_on_second_read(
        &state.join("data/app.db"),
        b"staged main bytes, grown by a tail".to_vec(),
    );
    let error = changed_refusal(&store, &state, reader);
    assert!(matches!(error, Error::Wayside(_)), "{error:?}");
    assert!(
        error.message().contains("changed during capture"),
        "{}",
        error.message()
    );
    let next = db_checkpoint(&store, "alpha", &state, &["data/app.db"]).unwrap();
    assert_eq!(next.sequence, 1);
}

#[test]
fn member_vanishing_between_readings_is_refused() {
    let base = temp_dir("changed-vanish");
    let store = base.join("store");
    let state = staged_state(&base);
    let reader = StateTreeReader::dropping_on_second_read(&state.join("data/app.db"));
    let error = changed_refusal(&store, &state, reader);
    assert!(matches!(error, Error::Wayside(_)), "{error:?}");
    assert!(
        error.message().contains("data/app.db")
            && error.message().contains("changed during capture"),
        "{}",
        error.message()
    );
    let next = db_checkpoint(&store, "alpha", &state, &["data/app.db"]).unwrap();
    assert_eq!(next.sequence, 1);
}

#[test]
fn sidecar_appearing_between_readings_is_refused() {
    let base = temp_dir("changed-appear");
    let store = base.join("store");
    let state = staged_state(&base);
    let reader = StateTreeReader::appearing_on_second_probe(&state.join("data/app.db-journal"), 12);
    let error = changed_refusal(&store, &state, reader);
    assert!(matches!(error, Error::Wayside(_)), "{error:?}");
    assert!(
        error.message().contains("data/app.db")
            && error.message().contains("changed during capture"),
        "{}",
        error.message()
    );
    let next = db_checkpoint(&store, "alpha", &state, &["data/app.db"]).unwrap();
    assert_eq!(next.sequence, 1);
}

#[test]
fn sidecar_disappearing_between_readings_is_refused() {
    let base = temp_dir("changed-disappear");
    let store = base.join("store");
    let state = staged_state(&base);
    let reader = StateTreeReader::vanishing_on_second_probe(&state.join("data/app.db-wal"));
    let error = changed_refusal(&store, &state, reader);
    assert!(matches!(error, Error::Wayside(_)), "{error:?}");
    assert!(
        error.message().contains("data/app.db")
            && error.message().contains("changed during capture"),
        "{}",
        error.message()
    );
    let next = db_checkpoint(&store, "alpha", &state, &["data/app.db"]).unwrap();
    assert_eq!(next.sequence, 1);
}

/// The worker mode of the parallel database writers pin.
/// Spawned by the pin below as a separate process with the
/// WAYSIDE_DB_WORKER_ variables set; run by the suite without
/// them it returns at once and asserts nothing.
#[test]
fn database_parallel_worker_entry() {
    let Ok(store) = std::env::var("WAYSIDE_DB_WORKER_STORE") else {
        return;
    };
    let session = std::env::var("WAYSIDE_DB_WORKER_SESSION").unwrap();
    let state = std::env::var("WAYSIDE_DB_WORKER_STATE").unwrap();
    let count: usize = std::env::var("WAYSIDE_DB_WORKER_COUNT")
        .unwrap()
        .parse()
        .unwrap();
    for _ in 0..count {
        db_checkpoint(
            Path::new(&store),
            &session,
            Path::new(&state),
            &["data/app.db"],
        )
        .unwrap();
    }
}

#[test]
fn parallel_writers_declaring_the_same_bundle_keep_the_sequence_gapless() {
    let base = temp_dir("parallel-databases");
    let store = base.join("store");
    let workers = 8;
    let each = 3;
    let exe = std::env::current_exe().unwrap();
    let mut children: Vec<Child> = Vec::new();
    for index in 0..workers {
        let state = make_state(
            &base.join(format!("state{index}")),
            &[
                ("data/app.db", b"parallel main bytes"),
                ("data/app.db-wal", b"parallel wal bytes"),
            ],
        );
        let child = Command::new(&exe)
            .arg("database_parallel_worker_entry")
            .arg("--exact")
            .arg("--nocapture")
            .env("WAYSIDE_DB_WORKER_STORE", &store)
            .env("WAYSIDE_DB_WORKER_SESSION", "alpha")
            .env("WAYSIDE_DB_WORKER_STATE", &state)
            .env("WAYSIDE_DB_WORKER_COUNT", each.to_string())
            .spawn()
            .unwrap();
        children.push(child);
    }
    for mut child in children {
        let status = child.wait().unwrap();
        assert!(status.success(), "a writer process failed: {status}");
    }
    let listed: Vec<i64> = list_checkpoints(&store, "alpha")
        .unwrap()
        .iter()
        .map(|entry| entry.sequence)
        .collect();
    let expected: Vec<i64> = (1..=(workers * each) as i64).collect();
    assert_eq!(listed, expected);
    for sequence in expected {
        let manifest = show(&store, "alpha", sequence).unwrap();
        assert_eq!(manifest.format, "wayside-checkpoint/2");
        assert_eq!(manifest.databases.len(), 1);
    }
}

// WS-14, the bundle size bound.

#[test]
fn bundle_at_exactly_the_bound_is_captured() {
    let base = temp_dir("bound-exact");
    let store = base.join("store");
    let state = make_sparse_state(&base.join("state"), "app.db", BUNDLE_BOUND);
    let manifest = db_checkpoint(&store, "alpha", &state, &["app.db"]).unwrap();
    assert_eq!(manifest.databases[0].members[0].size, BUNDLE_BOUND as i128);
    // The recorded hash is the hash of the sparse source
    // bytes, computed here by streaming the file in chunks.
    let mut hasher = Sha256::new();
    let mut source = std::fs::File::open(state.join("app.db")).unwrap();
    let mut buffer = [0u8; 65536];
    loop {
        let read = std::io::Read::read(&mut source, &mut buffer).unwrap();
        if read == 0 {
            break;
        }
        Digest::update(&mut hasher, &buffer[..read]);
    }
    let expected: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(manifest.databases[0].members[0].sha256, expected);
    verify_checkpoint(&store, "alpha", 1).unwrap();
}

#[test]
fn bundle_one_byte_past_the_bound_is_refused_at_validation() {
    let base = temp_dir("bound-past");
    let store = base.join("store");
    let state = make_sparse_state(&base.join("state"), "app.db", BUNDLE_BOUND + 1);
    let error = expect_error(db_checkpoint(&store, "alpha", &state, &["app.db"]));
    assert!(matches!(error, Error::Wayside(_)), "{error:?}");
    assert!(error.message().contains("268435456"), "{}", error.message());
    assert!(matches!(
        expect_error(list_checkpoints(&store, "alpha")),
        Error::NotFound(_)
    ));
}

#[test]
fn bundle_growing_past_the_bound_during_the_copy_is_refused() {
    let base = temp_dir("bound-growth");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("app.db", b"small main bytes")]);
    let reader = StateTreeReader::serving_bytes(
        &state.join("app.db"),
        vec![7u8; (BUNDLE_BOUND + 1) as usize],
    );
    let error = expect_error(checkpoint_with_databases_and_reader(
        &store,
        "alpha",
        &state,
        None,
        b"",
        &declare(&["app.db"]),
        &reader,
    ));
    assert!(matches!(error, Error::Wayside(_)), "{error:?}");
    assert!(error.message().contains("268435456"), "{}", error.message());
    assert!(matches!(
        expect_error(list_checkpoints(&store, "alpha")),
        Error::NotFound(_)
    ));
}

// WS-15, the successor format.

#[test]
fn checkpoint_without_databases_is_written_as_format_1() {
    let base = temp_dir("format-plain");
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[("app.db", b"unrecorded as a database")],
    );
    let manifest = checkpoint(&store, "alpha", &state, None, b"").unwrap();
    assert_eq!(manifest.format, "wayside-checkpoint/1");
    assert!(manifest.databases.is_empty());
    let text = std::fs::read_to_string(manifest_path(&store, "alpha", 1)).unwrap();
    assert!(!text.contains("databases"), "{text}");
}

#[test]
fn v2_manifest_reads_back_field_by_field() {
    let base = temp_dir("format-fields");
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[
            ("data/app.db", b"field main"),
            ("data/app.db-journal", b"field journal"),
        ],
    );
    db_checkpoint(&store, "alpha", &state, &["data/app.db"]).unwrap();
    let text = std::fs::read_to_string(manifest_path(&store, "alpha", 1)).unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    let mut top_keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(|key| key.as_str())
        .collect();
    top_keys.sort();
    assert_eq!(
        top_keys,
        [
            "created_at",
            "databases",
            "env",
            "files",
            "format",
            "memory",
            "sequence",
            "session"
        ]
    );
    assert_eq!(value["format"], "wayside-checkpoint/2");
    let database = &value["databases"][0];
    let mut database_keys: Vec<&str> = database
        .as_object()
        .unwrap()
        .keys()
        .map(|key| key.as_str())
        .collect();
    database_keys.sort();
    assert_eq!(database_keys, ["bundle_sha256", "members", "path"]);
    assert_eq!(database["path"], "data/app.db");
    let members = database["members"].as_array().unwrap();
    assert_eq!(members.len(), 2);
    for member in members {
        let mut member_keys: Vec<&str> = member
            .as_object()
            .unwrap()
            .keys()
            .map(|key| key.as_str())
            .collect();
        member_keys.sort();
        assert_eq!(member_keys, ["path", "sha256", "size"]);
    }
    assert_eq!(members[0]["path"], "data/app.db");
    assert_eq!(members[1]["path"], "data/app.db-journal");
}

#[test]
fn v2_manifest_bytes_match_the_recorded_layout() {
    let base = temp_dir("format-bytes");
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[("app.db", b"db-bytes"), ("app.db-wal", b"wal-bytes")],
    );
    let manifest = db_checkpoint(&store, "alpha", &state, &["app.db"]).unwrap();
    let main_sha = sha256_hex(b"db-bytes");
    let wal_sha = sha256_hex(b"wal-bytes");
    let bundle_sha = sha256_hex(b"db-byteswal-bytes");
    let expected = format!(
        "{{\n  \"created_at\": \"{}\",\n  \"databases\": [\n    {{\n      \"bundle_sha256\": \"{bundle_sha}\",\n      \"members\": [\n        {{\n          \"path\": \"app.db\",\n          \"sha256\": \"{main_sha}\",\n          \"size\": 8\n        }},\n        {{\n          \"path\": \"app.db-wal\",\n          \"sha256\": \"{wal_sha}\",\n          \"size\": 9\n        }}\n      ],\n      \"path\": \"app.db\"\n    }}\n  ],\n  \"env\": {{}},\n  \"files\": [],\n  \"format\": \"wayside-checkpoint/2\",\n  \"memory\": {{\n    \"sha256\": \"{EMPTY_SHA256}\",\n    \"size\": 0\n  }},\n  \"sequence\": 1,\n  \"session\": \"alpha\"\n}}\n",
        manifest.created_at
    );
    let stored = std::fs::read_to_string(manifest_path(&store, "alpha", 1)).unwrap();
    assert_eq!(stored, expected);
    assert_eq!(format!("{}\n", wayside::render_manifest(&manifest)), stored);
}

/// A /2 store over a two member bundle, the starting point of
/// the damage battery.
fn damaged_v2_store(tag: &str) -> (PathBuf, PathBuf) {
    let base = temp_dir(tag);
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[
            ("notes.txt", b"damage notes"),
            ("data/app.db", b"damage main"),
            ("data/app.db-wal", b"damage wal"),
            ("data/app.db-journal", b"damage journal"),
        ],
    );
    db_checkpoint(&store, "alpha", &state, &["data/app.db"]).unwrap();
    (base, store)
}

fn expect_corrupt_show(store: &Path) -> Error {
    let error = expect_error(show(store, "alpha", 1));
    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
    error
}

#[test]
fn v2_manifest_with_a_repeated_key_is_refused() {
    let (_base, store) = damaged_v2_store("damage-repeat");
    damage_manifest_text(&store, "alpha", 1, &|text| {
        text.replacen(
            "\"sha256\":",
            "\"path\": \"data/app.db\",\n          \"sha256\":",
            1,
        )
    });
    let error = expect_corrupt_show(&store);
    assert!(
        error.message().contains("it repeats the key path"),
        "{}",
        error.message()
    );
}

#[test]
fn v2_manifest_with_an_extra_database_field_is_refused() {
    let (_base, store) = damaged_v2_store("damage-extra-field");
    damage_manifest_json(&store, "alpha", 1, &|value| {
        value["databases"][0]["extra"] = serde_json::json!(1);
    });
    expect_corrupt_show(&store);
}

#[test]
fn v2_manifest_missing_a_database_field_is_refused() {
    let (_base, store) = damaged_v2_store("damage-missing-field");
    damage_manifest_json(&store, "alpha", 1, &|value| {
        value["databases"][0]
            .as_object_mut()
            .unwrap()
            .remove("bundle_sha256");
    });
    expect_corrupt_show(&store);
}

#[test]
fn v2_manifest_with_members_of_the_wrong_type_is_refused() {
    let (_base, store) = damaged_v2_store("damage-members-type");
    damage_manifest_json(&store, "alpha", 1, &|value| {
        value["databases"][0]["members"] = serde_json::json!({});
    });
    expect_corrupt_show(&store);
}

#[test]
fn v2_manifest_with_an_empty_databases_array_is_refused() {
    let (_base, store) = damaged_v2_store("damage-empty-databases");
    damage_manifest_json(&store, "alpha", 1, &|value| {
        value["databases"] = serde_json::json!([]);
    });
    expect_corrupt_show(&store);
}

#[test]
fn v2_manifest_with_member_paths_out_of_bundle_order_is_refused() {
    let (_base, store) = damaged_v2_store("damage-member-order");
    damage_manifest_json(&store, "alpha", 1, &|value| {
        value["databases"][0]["members"]
            .as_array_mut()
            .unwrap()
            .swap(1, 2);
    });
    let error = expect_corrupt_show(&store);
    assert!(
        error.message().contains("appendages in that order"),
        "{}",
        error.message()
    );
}

#[test]
fn v2_manifest_with_a_member_that_is_not_an_appendage_is_refused() {
    let (_base, store) = damaged_v2_store("damage-member-appendage");
    damage_manifest_json(&store, "alpha", 1, &|value| {
        value["databases"][0]["members"][1]["path"] = serde_json::json!("data/other.db");
    });
    expect_corrupt_show(&store);
}

#[test]
fn v2_manifest_with_a_path_in_files_and_databases_is_refused() {
    let (_base, store) = damaged_v2_store("damage-overlap");
    let member_sha = sha256_hex(b"damage main");
    damage_manifest_json(&store, "alpha", 1, &|value| {
        value["files"] = serde_json::json!([
            {
                "path": "data/app.db",
                "sha256": member_sha,
                "size": 11
            },
            {
                "path": "notes.txt",
                "sha256": sha256_hex(b"damage notes"),
                "size": 12
            }
        ]);
    });
    let error = expect_corrupt_show(&store);
    assert!(
        error.message().contains("stands both in the files list"),
        "{}",
        error.message()
    );
}

#[test]
fn v2_manifest_with_two_databases_sharing_a_member_path_is_refused() {
    let (_base, store) = damaged_v2_store("damage-shared-member");
    let wal_sha = sha256_hex(b"damage wal");
    damage_manifest_json(&store, "alpha", 1, &|value| {
        value["databases"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "path": "data/app.db-wal",
                "bundle_sha256": wal_sha,
                "members": [
                    {"path": "data/app.db-wal", "sha256": wal_sha, "size": 10}
                ]
            }));
    });
    expect_corrupt_show(&store);
}

#[test]
fn v2_manifest_named_format_1_with_databases_is_refused() {
    let (_base, store) = damaged_v2_store("damage-named-1");
    damage_manifest_json(&store, "alpha", 1, &|value| {
        value["format"] = serde_json::json!("wayside-checkpoint/1");
    });
    expect_corrupt_show(&store);
}

#[test]
fn v2_manifest_missing_the_databases_field_is_refused() {
    let (_base, store) = damaged_v2_store("damage-no-databases");
    damage_manifest_json(&store, "alpha", 1, &|value| {
        value.as_object_mut().unwrap().remove("databases");
    });
    expect_corrupt_show(&store);
}

// WS-16, restore and fork of database payloads.

#[test]
fn restore_places_every_bundle_byte() {
    let base = temp_dir("restore-bundle");
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[
            ("notes.txt", b"restore notes"),
            ("data/app.db", b"restore main"),
            ("data/app.db-wal", b"restore wal"),
            ("data/app.db-journal", b"restore journal"),
        ],
    );
    db_checkpoint(&store, "alpha", &state, &["data/app.db"]).unwrap();
    let target = base.join("target");
    restore(&store, "alpha", 1, &target).unwrap();
    let placed = tree_bytes(&target);
    assert_eq!(placed["data/app.db"], b"restore main".to_vec());
    assert_eq!(placed["data/app.db-wal"], b"restore wal".to_vec());
    assert_eq!(placed["data/app.db-journal"], b"restore journal".to_vec());
    assert_eq!(placed["notes.txt"], b"restore notes".to_vec());
    assert!(placed.contains_key("wayside.env"));
    assert!(placed.contains_key("wayside.memory"));
}

#[test]
fn restore_refuses_a_damaged_member_before_placing_a_byte() {
    let base = temp_dir("restore-damaged");
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[
            ("data/app.db", b"guarded main"),
            ("data/app.db-wal", b"guarded wal"),
        ],
    );
    db_checkpoint(&store, "alpha", &state, &["data/app.db"]).unwrap();
    flip_first_byte(
        &checkpoint_dir(&store, "alpha", 1)
            .join("files")
            .join("data/app.db-wal"),
    );
    let target = base.join("target");
    let error = expect_error(restore(&store, "alpha", 1, &target));
    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
    assert!(
        error.message().contains("data/app.db-wal"),
        "{}",
        error.message()
    );
    assert!(!target.exists(), "restore placed bytes into {target:?}");
}

#[test]
fn rewind_of_a_v2_checkpoint_places_the_bundle() {
    let base = temp_dir("rewind-bundle");
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[("app.db", b"first main"), ("app.db-wal", b"first wal")],
    );
    db_checkpoint(&store, "alpha", &state, &["app.db"]).unwrap();
    std::fs::write(state.join("app.db"), b"second main").unwrap();
    db_checkpoint(&store, "alpha", &state, &["app.db"]).unwrap();
    let target = base.join("target");
    rewind(&store, "alpha", 1, &target).unwrap();
    assert_eq!(std::fs::read(target.join("app.db")).unwrap(), b"first main");
    assert_eq!(
        std::fs::read(target.join("app.db-wal")).unwrap(),
        b"first wal"
    );
}

#[test]
fn fork_of_a_v2_checkpoint_keeps_format_and_digests() {
    let base = temp_dir("fork-v2");
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[
            ("data/app.db", b"fork main"),
            ("data/app.db-wal", b"fork wal"),
        ],
    );
    let source = db_checkpoint(&store, "alpha", &state, &["data/app.db"]).unwrap();
    let forked = fork(&store, "alpha", 1, "beta").unwrap();
    assert_eq!(forked.format, "wayside-checkpoint/2");
    assert_eq!(forked.databases, source.databases);
    assert_eq!(
        forked.databases[0].bundle_sha256,
        source.databases[0].bundle_sha256
    );
    let alpha_target = base.join("alpha-target");
    let beta_target = base.join("beta-target");
    restore(&store, "alpha", 1, &alpha_target).unwrap();
    restore(&store, "beta", 1, &beta_target).unwrap();
    assert_eq!(tree_bytes(&alpha_target), tree_bytes(&beta_target));
    verify_checkpoint(&store, "beta", 1).unwrap();
}

#[test]
fn fork_of_a_v1_checkpoint_stays_format_1() {
    let base = temp_dir("fork-v1");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("app.db", b"plain main")]);
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    let forked = fork(&store, "alpha", 1, "beta").unwrap();
    assert_eq!(forked.format, "wayside-checkpoint/1");
    assert!(forked.databases.is_empty());
}

// WS-17, verify.

#[test]
fn verify_reports_clean_checkpoints_of_both_formats() {
    let base = temp_dir("verify-clean");
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[("app.db", b"clean main"), ("app.db-wal", b"clean wal")],
    );
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    let first = verify_checkpoint(&store, "alpha", 1).unwrap();
    assert_eq!(first.format, "wayside-checkpoint/1");
    db_checkpoint(&store, "alpha", &state, &["app.db"]).unwrap();
    let second = verify_checkpoint(&store, "alpha", 2).unwrap();
    assert_eq!(second.format, "wayside-checkpoint/2");
    assert_eq!(second.sequence, 2);
}

#[test]
fn verify_names_a_damaged_file_payload() {
    let base = temp_dir("verify-file");
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[("notes.txt", b"verify notes"), ("app.db", b"verify main")],
    );
    db_checkpoint(&store, "alpha", &state, &["app.db"]).unwrap();
    flip_first_byte(&checkpoint_dir(&store, "alpha", 1).join("files/notes.txt"));
    let error = expect_error(verify_checkpoint(&store, "alpha", 1));
    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
    assert!(error.message().contains("notes.txt"), "{}", error.message());
}

#[test]
fn verify_names_a_damaged_member_payload() {
    let base = temp_dir("verify-member");
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[("app.db", b"verify main"), ("app.db-wal", b"verify wal")],
    );
    db_checkpoint(&store, "alpha", &state, &["app.db"]).unwrap();
    flip_first_byte(
        &checkpoint_dir(&store, "alpha", 1)
            .join("files")
            .join("app.db-wal"),
    );
    let error = expect_error(verify_checkpoint(&store, "alpha", 1));
    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
    assert!(
        error.message().contains("app.db-wal"),
        "{}",
        error.message()
    );
}

#[test]
fn verify_leaves_the_store_byte_for_byte_unchanged() {
    let base = temp_dir("verify-untouched");
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[
            ("app.db", b"untouched main"),
            ("app.db-wal", b"untouched wal"),
        ],
    );
    db_checkpoint(&store, "alpha", &state, &["app.db"]).unwrap();
    let before = hash_tree(&store);
    verify_checkpoint(&store, "alpha", 1).unwrap();
    assert_eq!(hash_tree(&store), before);
}

#[test]
fn verify_of_a_missing_checkpoint_is_not_found() {
    let base = temp_dir("verify-missing");
    let store = base.join("store");
    let error = expect_error(verify_checkpoint(&store, "alpha", 1));
    assert!(matches!(error, Error::NotFound(_)), "{error:?}");
}

// WS-18, compare.

#[test]
fn compare_reports_equal_for_identical_bundles() {
    let base = temp_dir("compare-equal");
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[("app.db", b"same main"), ("app.db-wal", b"same wal")],
    );
    db_checkpoint(&store, "alpha", &state, &["app.db"]).unwrap();
    db_checkpoint(&store, "alpha", &state, &["app.db"]).unwrap();
    let comparison = compare_checkpoints(&store, "alpha", 1, 2).unwrap();
    assert_eq!(comparison.session, "alpha");
    assert_eq!(comparison.first, 1);
    assert_eq!(comparison.second, 2);
    assert_eq!(comparison.databases.len(), 1);
    assert_eq!(comparison.databases[0].path, "app.db");
    assert_eq!(comparison.databases[0].standing, DatabaseStanding::Equal);
}

#[test]
fn compare_reports_different_for_a_changed_bundle() {
    let base = temp_dir("compare-different");
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[("app.db", b"changing main"), ("app.db-wal", b"wal one")],
    );
    db_checkpoint(&store, "alpha", &state, &["app.db"]).unwrap();
    std::fs::write(state.join("app.db-wal"), b"wal two").unwrap();
    db_checkpoint(&store, "alpha", &state, &["app.db"]).unwrap();
    let comparison = compare_checkpoints(&store, "alpha", 1, 2).unwrap();
    assert_eq!(
        comparison.databases[0].standing,
        DatabaseStanding::Different
    );
}

#[test]
fn compare_between_formats_reports_one_side_only() {
    let base = temp_dir("compare-formats");
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[("app.db", b"mixed main"), ("app.db-wal", b"mixed wal")],
    );
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    db_checkpoint(&store, "alpha", &state, &["app.db"]).unwrap();
    let forward = compare_checkpoints(&store, "alpha", 1, 2).unwrap();
    assert_eq!(forward.databases.len(), 1);
    assert_eq!(forward.databases[0].standing, DatabaseStanding::OnlySecond);
    let backward = compare_checkpoints(&store, "alpha", 2, 1).unwrap();
    assert_eq!(backward.databases[0].standing, DatabaseStanding::OnlyFirst);
}

#[test]
fn compare_of_two_format_1_checkpoints_reports_no_databases() {
    let base = temp_dir("compare-empty");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("app.db", b"plain main")]);
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    let comparison = compare_checkpoints(&store, "alpha", 1, 2).unwrap();
    assert!(comparison.databases.is_empty());
}

#[test]
fn compare_answers_from_the_record_with_damaged_payloads() {
    let base = temp_dir("compare-record");
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[
            ("app.db", b"recorded main"),
            ("app.db-wal", b"recorded wal"),
        ],
    );
    db_checkpoint(&store, "alpha", &state, &["app.db"]).unwrap();
    db_checkpoint(&store, "alpha", &state, &["app.db"]).unwrap();
    flip_first_byte(
        &checkpoint_dir(&store, "alpha", 2)
            .join("files")
            .join("app.db-wal"),
    );
    // Verify now refuses the damaged checkpoint, and compare
    // still answers from the recorded digests alone.
    assert!(matches!(
        expect_error(verify_checkpoint(&store, "alpha", 2)),
        Error::Corrupt(_)
    ));
    let comparison = compare_checkpoints(&store, "alpha", 1, 2).unwrap();
    assert_eq!(comparison.databases[0].standing, DatabaseStanding::Equal);
}

#[test]
fn compare_reports_each_database_path_in_order() {
    let base = temp_dir("compare-paths");
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[
            ("a.db", b"a main"),
            ("b.db", b"b main"),
            ("b.db-wal", b"b wal one"),
        ],
    );
    db_checkpoint(&store, "alpha", &state, &["b.db", "a.db"]).unwrap();
    std::fs::write(state.join("b.db-wal"), b"b wal two").unwrap();
    db_checkpoint(&store, "alpha", &state, &["b.db", "a.db"]).unwrap();
    let comparison = compare_checkpoints(&store, "alpha", 1, 2).unwrap();
    let standings: Vec<(&str, &DatabaseStanding)> = comparison
        .databases
        .iter()
        .map(|entry| (entry.path.as_str(), &entry.standing))
        .collect();
    assert_eq!(
        standings,
        [
            ("a.db", &DatabaseStanding::Equal),
            ("b.db", &DatabaseStanding::Different)
        ]
    );
}

// The command line surface of the slice.

#[test]
fn cli_database_checkpoint_round_trip() {
    let base = temp_dir("cli-round-trip");
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[
            ("data/app.db", b"cli main"),
            ("data/app.db-wal", b"cli wal"),
        ],
    );
    let recorded = run_cli(
        &store,
        &[
            "checkpoint",
            "alpha",
            "--state",
            state.to_str().unwrap(),
            "--database",
            "data/app.db",
        ],
    );
    assert_eq!(recorded.status, 0, "{}", recorded.stderr);
    assert_eq!(recorded.stdout, "session alpha checkpoint 1\n");
    let shown = run_cli(&store, &["show", "alpha", "1"]);
    assert_eq!(shown.status, 0, "{}", shown.stderr);
    assert!(shown.stdout.contains("\"databases\""), "{}", shown.stdout);
    assert!(shown.stdout.contains("data/app.db"), "{}", shown.stdout);
    let target = base.join("target");
    let restored = run_cli(
        &store,
        &[
            "restore",
            "alpha",
            "1",
            "--target",
            target.to_str().unwrap(),
        ],
    );
    assert_eq!(restored.status, 0, "{}", restored.stderr);
    assert_eq!(
        std::fs::read(target.join("data/app.db-wal")).unwrap(),
        b"cli wal"
    );
}

#[test]
fn cli_verify_and_compare_lines() {
    let base = temp_dir("cli-verify-compare");
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[("app.db", b"cli main"), ("app.db-wal", b"cli wal one")],
    );
    let plain = run_cli(
        &store,
        &["checkpoint", "alpha", "--state", state.to_str().unwrap()],
    );
    assert_eq!(plain.status, 0, "{}", plain.stderr);
    let with_db = run_cli(
        &store,
        &[
            "checkpoint",
            "alpha",
            "--state",
            state.to_str().unwrap(),
            "--database",
            "app.db",
        ],
    );
    assert_eq!(with_db.status, 0, "{}", with_db.stderr);
    let verified = run_cli(&store, &["verify", "alpha", "2"]);
    assert_eq!(verified.status, 0, "{}", verified.stderr);
    assert_eq!(verified.stdout, "verified session alpha checkpoint 2\n");
    let one_side = run_cli(&store, &["compare", "alpha", "1", "2"]);
    assert_eq!(one_side.status, 0, "{}", one_side.stderr);
    assert_eq!(one_side.stdout, "app.db recorded on checkpoint 2 only\n");
    std::fs::write(state.join("app.db-wal"), b"cli wal two").unwrap();
    let third = run_cli(
        &store,
        &[
            "checkpoint",
            "alpha",
            "--state",
            state.to_str().unwrap(),
            "--database",
            "app.db",
        ],
    );
    assert_eq!(third.status, 0, "{}", third.stderr);
    let different = run_cli(&store, &["compare", "alpha", "2", "3"]);
    assert_eq!(different.stdout, "app.db different\n");
    let fourth = run_cli(
        &store,
        &[
            "checkpoint",
            "alpha",
            "--state",
            state.to_str().unwrap(),
            "--database",
            "app.db",
        ],
    );
    assert_eq!(fourth.status, 0, "{}", fourth.stderr);
    let equal = run_cli(&store, &["compare", "alpha", "3", "4"]);
    assert_eq!(equal.stdout, "app.db equal\n");
    let empty = run_cli(&store, &["compare", "alpha", "1", "1"]);
    assert_eq!(empty.status, 0, "{}", empty.stderr);
    assert_eq!(empty.stdout, "");
}

#[test]
fn cli_verify_of_a_damaged_store_exits_two_without_a_traceback() {
    let base = temp_dir("cli-verify-damaged");
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[("app.db", b"cli main"), ("app.db-wal", b"cli wal")],
    );
    db_checkpoint(&store, "alpha", &state, &["app.db"]).unwrap();
    flip_first_byte(
        &checkpoint_dir(&store, "alpha", 1)
            .join("files")
            .join("app.db-wal"),
    );
    let verified = run_cli(&store, &["verify", "alpha", "1"]);
    assert_eq!(verified.status, 2);
    assert!(
        verified.stderr.starts_with("error: "),
        "{}",
        verified.stderr
    );
    assert!(
        verified.stderr.contains("app.db-wal"),
        "{}",
        verified.stderr
    );
    assert!(
        !verified.stderr.contains("Traceback"),
        "{}",
        verified.stderr
    );
}

#[test]
fn cli_database_option_accepts_an_unambiguous_prefix() {
    let base = temp_dir("cli-prefix");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("app.db", b"prefix main")]);
    let recorded = run_cli(
        &store,
        &[
            "checkpoint",
            "alpha",
            "--state",
            state.to_str().unwrap(),
            "--data",
            "app.db",
        ],
    );
    assert_eq!(recorded.status, 0, "{}", recorded.stderr);
    let manifest = show(&store, "alpha", 1).unwrap();
    assert_eq!(manifest.format, "wayside-checkpoint/2");
}

// The frozen /2 store, written by this slice's own command
// line. The production record stands in MANIFEST.md beside
// the fixture.

fn fixture_root(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

#[test]
fn fixture_database_snapshot() {
    let fixture = fixture_root("database-snapshot");
    let base = temp_dir("fixture-database-snapshot");
    let store = base.join("store");
    copy_tree(&fixture.join("store"), &store);

    // The frozen store matches its recorded inventory.
    let inventory = std::fs::read_to_string(fixture.join("inventory.sha256")).unwrap();
    for line in inventory.lines() {
        let mut parts = line.splitn(3, "  ");
        let hash = parts.next().unwrap();
        let size: usize = parts.next().unwrap().parse().unwrap();
        let rel = parts.next().unwrap();
        let bytes = std::fs::read(store.join(rel)).unwrap();
        assert_eq!(bytes.len(), size, "inventory size for {rel}");
        assert_eq!(sha256_hex(&bytes), hash, "inventory hash for {rel}");
    }

    let listed = run_cli(&store, &["list", "alpha"]);
    assert_eq!(listed.status, 0, "{}", listed.stderr);
    let expected_list = std::fs::read_to_string(fixture.join("expected-list-alpha.txt")).unwrap();
    assert_eq!(listed.stdout, expected_list);

    for sequence in [1, 2, 3] {
        let shown = run_cli(&store, &["show", "alpha", &sequence.to_string()]);
        assert_eq!(shown.status, 0, "{}", shown.stderr);
        let expected_show =
            std::fs::read_to_string(fixture.join(format!("expected-show-alpha-{sequence}.txt")))
                .unwrap();
        assert_eq!(shown.stdout, expected_show, "show for {sequence}");
        let verified = run_cli(&store, &["verify", "alpha", &sequence.to_string()]);
        assert_eq!(verified.status, 0, "{}", verified.stderr);
        let expected_verify =
            std::fs::read_to_string(fixture.join(format!("expected-verify-alpha-{sequence}.txt")))
                .unwrap();
        assert_eq!(verified.stdout, expected_verify, "verify for {sequence}");
    }

    for (first, second) in [(1, 2), (2, 3), (1, 1)] {
        let compared = run_cli(
            &store,
            &["compare", "alpha", &first.to_string(), &second.to_string()],
        );
        assert_eq!(compared.status, 0, "{}", compared.stderr);
        let expected_compare =
            std::fs::read_to_string(fixture.join(format!("expected-compare-{first}-{second}.txt")))
                .unwrap();
        assert_eq!(
            compared.stdout, expected_compare,
            "compare for {first} and {second}"
        );
    }

    let target = base.join("restore-alpha-2");
    let restored = run_cli(
        &store,
        &[
            "restore",
            "alpha",
            "2",
            "--target",
            target.to_str().unwrap(),
        ],
    );
    assert_eq!(restored.status, 0, "{}", restored.stderr);
    let expected_tree = tree_bytes(&fixture.join("expected-restore").join("alpha-2"));
    assert_eq!(tree_bytes(&target), expected_tree);

    // A checkpoint this code writes into a copy of the frozen
    // store takes the next sequence and reads back.
    let state = make_state(
        &base.join("fresh-state"),
        &[
            ("data/app.db", b"fixture main, fourth writing"),
            ("data/app.db-wal", b"fixture wal, fourth writing"),
        ],
    );
    let manifest = db_checkpoint(&store, "alpha", &state, &["data/app.db"]).unwrap();
    assert_eq!(manifest.sequence, 4);
    assert_eq!(manifest.format, "wayside-checkpoint/2");
    assert_eq!(show(&store, "alpha", 4).unwrap(), manifest);
}
