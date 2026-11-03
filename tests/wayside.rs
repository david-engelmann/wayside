//! The behaviour pins, translated pin for pin from the Python
//! suite that closed Round 1. Each test names the same fact its
//! Python original named, and the parametrized families expand
//! to one test per value, named by the value, keeping the item
//! count at parity: 76 translated tests. Where the Rust harness
//! cannot carry a Python mechanism, the pin keeps its substance
//! and changes its injection, and each such change is stated
//! where it lands. The monkeypatched directory listing of the
//! two walk error pins is the listing seam on the library's
//! state tree reader. The parallel writers pin spawns this test
//! binary in a worker mode, so the writers are separate
//! processes racing the store lock, as the Python driver
//! processes were. The killed writer pin drives the built
//! wayside binary, as the plan for the port records.
//!
//! Reported separately from the translated count, in the
//! fixture module at the bottom: one test per Python written
//! fixture store, and one manifest byte identity pin.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use wayside::{
    checkpoint, checkpoint_with_reader, fork, list_checkpoints, restore, rewind, show, Error,
    StateTreeReader,
};

const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

static COUNTER: AtomicUsize = AtomicUsize::new(0);
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn temp_dir(tag: &str) -> PathBuf {
    let count = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir =
        std::env::temp_dir().join(format!("wayside-test-{}-{tag}-{count}", std::process::id()));
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

fn env_of(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn expect_error<T>(result: wayside::Result<T>) -> Error {
    match result {
        Ok(_) => panic!("the operation succeeded where the pin requires a refusal"),
        Err(error) => error,
    }
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

/// Drive the CLI with a deadline, for the pin whose Python
/// shape fails the test when the command hangs.
fn run_cli_timed(store: &Path, args: &[&str], timeout: Duration) -> CliOutput {
    let mut child = Command::new(env!("CARGO_BIN_EXE_wayside"))
        .arg("--store")
        .arg(store)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            child
                .stdout
                .take()
                .unwrap()
                .read_to_end(&mut stdout)
                .unwrap();
            child
                .stderr
                .take()
                .unwrap()
                .read_to_end(&mut stderr)
                .unwrap();
            return CliOutput {
                status: status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&stdout).into_owned(),
                stderr: String::from_utf8_lossy(&stderr).into_owned(),
            };
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the command did not finish, it hung");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
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

#[test]
fn checkpoint_records_files_env_and_memory() {
    let base = temp_dir("records");
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[
            ("notes.txt", b"synthetic notes"),
            ("work/plan.txt", b"synthetic plan"),
        ],
    );
    let env = env_of(&[("MODE", "synthetic"), ("LEVEL", "3")]);
    let manifest = checkpoint(
        &store,
        "alpha",
        &state,
        Some(&env),
        b"synthetic memory document",
    )
    .unwrap();
    assert_eq!(manifest.format, "wayside-checkpoint/1");
    assert_eq!(manifest.session, "alpha");
    assert_eq!(manifest.sequence, 1);
    assert_eq!(
        manifest.env,
        env_of(&[("LEVEL", "3"), ("MODE", "synthetic")])
    );
    let paths: Vec<&str> = manifest
        .files
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert_eq!(paths, ["notes.txt", "work/plan.txt"]);
    for entry in &manifest.files {
        let payload = checkpoint_dir(&store, "alpha", 1)
            .join("files")
            .join(&entry.path);
        let bytes = std::fs::read(&payload).unwrap();
        assert_eq!(bytes, std::fs::read(state.join(&entry.path)).unwrap());
        assert_eq!(entry.size, bytes.len() as i128);
        assert_eq!(entry.sha256, sha256_hex(&bytes));
    }
    assert_eq!(
        manifest.memory.size,
        b"synthetic memory document".len() as i128
    );
    assert_eq!(show(&store, "alpha", 1).unwrap(), manifest);
    let listed: Vec<i64> = list_checkpoints(&store, "alpha")
        .unwrap()
        .iter()
        .map(|entry| entry.sequence)
        .collect();
    assert_eq!(listed, [1]);
}

#[test]
fn checkpoint_never_records_the_live_environment() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::set_var("WAYSIDE_UNDECLARED_PROBE", "synthetic-undeclared-value");
    let base = temp_dir("live-env");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    let env = env_of(&[("DECLARED", "yes")]);
    let manifest = checkpoint(&store, "alpha", &state, Some(&env), b"").unwrap();
    std::env::remove_var("WAYSIDE_UNDECLARED_PROBE");
    assert_eq!(manifest.env, env_of(&[("DECLARED", "yes")]));
    let raw = std::fs::read_to_string(manifest_path(&store, "alpha", 1)).unwrap();
    assert!(!raw.contains("WAYSIDE_UNDECLARED_PROBE"));
    assert!(!raw.contains("synthetic-undeclared-value"));
}

#[test]
fn checkpoint_refuses_a_missing_state_directory() {
    let base = temp_dir("missing-state");
    let error = expect_error(checkpoint(
        &base.join("store"),
        "alpha",
        &base.join("nope"),
        None,
        b"",
    ));
    assert!(matches!(error, Error::Wayside(_)), "{error:?}");
}

#[test]
fn checkpoint_refuses_a_symlink_in_the_state_tree() {
    let base = temp_dir("state-link");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("real.txt", b"real")]);
    std::os::unix::fs::symlink(state.join("real.txt"), state.join("link.txt")).unwrap();
    let error = expect_error(checkpoint(&store, "alpha", &state, None, b""));
    assert!(matches!(error, Error::Wayside(_)), "{error:?}");
    std::fs::remove_file(state.join("link.txt")).unwrap();
    std::os::unix::fs::symlink(&state, state.join("linked-dir")).unwrap();
    let error = expect_error(checkpoint(&store, "alpha", &state, None, b""));
    assert!(matches!(error, Error::Wayside(_)), "{error:?}");
}

#[test]
fn checkpoint_refuses_a_non_regular_file_in_the_state_tree() {
    let base = temp_dir("state-fifo");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("real.txt", b"real")]);
    let fifo = state.join("pipe");
    let c_path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    let made = unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) };
    assert_eq!(made, 0);
    let error = expect_error(checkpoint(&store, "alpha", &state, None, b""));
    assert!(matches!(error, Error::Wayside(_)), "{error:?}");
}

#[test]
fn checkpoint_refuses_a_reserved_top_level_name() {
    let base = temp_dir("reserved");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("wayside.env", b"planted")]);
    let error = expect_error(checkpoint(&store, "alpha", &state, None, b""));
    assert!(matches!(error, Error::Wayside(_)), "{error:?}");
    let state2 = make_state(
        &base.join("state2"),
        &[("nested/wayside.env", b"nested is fine")],
    );
    let manifest = checkpoint(&store, "alpha", &state2, None, b"").unwrap();
    let paths: Vec<&str> = manifest
        .files
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert_eq!(paths, ["nested/wayside.env"]);
}

#[test]
fn checkpoint_of_an_empty_state_tree_records_no_files() {
    let base = temp_dir("empty-tree");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[]);
    let manifest = checkpoint(&store, "alpha", &state, None, b"").unwrap();
    assert!(manifest.files.is_empty());
    let target = base.join("target");
    restore(&store, "alpha", 1, &target).unwrap();
    let mut names: Vec<String> = std::fs::read_dir(&target)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["wayside.env", "wayside.memory"]);
}

#[test]
fn memory_defaults_to_an_empty_document() {
    let base = temp_dir("memory-default");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    let manifest = checkpoint(&store, "alpha", &state, None, b"").unwrap();
    assert_eq!(manifest.memory.sha256, EMPTY_SHA256);
    assert_eq!(manifest.memory.size, 0);
    let target = base.join("target");
    restore(&store, "alpha", 1, &target).unwrap();
    assert_eq!(std::fs::read(target.join("wayside.memory")).unwrap(), b"");
}

#[test]
fn env_values_with_a_newline_or_nul_are_refused() {
    let base = temp_dir("env-refused");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    for env in [
        env_of(&[("BAD", "two\nlines")]),
        env_of(&[("BAD", "has\0nul")]),
        env_of(&[("1BAD", "fine value")]),
    ] {
        let error = expect_error(checkpoint(&store, "alpha", &state, Some(&env), b""));
        assert!(matches!(error, Error::Wayside(_)), "{error:?}");
    }
}

#[test]
fn sequences_increment_and_list_in_order() {
    let base = temp_dir("sequence-order");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    for expected in [1, 2, 3] {
        let manifest = checkpoint(&store, "alpha", &state, None, b"").unwrap();
        assert_eq!(manifest.sequence, expected);
    }
    let listed: Vec<i64> = list_checkpoints(&store, "alpha")
        .unwrap()
        .iter()
        .map(|entry| entry.sequence)
        .collect();
    assert_eq!(listed, [1, 2, 3]);
}

#[test]
fn restore_materializes_exactly() {
    let base = temp_dir("restore-exact");
    let store = base.join("store");
    let binary: Vec<u8> = (0..=255u8).collect();
    let state = make_state(
        &base.join("state"),
        &[("top.txt", b"top bytes"), ("deep/nested/file.bin", &binary)],
    );
    let env = env_of(&[("ZED", "last"), ("FIRST", "1")]);
    checkpoint(&store, "alpha", &state, Some(&env), b"memory payload").unwrap();
    let target = base.join("target");
    restore(&store, "alpha", 1, &target).unwrap();
    assert_eq!(std::fs::read(target.join("top.txt")).unwrap(), b"top bytes");
    assert_eq!(
        std::fs::read(target.join("deep/nested/file.bin")).unwrap(),
        binary
    );
    assert_eq!(
        std::fs::read(target.join("wayside.memory")).unwrap(),
        b"memory payload"
    );
    let env_text = std::fs::read_to_string(target.join("wayside.env")).unwrap();
    let lines: Vec<&str> = env_text.lines().collect();
    assert!(lines[0].starts_with('#'));
    assert_eq!(lines[1..], ["FIRST=1", "ZED=last"]);
}

#[test]
fn restore_into_an_empty_existing_directory() {
    let base = temp_dir("restore-empty-dir");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    let target = base.join("target");
    std::fs::create_dir_all(&target).unwrap();
    restore(&store, "alpha", 1, &target).unwrap();
    assert_eq!(std::fs::read(target.join("a.txt")).unwrap(), b"a");
}

#[test]
fn restore_refuses_a_non_empty_target_and_leaves_it_untouched() {
    let base = temp_dir("restore-nonempty");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    let target = base.join("target");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("sentinel.txt"), b"do not touch").unwrap();
    let error = expect_error(restore(&store, "alpha", 1, &target));
    assert!(matches!(error, Error::Target(_)), "{error:?}");
    let mut expected = BTreeMap::new();
    expected.insert("sentinel.txt".to_string(), sha256_hex(b"do not touch"));
    assert_eq!(hash_tree(&target), expected);
}

#[test]
fn restore_refuses_a_file_at_the_target_path() {
    let base = temp_dir("restore-file-target");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    let target = base.join("target");
    std::fs::write(&target, b"a file, not a directory").unwrap();
    let error = expect_error(restore(&store, "alpha", 1, &target));
    assert!(matches!(error, Error::Target(_)), "{error:?}");
    assert_eq!(std::fs::read(&target).unwrap(), b"a file, not a directory");
}

#[test]
fn flipped_payload_byte_fails_restore_and_places_nothing() {
    let base = temp_dir("flipped-payload");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"original bytes")]);
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    let payload = checkpoint_dir(&store, "alpha", 1)
        .join("files")
        .join("a.txt");
    let mut damaged = std::fs::read(&payload).unwrap();
    damaged[0] ^= 0xff;
    std::fs::write(&payload, &damaged).unwrap();
    let target = base.join("target");
    let error = expect_error(restore(&store, "alpha", 1, &target));
    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
    assert!(!target.exists());
}

#[test]
fn flipped_memory_byte_fails_restore_and_places_nothing() {
    let base = temp_dir("flipped-memory");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    checkpoint(&store, "alpha", &state, None, b"memory bytes").unwrap();
    let memory = checkpoint_dir(&store, "alpha", 1).join("memory.bin");
    let mut damaged = std::fs::read(&memory).unwrap();
    let last = damaged.len() - 1;
    damaged[last] ^= 0xff;
    std::fs::write(&memory, &damaged).unwrap();
    let target = base.join("target");
    let error = expect_error(restore(&store, "alpha", 1, &target));
    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
    assert!(!target.exists());
}

#[test]
fn a_checkpoint_never_changes_once_written() {
    let base = temp_dir("immutable");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"first")]);
    checkpoint(&store, "alpha", &state, None, b"one").unwrap();
    let before = hash_tree(&checkpoint_dir(&store, "alpha", 1));
    std::fs::write(state.join("a.txt"), b"second").unwrap();
    checkpoint(&store, "alpha", &state, None, b"two").unwrap();
    checkpoint(&store, "alpha", &state, None, b"three").unwrap();
    assert_eq!(hash_tree(&checkpoint_dir(&store, "alpha", 1)), before);
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

#[test]
fn manifest_with_a_duplicate_top_level_key_is_refused() {
    let base = temp_dir("dup-top");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    damage_manifest_text(&store, "alpha", 1, &|text| {
        text.replacen(
            "\"format\": \"wayside-checkpoint/1\",",
            "\"format\": \"wayside-checkpoint/1\",\n  \"format\": \"wayside-checkpoint/1\",",
            1,
        )
    });
    let error = expect_error(show(&store, "alpha", 1));
    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
}

#[test]
fn manifest_with_a_duplicate_nested_key_is_refused() {
    let base = temp_dir("dup-nested");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    damage_manifest_text(&store, "alpha", 1, &|text| {
        text.replacen(
            "\"path\": \"a.txt\",",
            "\"path\": \"a.txt\",\n      \"path\": \"a.txt\",",
            1,
        )
    });
    let error = expect_error(restore(&store, "alpha", 1, &base.join("target")));
    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
}

#[test]
fn manifest_with_a_wrong_format_is_refused() {
    let base = temp_dir("wrong-format");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    damage_manifest_text(&store, "alpha", 1, &|text| {
        text.replace("wayside-checkpoint/1", "wayside-checkpoint/2")
    });
    let error = expect_error(show(&store, "alpha", 1));
    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
}

#[test]
fn manifest_with_a_missing_or_extra_field_is_refused() {
    let base = temp_dir("field-set");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    damage_manifest_json(&store, "alpha", 1, &|value| {
        value.as_object_mut().unwrap().remove("env");
    });
    let error = expect_error(show(&store, "alpha", 1));
    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
    checkpoint(&store, "beta", &state, None, b"").unwrap();
    damage_manifest_json(&store, "beta", 1, &|value| {
        value
            .as_object_mut()
            .unwrap()
            .insert("surprise".to_string(), serde_json::json!(1));
    });
    let error = expect_error(show(&store, "beta", 1));
    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
}

#[test]
fn manifest_with_undecodable_bytes_is_refused() {
    let base = temp_dir("undecodable");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    std::fs::write(
        manifest_path(&store, "alpha", 1),
        b"\xff\xfe not a manifest",
    )
    .unwrap();
    let error = expect_error(show(&store, "alpha", 1));
    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
}

#[test]
fn manifest_naming_a_foreign_session_or_sequence_is_refused() {
    let base = temp_dir("foreign-names");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    damage_manifest_json(&store, "alpha", 1, &|value| {
        value["session"] = serde_json::json!("other");
    });
    let error = expect_error(show(&store, "alpha", 1));
    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
    checkpoint(&store, "beta", &state, None, b"").unwrap();
    damage_manifest_json(&store, "beta", 1, &|value| {
        value["sequence"] = serde_json::json!(99);
    });
    let error = expect_error(show(&store, "beta", 1));
    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
}

fn unsafe_recorded_path_case(tag: &str, bad_path: &str) {
    let base = temp_dir(tag);
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    damage_manifest_json(&store, "alpha", 1, &|value| {
        value["files"][0]["path"] = serde_json::json!(bad_path);
    });
    let error = expect_error(show(&store, "alpha", 1));
    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
    let target = base.join("target");
    let error = expect_error(restore(&store, "alpha", 1, &target));
    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
    assert!(!target.exists());
    assert!(!base.join("escape.txt").exists());
}

#[test]
fn manifest_with_an_unsafe_recorded_path_is_refused_parent_reference() {
    unsafe_recorded_path_case("unsafe-parent", "../escape.txt");
}

#[test]
fn manifest_with_an_unsafe_recorded_path_is_refused_absolute_path() {
    unsafe_recorded_path_case("unsafe-absolute", "/etc/passwd");
}

#[test]
fn manifest_with_an_unsafe_recorded_path_is_refused_nested_parent_reference() {
    unsafe_recorded_path_case("unsafe-nested-parent", "a/../../escape.txt");
}

#[test]
fn manifest_with_an_unsafe_recorded_path_is_refused_backslash() {
    unsafe_recorded_path_case("unsafe-backslash", "a\\b.txt");
}

fn unsafe_session_id_case(tag: &str, bad_id: &str) {
    let base = temp_dir(tag);
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    let error = expect_error(checkpoint(&store, bad_id, &state, None, b""));
    assert!(matches!(error, Error::InvalidName(_)), "{error:?}");
    let error = expect_error(list_checkpoints(&store, bad_id));
    assert!(matches!(error, Error::InvalidName(_)), "{error:?}");
}

#[test]
fn unsafe_session_ids_are_refused_parent_reference() {
    unsafe_session_id_case("bad-id-parent", "../x");
}

#[test]
fn unsafe_session_ids_are_refused_slash() {
    unsafe_session_id_case("bad-id-slash", "a/b");
}

#[test]
fn unsafe_session_ids_are_refused_backslash() {
    unsafe_session_id_case("bad-id-backslash", "a\\b");
}

#[test]
fn unsafe_session_ids_are_refused_empty() {
    unsafe_session_id_case("bad-id-empty", "");
}

#[test]
fn unsafe_session_ids_are_refused_upper() {
    unsafe_session_id_case("bad-id-upper", "UPPER");
}

#[test]
fn unsafe_session_ids_are_refused_lead_hyphen() {
    unsafe_session_id_case("bad-id-lead", "-lead");
}

#[test]
fn unsafe_session_ids_are_refused_space() {
    unsafe_session_id_case("bad-id-space", "a b");
}

#[test]
fn unsafe_session_ids_are_refused_double_dot() {
    unsafe_session_id_case("bad-id-dots", "a..b");
}

#[test]
fn unsafe_session_ids_are_refused_too_long() {
    unsafe_session_id_case("bad-id-long", &"x".repeat(65));
}

#[test]
fn list_and_show_on_a_missing_session_are_clean_errors() {
    let base = temp_dir("missing-session");
    let store = base.join("store");
    let error = expect_error(list_checkpoints(&store, "ghost"));
    assert!(matches!(error, Error::NotFound(_)), "{error:?}");
    let error = expect_error(show(&store, "ghost", 1));
    assert!(matches!(error, Error::NotFound(_)), "{error:?}");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    let error = expect_error(show(&store, "alpha", 7));
    assert!(matches!(error, Error::NotFound(_)), "{error:?}");
}

#[test]
fn planted_residue_is_ignored_by_list_and_sequence() {
    let base = temp_dir("residue");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    let residue = store
        .join("sessions")
        .join("alpha")
        .join("checkpoints")
        .join(".tmp-deadbeef");
    std::fs::create_dir_all(&residue).unwrap();
    std::fs::write(residue.join("manifest.json"), "{}").unwrap();
    let listed: Vec<i64> = list_checkpoints(&store, "alpha")
        .unwrap()
        .iter()
        .map(|entry| entry.sequence)
        .collect();
    assert_eq!(listed, [1]);
    let manifest = checkpoint(&store, "alpha", &state, None, b"").unwrap();
    assert_eq!(manifest.sequence, 2);
    let ghost = store
        .join("sessions")
        .join("ghost")
        .join("checkpoints")
        .join(".tmp-cafe");
    std::fs::create_dir_all(&ghost).unwrap();
    std::fs::write(ghost.join("manifest.json"), "{}").unwrap();
    assert_eq!(list_checkpoints(&store, "ghost").unwrap(), []);
}

/// The worker mode of the parallel writers pin. Spawned by
/// the pin below as a separate process with the
/// WAYSIDE_WORKER_ variables set; run by the suite without
/// them it returns at once and asserts nothing. As a worker,
/// a failed checkpoint fails the test and the worker process
/// exits nonzero, which the pin asserts against.
#[test]
fn parallel_worker_entry() {
    let Ok(store) = std::env::var("WAYSIDE_WORKER_STORE") else {
        return;
    };
    let session = std::env::var("WAYSIDE_WORKER_SESSION").unwrap();
    let state = std::env::var("WAYSIDE_WORKER_STATE").unwrap();
    let count: usize = std::env::var("WAYSIDE_WORKER_COUNT")
        .unwrap()
        .parse()
        .unwrap();
    let env = env_of(&[("DRIVER", &session)]);
    for _ in 0..count {
        checkpoint(
            Path::new(&store),
            &session,
            Path::new(&state),
            Some(&env),
            b"driver memory",
        )
        .unwrap();
    }
}

#[test]
fn parallel_writers_publish_a_gapless_sequence() {
    let base = temp_dir("parallel");
    let store = base.join("store");
    let workers = 8;
    let each = 3;
    let exe = std::env::current_exe().unwrap();
    let mut children: Vec<Child> = Vec::new();
    for index in 0..workers {
        let state = make_state(
            &base.join(format!("state{index}")),
            &[("work.txt", format!("worker {index}").as_bytes())],
        );
        let child = Command::new(&exe)
            .arg("parallel_worker_entry")
            .arg("--exact")
            .arg("--nocapture")
            .env("WAYSIDE_WORKER_STORE", &store)
            .env("WAYSIDE_WORKER_SESSION", "alpha")
            .env("WAYSIDE_WORKER_STATE", &state)
            .env("WAYSIDE_WORKER_COUNT", each.to_string())
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
        assert_eq!(show(&store, "alpha", sequence).unwrap().sequence, sequence);
    }
}

#[test]
fn killed_writer_leaves_no_visible_checkpoint() {
    let base = temp_dir("killed-writer");
    let big = base.join("big-state");
    std::fs::create_dir_all(&big).unwrap();
    let payload = vec![b'x'; 8192];
    for index in 0..2500 {
        std::fs::write(big.join(format!("file{index:04}.bin")), &payload).unwrap();
    }
    let mut killed_mid_write = false;
    let mut last_store = base.join("store-never");
    for attempt in 0..5 {
        let store = base.join(format!("store{attempt}"));
        let mut child = Command::new(env!("CARGO_BIN_EXE_wayside"))
            .arg("--store")
            .arg(&store)
            .arg("checkpoint")
            .arg("victim")
            .arg("--state")
            .arg(&big)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let ck_dir = store.join("sessions").join("victim").join("checkpoints");
        let mut seen_tmp = false;
        loop {
            if ck_dir.is_dir() {
                if let Ok(entries) = std::fs::read_dir(&ck_dir) {
                    for entry in entries.flatten() {
                        if entry.file_name().to_string_lossy().starts_with(".tmp-") {
                            seen_tmp = true;
                        }
                    }
                }
            }
            if seen_tmp {
                break;
            }
            match child.try_wait().unwrap() {
                Some(_) => break,
                None => std::thread::sleep(Duration::from_millis(1)),
            }
        }
        let killed = child.try_wait().unwrap().is_none();
        if killed {
            child.kill().unwrap();
        }
        child.wait().unwrap();
        let visible = match list_checkpoints(&store, "victim") {
            Ok(entries) => entries,
            Err(Error::NotFound(_)) => Vec::new(),
            Err(error) => panic!("list after the kill failed otherwise: {error:?}"),
        };
        // A writer that finished before the kill, or was killed
        // after its rename landed, leaves a complete checkpoint
        // standing, and that is a clean outcome this attempt
        // simply did not interrupt. Every visible checkpoint
        // must read clean, which is the standing invariant, and
        // the attempt counts as caught only when the kill landed
        // mid write and nothing is visible.
        for entry in &visible {
            show(&store, "victim", entry.sequence).unwrap();
        }
        if killed && seen_tmp && visible.is_empty() {
            killed_mid_write = true;
            last_store = store.clone();
            break;
        }
    }
    assert!(killed_mid_write, "the writer was never caught mid write");
    let small = make_state(&base.join("small"), &[("a", b"a")]);
    let manifest = checkpoint(&last_store, "victim", &small, None, b"big").unwrap();
    assert_eq!(manifest.sequence, 1);
}

#[test]
fn fork_carries_content_and_records_lineage() {
    let base = temp_dir("fork-lineage");
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[("a.txt", b"alpha bytes"), ("dir/b.txt", b"beta")],
    );
    let env = env_of(&[("K", "V")]);
    checkpoint(&store, "alpha", &state, Some(&env), b"alpha memory").unwrap();
    std::fs::write(state.join("a.txt"), b"changed later").unwrap();
    checkpoint(&store, "alpha", &state, None, b"later memory").unwrap();
    let manifest = fork(&store, "alpha", 1, "beta").unwrap();
    assert_eq!(manifest.session, "beta");
    assert_eq!(manifest.sequence, 1);
    let lineage = manifest.forked_from.clone().unwrap();
    assert_eq!(lineage.session, "alpha");
    assert_eq!(lineage.sequence, 1);
    let listed: Vec<i64> = list_checkpoints(&store, "beta")
        .unwrap()
        .iter()
        .map(|entry| entry.sequence)
        .collect();
    assert_eq!(listed, [1]);
    let from_alpha = base.join("from-alpha");
    let from_beta = base.join("from-beta");
    restore(&store, "alpha", 1, &from_alpha).unwrap();
    restore(&store, "beta", 1, &from_beta).unwrap();
    assert_eq!(hash_tree(&from_alpha), hash_tree(&from_beta));
    assert_eq!(
        std::fs::read(from_beta.join("a.txt")).unwrap(),
        b"alpha bytes"
    );
}

#[test]
fn fork_refuses_an_existing_session() {
    let base = temp_dir("fork-existing");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    checkpoint(&store, "beta", &state, None, b"").unwrap();
    let error = expect_error(fork(&store, "alpha", 1, "beta"));
    assert!(matches!(error, Error::Wayside(_)), "{error:?}");
}

#[test]
fn fork_refuses_a_corrupt_source_and_creates_nothing() {
    let base = temp_dir("fork-corrupt");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"original")]);
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    let payload = checkpoint_dir(&store, "alpha", 1)
        .join("files")
        .join("a.txt");
    let mut damaged = std::fs::read(&payload).unwrap();
    damaged[0] ^= 0xff;
    std::fs::write(&payload, &damaged).unwrap();
    let error = expect_error(fork(&store, "alpha", 1, "beta"));
    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
    assert!(!store.join("sessions").join("beta").exists());
}

#[test]
fn rewind_restores_an_earlier_state() {
    let base = temp_dir("rewind-earlier");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"version one")]);
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    std::fs::write(state.join("a.txt"), b"version two").unwrap();
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    std::fs::write(state.join("a.txt"), b"version three").unwrap();
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    let target = base.join("target");
    rewind(&store, "alpha", 1, &target).unwrap();
    assert_eq!(std::fs::read(target.join("a.txt")).unwrap(), b"version one");
}

#[test]
fn rewind_refuses_the_newest_and_a_missing_sequence() {
    let base = temp_dir("rewind-refusals");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    let error = expect_error(rewind(&store, "alpha", 2, &base.join("t1")));
    assert!(matches!(error, Error::Wayside(_)), "{error:?}");
    let error = expect_error(rewind(&store, "alpha", 9, &base.join("t2")));
    assert!(matches!(error, Error::NotFound(_)), "{error:?}");
}

#[test]
fn cli_checkpoint_restore_round_trip() {
    let base = temp_dir("cli-round-trip");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"cli bytes")]);
    let memory_file = base.join("memory.txt");
    std::fs::write(&memory_file, b"cli memory").unwrap();
    let result = run_cli(
        &store,
        &[
            "checkpoint",
            "alpha",
            "--state",
            state.to_str().unwrap(),
            "--memory",
            memory_file.to_str().unwrap(),
            "--env",
            "MODE=cli",
        ],
    );
    assert_eq!(result.status, 0, "{}", result.stderr);
    assert_eq!(result.stdout.trim(), "session alpha checkpoint 1");
    let result = run_cli(&store, &["list", "alpha"]);
    assert_eq!(result.status, 0, "{}", result.stderr);
    let lines: Vec<&str> = result.stdout.trim().lines().collect();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].split(' ').next().unwrap(), "1");
    let result = run_cli(&store, &["show", "alpha", "1"]);
    assert_eq!(result.status, 0, "{}", result.stderr);
    let shown: serde_json::Value = serde_json::from_str(&result.stdout).unwrap();
    assert_eq!(shown["env"], serde_json::json!({"MODE": "cli"}));
    let target = base.join("target");
    let result = run_cli(
        &store,
        &[
            "restore",
            "alpha",
            "1",
            "--target",
            target.to_str().unwrap(),
        ],
    );
    assert_eq!(result.status, 0, "{}", result.stderr);
    assert_eq!(std::fs::read(target.join("a.txt")).unwrap(), b"cli bytes");
    assert_eq!(
        std::fs::read(target.join("wayside.memory")).unwrap(),
        b"cli memory"
    );
}

#[test]
fn cli_reports_a_corrupt_manifest_without_a_traceback() {
    let base = temp_dir("cli-corrupt");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    let result = run_cli(
        &store,
        &["checkpoint", "alpha", "--state", state.to_str().unwrap()],
    );
    assert_eq!(result.status, 0);
    std::fs::write(manifest_path(&store, "alpha", 1), b"\xff\xfe broken").unwrap();
    let result = run_cli(&store, &["show", "alpha", "1"]);
    assert_eq!(result.status, 2);
    assert!(result.stderr.starts_with("error:"), "{}", result.stderr);
    assert!(!result.stderr.contains("Traceback"));
}

#[test]
fn cli_refuses_an_unsafe_session_id_without_a_traceback() {
    let base = temp_dir("cli-unsafe-id");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    let result = run_cli(
        &store,
        &[
            "checkpoint",
            "../escape",
            "--state",
            state.to_str().unwrap(),
        ],
    );
    assert_eq!(result.status, 2);
    assert!(result.stderr.starts_with("error:"), "{}", result.stderr);
    assert!(!result.stderr.contains("Traceback"));
    assert!(!base.join("escape").exists());
}

#[test]
fn checkpoint_refuses_a_symlinked_session_directory() {
    let base = temp_dir("write-link-session");
    let store = base.join("store");
    let outside = base.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::create_dir_all(store.join("sessions")).unwrap();
    std::os::unix::fs::symlink(&outside, store.join("sessions").join("ghost")).unwrap();
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    let error = expect_error(checkpoint(&store, "ghost", &state, None, b""));
    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
}

#[test]
fn reads_refuse_a_symlinked_session_directory() {
    let base = temp_dir("read-link-session");
    let store = base.join("store");
    let planted = base.join("planted");
    let state = make_state(&base.join("state"), &[("planted.txt", b"outside-bytes")]);
    checkpoint(&planted, "ghost", &state, None, b"").unwrap();
    std::fs::create_dir_all(store.join("sessions")).unwrap();
    std::os::unix::fs::symlink(
        planted.join("sessions").join("ghost"),
        store.join("sessions").join("ghost"),
    )
    .unwrap();
    let error = expect_error(list_checkpoints(&store, "ghost"));
    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
    let error = expect_error(show(&store, "ghost", 1));
    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
    let target = base.join("target");
    let error = expect_error(restore(&store, "ghost", 1, &target));
    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
    assert!(!target.exists());
}

#[test]
fn reads_and_writes_refuse_a_symlinked_checkpoints_directory() {
    let base = temp_dir("link-checkpoints");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    let ckpts = store.join("sessions").join("alpha").join("checkpoints");
    let moved = base.join("moved-checkpoints");
    std::fs::rename(&ckpts, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &ckpts).unwrap();
    let error = expect_error(list_checkpoints(&store, "alpha"));
    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
    let error = expect_error(checkpoint(&store, "alpha", &state, None, b""));
    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
    let mut names: Vec<String> = std::fs::read_dir(&moved)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["000001"]);
}

#[test]
fn reads_refuse_a_symlinked_sequence_directory() {
    let base = temp_dir("link-sequence");
    let store = base.join("store");
    let planted = base.join("planted");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    checkpoint(&planted, "alpha", &state, None, b"").unwrap();
    let link = checkpoint_dir(&store, "alpha", 2);
    std::fs::remove_dir_all(&link).unwrap();
    std::os::unix::fs::symlink(checkpoint_dir(&planted, "alpha", 1), &link).unwrap();
    let error = expect_error(show(&store, "alpha", 2));
    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
    let error = expect_error(list_checkpoints(&store, "alpha"));
    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
    let target = base.join("target");
    let error = expect_error(restore(&store, "alpha", 2, &target));
    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
    assert!(!target.exists());
}

#[test]
fn reads_refuse_a_symlinked_manifest() {
    let base = temp_dir("link-manifest");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    let path = manifest_path(&store, "alpha", 1);
    let copy = base.join("manifest-copy.json");
    std::fs::copy(&path, &copy).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&copy, &path).unwrap();
    let error = expect_error(show(&store, "alpha", 1));
    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
}

#[test]
fn restore_refuses_a_symlinked_payload() {
    let base = temp_dir("link-payload");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"same bytes")]);
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    let payload = checkpoint_dir(&store, "alpha", 1)
        .join("files")
        .join("a.txt");
    let twin = base.join("twin.txt");
    std::fs::write(&twin, b"same bytes").unwrap();
    std::fs::remove_file(&payload).unwrap();
    std::os::unix::fs::symlink(&twin, &payload).unwrap();
    let target = base.join("target");
    let error = expect_error(restore(&store, "alpha", 1, &target));
    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
    assert!(!target.exists());
}

fn reserved_recorded_name_case(tag: &str, reserved: &str) {
    let base = temp_dir(tag);
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"payload-bytes-here")]);
    let env = env_of(&[("K", "V")]);
    checkpoint(&store, "alpha", &state, Some(&env), b"the memory").unwrap();
    let ck = checkpoint_dir(&store, "alpha", 1);
    std::fs::rename(
        ck.join("files").join("a.txt"),
        ck.join("files").join(reserved),
    )
    .unwrap();
    damage_manifest_json(&store, "alpha", 1, &|value| {
        value["files"][0]["path"] = serde_json::json!(reserved);
    });
    let error = expect_error(show(&store, "alpha", 1));
    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
    let target = base.join("target");
    let error = expect_error(restore(&store, "alpha", 1, &target));
    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
    assert!(!target.exists());
}

#[test]
fn manifest_recording_a_reserved_name_is_refused_wayside_env() {
    reserved_recorded_name_case("reserved-env", "wayside.env");
}

#[test]
fn manifest_recording_a_reserved_name_is_refused_wayside_memory() {
    reserved_recorded_name_case("reserved-memory", "wayside.memory");
}

#[test]
fn a_nested_reserved_name_restores_as_a_payload() {
    let base = temp_dir("nested-reserved");
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[("sub/wayside.env", b"nested payload")],
    );
    let env = env_of(&[("K", "V")]);
    checkpoint(&store, "alpha", &state, Some(&env), b"").unwrap();
    let target = base.join("target");
    restore(&store, "alpha", 1, &target).unwrap();
    assert_eq!(
        std::fs::read(target.join("sub").join("wayside.env")).unwrap(),
        b"nested payload"
    );
    let env_text = std::fs::read_to_string(target.join("wayside.env")).unwrap();
    assert!(env_text.contains("K=V"));
}

#[test]
fn sequences_past_a_million_stay_visible() {
    let base = temp_dir("past-million");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    copy_tree(
        &checkpoint_dir(&store, "alpha", 1),
        &checkpoint_dir(&store, "alpha", 999999),
    );
    damage_manifest_json(&store, "alpha", 999999, &|value| {
        value["sequence"] = serde_json::json!(999999);
    });
    let first = checkpoint(&store, "alpha", &state, None, b"").unwrap();
    assert_eq!(first.sequence, 1000000);
    let second = checkpoint(&store, "alpha", &state, None, b"").unwrap();
    assert_eq!(second.sequence, 1000001);
    let listed: Vec<i64> = list_checkpoints(&store, "alpha")
        .unwrap()
        .iter()
        .map(|entry| entry.sequence)
        .collect();
    assert_eq!(listed, [1, 999999, 1000000, 1000001]);
    assert_eq!(show(&store, "alpha", 1000000).unwrap().sequence, 1000000);
}

#[test]
fn a_sequence_directory_without_a_manifest_is_corruption() {
    let base = temp_dir("manifestless");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    std::fs::create_dir_all(checkpoint_dir(&store, "alpha", 5)).unwrap();
    let error = expect_error(list_checkpoints(&store, "alpha"));
    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
    assert!(error.message().contains("manifest"), "{}", error.message());
    let error = expect_error(show(&store, "alpha", 5));
    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
    let error = expect_error(restore(&store, "alpha", 5, &base.join("target")));
    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
    let manifest = checkpoint(&store, "alpha", &state, None, b"").unwrap();
    assert_eq!(manifest.sequence, 6);
}

#[test]
fn sequence_allocation_at_the_top_of_the_range_still_assigns() {
    let base = temp_dir("seq-top");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    std::fs::create_dir_all(checkpoint_dir(&store, "alpha", i64::MAX - 1)).unwrap();
    let manifest = checkpoint(&store, "alpha", &state, None, b"").unwrap();
    assert_eq!(manifest.sequence, i64::MAX);
    assert!(checkpoint_dir(&store, "alpha", i64::MAX).is_dir());
    assert_eq!(show(&store, "alpha", i64::MAX).unwrap().sequence, i64::MAX);
}

#[test]
fn sequence_allocation_past_the_top_of_the_range_is_refused() {
    let base = temp_dir("seq-past-top");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    std::fs::create_dir_all(checkpoint_dir(&store, "alpha", i64::MAX)).unwrap();
    let error = expect_error(checkpoint(&store, "alpha", &state, None, b""));
    assert!(matches!(error, Error::Wayside(_)), "{error:?}");
    assert_eq!(error.exit_code(), 1);
    assert!(
        error
            .message()
            .contains("no sequence past it can be assigned"),
        "{}",
        error.message()
    );
    let mut names: Vec<String> =
        std::fs::read_dir(store.join("sessions").join("alpha").join("checkpoints"))
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
    names.sort();
    assert_eq!(names, ["000001", "9223372036854775807"]);
}

fn wrong_typed_value_case(tag: &str, damage: &dyn Fn(&mut serde_json::Value)) {
    let base = temp_dir(tag);
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    let env = env_of(&[("K", "V")]);
    checkpoint(&store, "alpha", &state, Some(&env), b"").unwrap();
    damage_manifest_json(&store, "alpha", 1, damage);
    let error = expect_error(show(&store, "alpha", 1));
    assert!(matches!(error, Error::Corrupt(_)), "{error:?}");
}

#[test]
fn manifest_with_a_wrong_typed_value_is_refused_size_as_string() {
    wrong_typed_value_case("type-size", &|value| {
        value["files"][0]["size"] = serde_json::json!("3");
    });
}

#[test]
fn manifest_with_a_wrong_typed_value_is_refused_env_as_list() {
    wrong_typed_value_case("type-env", &|value| {
        value["env"] = serde_json::json!(["K=V"]);
    });
}

#[test]
fn manifest_with_a_wrong_typed_value_is_refused_files_as_object() {
    wrong_typed_value_case("type-files", &|value| {
        value["files"] = serde_json::json!({});
    });
}

#[test]
fn manifest_with_a_wrong_typed_value_is_refused_sequence_as_string() {
    wrong_typed_value_case("type-sequence", &|value| {
        value["sequence"] = serde_json::json!("1");
    });
}

#[test]
fn manifest_with_a_wrong_typed_value_is_refused_created_at_not_text() {
    wrong_typed_value_case("type-created-num", &|value| {
        value["created_at"] = serde_json::json!(12345);
    });
}

#[test]
fn manifest_with_a_wrong_typed_value_is_refused_created_at_unparseable() {
    wrong_typed_value_case("type-created-text", &|value| {
        value["created_at"] = serde_json::json!("not a timestamp");
    });
}

#[test]
fn manifest_with_an_uppercase_z_zone_is_accepted() {
    let base = temp_dir("created-zulu");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    checkpoint(&store, "alpha", &state, None, b"").unwrap();
    damage_manifest_json(&store, "alpha", 1, &|value| {
        value["created_at"] = serde_json::json!("2026-11-02T10:00:00Z");
    });
    let manifest = show(&store, "alpha", 1).unwrap();
    assert_eq!(manifest.created_at, "2026-11-02T10:00:00Z");
}

#[test]
fn manifest_with_a_lowercase_z_zone_is_refused() {
    wrong_typed_value_case("created-lower-z", &|value| {
        value["created_at"] = serde_json::json!("2026-11-02T10:00:00z");
    });
}

#[test]
fn manifest_with_a_lowercase_z_zone_after_a_fraction_is_refused() {
    wrong_typed_value_case("created-lower-z-frac", &|value| {
        value["created_at"] = serde_json::json!("2026-11-02T10:00:00.5z");
    });
}

// A leap second count of 60 in the seconds field and a year of zero
// are two more shapes the Python reader refused that the RFC 3339
// parser underneath this port takes, found by the CodeRabbit pass on
// the repair head. Both read corrupt, exactly Python's class.

#[test]
fn manifest_with_a_leap_second_is_refused() {
    wrong_typed_value_case("created-leap-second", &|value| {
        value["created_at"] = serde_json::json!("2026-11-02T23:59:60+00:00");
    });
}

#[test]
fn manifest_with_a_year_of_zero_is_refused() {
    wrong_typed_value_case("created-year-zero", &|value| {
        value["created_at"] = serde_json::json!("0000-01-01T00:00:00+00:00");
    });
}

fn session_id_accepted_case(tag: &str, good_id: &str) {
    let base = temp_dir(tag);
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    let manifest = checkpoint(&store, good_id, &state, None, b"").unwrap();
    assert_eq!(manifest.session, good_id);
}

#[test]
fn session_id_accepting_edges_single_character() {
    session_id_accepted_case("id-single", "a");
}

#[test]
fn session_id_accepting_edges_exactly_64_characters() {
    session_id_accepted_case("id-64", &"x".repeat(64));
}

#[test]
fn session_id_accepting_edges_hyphen_underscore_digit() {
    session_id_accepted_case("id-mixed", "a-b_2");
}

#[test]
fn session_ids_ending_in_a_separator_are_refused_trailing_hyphen() {
    let base = temp_dir("id-trail-hyphen");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    let error = expect_error(checkpoint(&base.join("store"), "a-", &state, None, b""));
    assert!(matches!(error, Error::InvalidName(_)), "{error:?}");
}

#[test]
fn session_ids_ending_in_a_separator_are_refused_trailing_underscore() {
    let base = temp_dir("id-trail-underscore");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    let error = expect_error(checkpoint(&base.join("store"), "a_", &state, None, b""));
    assert!(matches!(error, Error::InvalidName(_)), "{error:?}");
}

#[test]
fn cli_memory_fifo_is_refused_promptly() {
    let base = temp_dir("cli-memory-fifo");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    let fifo = base.join("memory.fifo");
    let c_path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) }, 0);
    let result = run_cli_timed(
        &store,
        &[
            "checkpoint",
            "alpha",
            "--state",
            state.to_str().unwrap(),
            "--memory",
            fifo.to_str().unwrap(),
        ],
        Duration::from_secs(20),
    );
    assert_eq!(result.status, 1);
    assert!(result.stderr.starts_with("error:"), "{}", result.stderr);
    assert!(
        result.stderr.contains("not a regular file"),
        "{}",
        result.stderr
    );
    assert!(!result.stderr.contains("Traceback"));
}

#[test]
fn cli_memory_directory_and_missing_file_are_refused() {
    let base = temp_dir("cli-memory-shapes");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    let result = run_cli_timed(
        &store,
        &[
            "checkpoint",
            "alpha",
            "--state",
            state.to_str().unwrap(),
            "--memory",
            base.to_str().unwrap(),
        ],
        Duration::from_secs(20),
    );
    assert_eq!(result.status, 1);
    assert!(result.stderr.starts_with("error:"), "{}", result.stderr);
    assert!(!result.stderr.contains("Traceback"));
    let missing = base.join("no-memory-here");
    let result = run_cli_timed(
        &store,
        &[
            "checkpoint",
            "alpha",
            "--state",
            state.to_str().unwrap(),
            "--memory",
            missing.to_str().unwrap(),
        ],
        Duration::from_secs(20),
    );
    assert_eq!(result.status, 1);
    assert!(result.stderr.starts_with("error:"), "{}", result.stderr);
    assert!(!result.stderr.contains("Traceback"));
}

#[test]
fn cli_sequence_errors_share_the_invalid_name_exit_code() {
    let base = temp_dir("cli-sequence");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    let result = run_cli(
        &store,
        &["checkpoint", "alpha", "--state", state.to_str().unwrap()],
    );
    assert_eq!(result.status, 0);
    let target = base.join("target");
    for bad in ["abc", "0"] {
        let result = run_cli(
            &store,
            &[
                "restore",
                "alpha",
                bad,
                "--target",
                target.to_str().unwrap(),
            ],
        );
        assert_eq!(result.status, 2);
        assert!(result.stderr.starts_with("error:"), "{}", result.stderr);
        assert!(
            result.stderr.contains("invalid sequence"),
            "{}",
            result.stderr
        );
    }
    assert!(!target.exists());
}

#[test]
fn checkpoint_fails_when_a_state_subdirectory_cannot_be_listed() {
    let base = temp_dir("walk-sub-blocked");
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[("sub/inner.txt", b"inner"), ("top.txt", b"top")],
    );
    let blocked = state.join("sub");
    let reader = StateTreeReader::failing_at(&blocked);
    let error = expect_error(checkpoint_with_reader(
        &store, "alpha", &state, None, b"", &reader,
    ));
    assert!(matches!(error, Error::Wayside(_)), "{error:?}");
    assert!(
        error
            .message()
            .contains(&blocked.to_string_lossy().into_owned()),
        "{}",
        error.message()
    );
    assert!(
        error.message().contains("cannot be listed"),
        "{}",
        error.message()
    );
    assert!(!checkpoint_dir(&store, "alpha", 1).exists());
}

#[test]
fn checkpoint_fails_when_the_state_directory_cannot_be_listed() {
    let base = temp_dir("walk-root-blocked");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("top.txt", b"top")]);
    let blocked = state.clone();
    let reader = StateTreeReader::failing_at(&blocked);
    let error = expect_error(checkpoint_with_reader(
        &store, "alpha", &state, None, b"", &reader,
    ));
    assert!(matches!(error, Error::Wayside(_)), "{error:?}");
    assert!(
        error
            .message()
            .contains(&blocked.to_string_lossy().into_owned()),
        "{}",
        error.message()
    );
    assert!(
        error.message().contains("cannot be listed"),
        "{}",
        error.message()
    );
    assert!(!checkpoint_dir(&store, "alpha", 1).exists());
}

#[test]
fn cli_memory_symlink_is_refused_with_its_own_message() {
    let base = temp_dir("cli-memory-link");
    let store = base.join("store");
    let state = make_state(&base.join("state"), &[("a.txt", b"a")]);
    let real = base.join("memory.txt");
    std::fs::write(&real, b"memory").unwrap();
    let link = base.join("memory-link.txt");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let result = run_cli(
        &store,
        &[
            "checkpoint",
            "alpha",
            "--state",
            state.to_str().unwrap(),
            "--memory",
            link.to_str().unwrap(),
        ],
    );
    assert_eq!(result.status, 1);
    assert!(result.stderr.starts_with("error:"), "{}", result.stderr);
    assert!(
        result.stderr.contains("is a symbolic link"),
        "{}",
        result.stderr
    );
    assert!(
        !result.stderr.contains("not a regular file"),
        "{}",
        result.stderr
    );
    assert!(!result.stderr.contains("Traceback"));
    assert!(!checkpoint_dir(&store, "alpha", 1).exists());
}

// The fixture equivalence tests and the manifest byte identity
// pin, reported separately from the translated count above,
// as the mayfly port reported its drive battery separately.
// Each fixture store was written by the Python implementation
// and frozen with its recorded truths under tests/fixtures.

fn fixture_root(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
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

fn fixture_case(name: &str, sessions: &[(&str, &[i64])], grow_session: &str, next_sequence: i64) {
    let fixture = fixture_root(name);
    let base = temp_dir(&format!("fixture-{name}"));
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
        assert_eq!(bytes.len(), size, "{name} inventory size for {rel}");
        assert_eq!(sha256_hex(&bytes), hash, "{name} inventory hash for {rel}");
    }

    for (session, sequences) in sessions {
        let listed = run_cli(&store, &["list", session]);
        assert_eq!(listed.status, 0, "{}", listed.stderr);
        let expected_list =
            std::fs::read_to_string(fixture.join(format!("expected-list-{session}.txt"))).unwrap();
        assert_eq!(listed.stdout, expected_list, "{name} list for {session}");
        for sequence in *sequences {
            let shown = run_cli(&store, &["show", session, &sequence.to_string()]);
            assert_eq!(shown.status, 0, "{}", shown.stderr);
            let expected_show = std::fs::read_to_string(
                fixture.join(format!("expected-show-{session}-{sequence}.txt")),
            )
            .unwrap();
            assert_eq!(
                shown.stdout, expected_show,
                "{name} show for {session} {sequence}"
            );
            let target = base.join(format!("restore-{session}-{sequence}"));
            let restored = run_cli(
                &store,
                &[
                    "restore",
                    session,
                    &sequence.to_string(),
                    "--target",
                    target.to_str().unwrap(),
                ],
            );
            assert_eq!(restored.status, 0, "{}", restored.stderr);
            let expected_tree = tree_bytes(
                &fixture
                    .join("expected-restore")
                    .join(format!("{session}-{sequence}")),
            );
            assert_eq!(
                tree_bytes(&target),
                expected_tree,
                "{name} restore tree for {session} {sequence}"
            );
        }
    }

    // A checkpoint the Rust code writes into a copy of the
    // frozen store takes the next sequence and reads back.
    let state = make_state(&base.join("fresh-state"), &[("fresh.txt", b"fresh")]);
    let manifest = checkpoint(&store, grow_session, &state, None, b"").unwrap();
    assert_eq!(manifest.sequence, next_sequence);
    let read_back = show(&store, grow_session, next_sequence).unwrap();
    assert_eq!(read_back, manifest);
}

#[test]
fn fixture_single_small_tree() {
    fixture_case("single-small-tree", &[("alpha", &[1])], "alpha", 2);
}

#[test]
fn fixture_sequence_of_three() {
    fixture_case("sequence-of-three", &[("alpha", &[1, 2, 3])], "alpha", 4);
}

#[test]
fn fixture_forked_session() {
    fixture_case(
        "forked-session",
        &[("alpha", &[1, 2]), ("beta", &[1])],
        "alpha",
        3,
    );
}

#[test]
fn fixture_env_and_memory() {
    fixture_case("env-and-memory", &[("alpha", &[1])], "alpha", 2);
}

#[test]
fn fixture_nested_and_empty() {
    fixture_case("nested-and-empty", &[("alpha", &[1])], "alpha", 2);
}

#[test]
fn manifest_bytes_match_the_python_layout_for_non_ascii_content() {
    let base = temp_dir("byte-identity");
    let store = base.join("store");
    let state = make_state(
        &base.join("state"),
        &[("café.txt", "café bytes".as_bytes())],
    );
    let env = env_of(&[("ACCENT", "café")]);
    let manifest = checkpoint(&store, "alpha", &state, Some(&env), b"").unwrap();
    let stored = std::fs::read_to_string(manifest_path(&store, "alpha", 1)).unwrap();
    let payload_sha = sha256_hex("café bytes".as_bytes());
    let payload_size = "café bytes".len();
    let expected = format!(
        "{{\n  \"created_at\": \"{}\",\n  \"env\": {{\n    \"ACCENT\": \"caf\\u00e9\"\n  }},\n  \"files\": [\n    {{\n      \"path\": \"caf\\u00e9.txt\",\n      \"sha256\": \"{payload_sha}\",\n      \"size\": {payload_size}\n    }}\n  ],\n  \"format\": \"wayside-checkpoint/1\",\n  \"memory\": {{\n    \"sha256\": \"{EMPTY_SHA256}\",\n    \"size\": 0\n  }},\n  \"sequence\": 1,\n  \"session\": \"alpha\"\n}}\n",
        manifest.created_at
    );
    assert_eq!(stored, expected);
    // The rendered show text is the stored text but for the
    // trailing newline both forms carry.
    assert_eq!(format!("{}\n", wayside::render_manifest(&manifest)), stored);
}
