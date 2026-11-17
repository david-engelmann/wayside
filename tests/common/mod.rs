//! The shared test harness of the wayside suites (WS-A24).
//!
//! Every suite includes this module and calls the helpers
//! here; no suite keeps a copy of its own. What stands here
//! is the mechanism the pins share: the temporary directory
//! shape, the digest rendering, the state staging, the
//! manifest damage tools, the tree walks the comparisons
//! judge by, and the command line runner. Helpers only one
//! suite uses stay in that suite.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use sha2::{Digest, Sha256};
use wayside::Error;

pub const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

static COUNTER: AtomicUsize = AtomicUsize::new(0);

pub fn temp_dir(prefix: &str, tag: &str) -> PathBuf {
    let count = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "{prefix}-{}-{tag}-{count}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn make_state(base: &Path, files: &[(&str, &[u8])]) -> PathBuf {
    let state = base.to_path_buf();
    std::fs::create_dir_all(&state).unwrap();
    for (rel, data) in files {
        let path = state.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, data).unwrap();
    }
    state
}

pub fn checkpoint_dir(store: &Path, session: &str, sequence: i64) -> PathBuf {
    store
        .join("sessions")
        .join(session)
        .join("checkpoints")
        .join(format!("{sequence:06}"))
}

pub fn manifest_path(store: &Path, session: &str, sequence: i64) -> PathBuf {
    checkpoint_dir(store, session, sequence).join("manifest.json")
}

pub fn expect_error<T>(result: wayside::Result<T>) -> Error {
    match result {
        Ok(_) => panic!("the operation succeeded where the pin requires a refusal"),
        Err(error) => error,
    }
}

pub fn declare(paths: &[&str]) -> Vec<String> {
    paths.iter().map(|path| path.to_string()).collect()
}

pub fn damage_manifest_json(
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

pub fn damage_manifest_text(
    store: &Path,
    session: &str,
    sequence: i64,
    damage: &dyn Fn(&str) -> String,
) {
    let path = manifest_path(store, session, sequence);
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, damage(&text)).unwrap();
}

pub fn copy_tree(from: &Path, to: &Path) {
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

pub fn hash_tree(root: &Path) -> BTreeMap<String, String> {
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

pub fn tree_bytes(root: &Path) -> BTreeMap<String, Vec<u8>> {
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

pub struct CliOutput {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

pub fn run_cli(store: &Path, args: &[&str]) -> CliOutput {
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
