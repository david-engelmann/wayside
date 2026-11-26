//! The setup pins, translated from the Python setup suite the repo
//! was seeded with, brought up to the current fleet standard. Each
//! test names the same fact its Python original named, with the
//! strengthenings the fleet folded in after wayside's Round 1
//! suite was written, three of them landing with this port: the
//! job level if false pin, the wrong arc label pin, and the CI
//! credential hygiene pin. Two facts changed with the Rust
//! conversion and the
//! change is stated where it lands. The CI gate commands are the
//! Rust gate commands of amended obligation S2, and the contract
//! substance pin asserts the amended S2 phrases.

use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};
mod common;

fn temp_dir(tag: &str) -> PathBuf {
    common::temp_dir("wayside-setup-test", tag)
}

const MARKER: &str = "<!-- repo annex below this line -->";
const GATE_COMMAND: &str = "cargo test";
const SCAN_COMMAND: &str = "cargo run --bin codepoint_scan";
const FMT_COMMAND: &str = "cargo fmt --check";
const CLIPPY_COMMAND: &str = "cargo clippy --all-targets -- -D warnings";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(name: &str) -> String {
    std::fs::read_to_string(root().join(name)).unwrap()
}

#[test]
fn seed_files_exist() {
    for name in ["README.md", "LICENSE", ".gitignore"] {
        assert!(root().join(name).is_file(), "{name}");
    }
}

#[test]
fn license_starts_with_mit_heading() {
    assert!(read("LICENSE").starts_with("MIT License\n"));
}

#[test]
fn agents_has_marker_and_wayside_annex_below_it() {
    let text = read("AGENTS.md");
    assert!(text.contains(MARKER));
    let annex = text.split(MARKER).nth(1).unwrap();
    assert!(annex.contains("wayside"));
    assert!(annex.contains("CONTRACT.md"));
    assert!(annex.contains("remembers where everything was"));
}

#[test]
fn agents_canonical_part_carries_program_headings() {
    let text = read("AGENTS.md");
    let canonical = text.split(MARKER).next().unwrap();
    assert!(canonical.starts_with("# AGENTS.md\n"));
    assert!(canonical.contains("## The round discipline"));
    assert!(canonical.contains("## Contract first"));
    assert!(canonical.contains("39 repo storefront perfection program"));
}

#[test]
fn contract_names_five_setup_obligations() {
    let text = read("CONTRACT.md");
    for name in ["S1", "S2", "S3", "S4", "S5"] {
        assert!(
            text.contains(&format!("Obligation {name} covers")),
            "{name}"
        );
    }
    assert!(!text.contains("Obligation S6"));
}

fn status_section(text: &str) -> &str {
    let start = text.find("\n## Status\n").unwrap() + "\n## Status\n".len();
    let rest = &text[start..];
    match rest.find("\n## ") {
        Some(end) => &rest[..end],
        None => rest,
    }
}

#[test]
fn readme_status_states_one_hundred_of_one_hundred() {
    let text = read("README.md");
    assert!(text.contains("## Status"));
    let status = status_section(&text);
    // The full sentence, never the bare count, because the
    // bare count is a substring of every two digit count that
    // ends in the same digit and a pin on it would pass over
    // a wrong statement of the closed rounds.
    assert!(status.contains("Closed perfection rounds are 100 of 100."));
    assert!(status.contains("a9bdc896322b1787d0719d6a76baaf1d91ef8860"));
}

fn flush_step(step_lines: &mut Vec<String>, commands: &mut Vec<String>) {
    if step_lines.is_empty() {
        return;
    }
    let mut disabled = false;
    let mut runs = Vec::new();
    for stripped in step_lines.iter() {
        if let Some(value) = stripped.strip_prefix("if:") {
            let value = value
                .trim()
                .trim_matches(|c| c == '\'' || c == '"')
                .to_lowercase();
            if value == "false" {
                disabled = true;
            }
        }
        if let Some(pos) = stripped.find("run:") {
            runs.push(stripped[pos + "run:".len()..].trim().to_string());
        }
    }
    if !disabled {
        commands.extend(runs);
    }
    step_lines.clear();
}

fn run_commands(ci_text: &str) -> Vec<String> {
    let mut commands = Vec::new();
    let mut step_lines: Vec<String> = Vec::new();
    let mut in_steps = false;
    let mut in_test_job = false;
    let mut job_disabled = false;
    for line in ci_text.lines() {
        let stripped = line.trim();
        if stripped.starts_with('#') {
            continue;
        }
        if stripped == "test:" {
            in_test_job = true;
            continue;
        }
        if in_test_job && !in_steps && stripped.starts_with("if:") {
            let value = stripped
                .split_once(':')
                .map(|(_, rest)| rest)
                .unwrap_or_default()
                .trim()
                .trim_matches(|c| c == '\'' || c == '"')
                .to_lowercase();
            if value == "false" {
                job_disabled = true;
            }
            continue;
        }
        if stripped == "steps:" {
            in_steps = true;
            continue;
        }
        if in_steps && stripped.starts_with("- ") {
            flush_step(&mut step_lines, &mut commands);
            step_lines.push(stripped[2..].trim().to_string());
            continue;
        }
        if in_steps {
            step_lines.push(stripped.to_string());
        }
    }
    flush_step(&mut step_lines, &mut commands);
    if job_disabled {
        return Vec::new();
    }
    commands
}

#[test]
fn ci_runs_the_local_gate_commands() {
    let commands = run_commands(&read(".github/workflows/ci.yml"));
    assert!(commands.iter().any(|c| c == GATE_COMMAND));
    assert!(commands.iter().any(|c| c == SCAN_COMMAND));
    assert!(commands.iter().any(|c| c == FMT_COMMAND));
    assert!(commands.iter().any(|c| c == CLIPPY_COMMAND));
}

#[test]
fn ci_carries_the_fleet_credential_hygiene() {
    let text = read(".github/workflows/ci.yml");
    // The workflow asks for no more than it uses. Its token
    // is limited to reading the repository, the checkout
    // leaves no credential behind for later steps, and the
    // toolchain action is pinned to a full commit SHA rather
    // than a moving branch name.
    assert!(text.contains("permissions:"));
    assert!(text.contains("contents: read"));
    // No permission line in the workflow carries a write
    // value on any scope, and no write-all shorthand stands:
    // a write grant beside the read grant, a job level
    // permissions block, or a further scope at write fails
    // this pin (WS-P74).
    assert!(!text.contains("write-all"));
    for line in text.lines() {
        let stripped = line.trim();
        assert!(
            !stripped.ends_with(": write"),
            "a permission line carries a write value: {stripped}"
        );
    }
    // Every checkout step stands judged in its own block, not
    // by one whole-text presence: the workflow carries two
    // checkout steps, one per job, and dropping the setting
    // from either, or flipping either to true while the other
    // stands, fails this pin (WS-P73).
    assert!(!text.contains("persist-credentials: true"));
    let checkout_blocks: Vec<&str> = text.split("- uses: actions/checkout@").skip(1).collect();
    assert_eq!(
        checkout_blocks.len(),
        2,
        "the workflow carries two checkout steps"
    );
    for block in checkout_blocks {
        let block = block.split("\n      - ").next().unwrap();
        assert!(
            block.contains("persist-credentials: false"),
            "a checkout step does not stand with persist-credentials: false"
        );
    }
    assert!(!text.contains("rust-toolchain@master"));
    // Every toolchain step stands judged, not only the first
    // one the file finds: the workflow carries two, one per
    // job, and a shortened pin on either fails this pin
    // (WS-A12).
    let toolchain_lines: Vec<&str> = text
        .lines()
        .filter(|line| line.contains("dtolnay/rust-toolchain@"))
        .collect();
    assert!(
        !toolchain_lines.is_empty(),
        "the workflow carries no toolchain step"
    );
    for toolchain_line in toolchain_lines {
        let pin = toolchain_line
            .split("dtolnay/rust-toolchain@")
            .nth(1)
            .unwrap()
            .trim();
        assert_eq!(pin.len(), 40, "{}", toolchain_line);
        assert!(pin
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
    }
}

#[test]
fn ci_gate_parser_skips_a_step_disabled_with_if_false() {
    let ci_text = read(".github/workflows/ci.yml");
    let gate_step = "      - name: Run the local gate\n        run: cargo test";
    assert!(ci_text.contains(gate_step));
    for flag in ["false", "\"false\""] {
        let disabled = ci_text.replace(
            gate_step,
            &format!(
                "      - name: Run the local gate\n        if: {flag}\n        run: cargo test"
            ),
        );
        let commands = run_commands(&disabled);
        assert!(!commands.iter().any(|c| c == GATE_COMMAND));
        assert!(commands.iter().any(|c| c == SCAN_COMMAND));
    }
}

#[test]
fn ci_gate_parser_rejects_a_job_disabled_with_if_false() {
    let ci_text = read(".github/workflows/ci.yml");
    let job_head = "  test:\n    runs-on: ubuntu-latest";
    assert!(ci_text.contains(job_head));
    for flag in ["false", "\"false\""] {
        let disabled = ci_text.replace(
            job_head,
            &format!("  test:\n    if: {flag}\n    runs-on: ubuntu-latest"),
        );
        let commands = run_commands(&disabled);
        assert!(!commands.iter().any(|c| c == GATE_COMMAND));
        assert!(!commands.iter().any(|c| c == SCAN_COMMAND));
        assert!(!commands.iter().any(|c| c == FMT_COMMAND));
        assert!(!commands.iter().any(|c| c == CLIPPY_COMMAND));
    }
    let commands = run_commands(&ci_text);
    assert!(commands.iter().any(|c| c == GATE_COMMAND));
    assert!(commands.iter().any(|c| c == SCAN_COMMAND));
    assert!(commands.iter().any(|c| c == FMT_COMMAND));
    assert!(commands.iter().any(|c| c == CLIPPY_COMMAND));
}

fn binding_smoke_command_stands(ci_text: &str) -> bool {
    let Some(block) = ci_text.split("\n  binding:\n").nth(1) else {
        return false;
    };
    let mut job_disabled = false;
    let mut in_steps = false;
    let mut carries_smoke = false;
    for line in block.lines() {
        let stripped = line.trim();
        if stripped.starts_with('#') {
            continue;
        }
        if !in_steps && stripped.starts_with("if:") {
            let value = stripped
                .split_once(':')
                .map(|(_, rest)| rest)
                .unwrap_or_default()
                .trim()
                .trim_matches(|c| c == '\'' || c == '"')
                .to_lowercase();
            if value == "false" {
                job_disabled = true;
            }
            continue;
        }
        if stripped == "steps:" {
            in_steps = true;
            continue;
        }
        if in_steps && stripped == "python -m pytest tests/test_binding_smoke.py" {
            carries_smoke = true;
        }
    }
    carries_smoke && !job_disabled
}

#[test]
fn ci_carries_the_binding_job_undisabled_with_its_smoke_command() {
    let ci_text = read(".github/workflows/ci.yml");
    let job_head = "  binding:\n    runs-on: ubuntu-latest";
    assert!(ci_text.contains(job_head));
    assert!(binding_smoke_command_stands(&ci_text));
    for flag in ["false", "\"false\""] {
        let disabled = ci_text.replace(
            job_head,
            &format!("  binding:\n    if: {flag}\n    runs-on: ubuntu-latest"),
        );
        assert!(!binding_smoke_command_stands(&disabled));
    }
}

struct ScanOutput {
    status: i32,
    stdout: String,
    stderr: String,
}

fn scan_tree(root: &Path) -> ScanOutput {
    let output = Command::new(env!("CARGO_BIN_EXE_codepoint_scan"))
        .arg(root)
        .output()
        .unwrap();
    ScanOutput {
        status: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

#[test]
fn codepoint_scan_reports_zero_for_the_repo() {
    let result = scan_tree(&root());
    assert_eq!(result.status, 0, "{}", result.stdout);
    assert!(result.stdout.contains("em dash U+2014 count 0"));
    assert!(result.stdout.contains("en dash U+2013 count 0"));
}

const CANONICAL_ABOVE_MARKER_SHA256: &str =
    "bb3e2fc1790b13729da3495c3f0902b968577f4e45ceb27e48e90b489f36e1a7";

#[test]
fn agents_canonical_part_matches_pinned_digest() {
    let agents = std::fs::read(root().join("AGENTS.md")).unwrap();
    let marker: &[u8] = b"<!-- repo annex below this line -->";
    let position = agents
        .windows(marker.len())
        .position(|window| window == marker)
        .unwrap();
    let mut hasher = Sha256::new();
    hasher.update(&agents[..position]);
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(digest, CANONICAL_ABOVE_MARKER_SHA256);
}

#[test]
fn readme_has_exactly_one_status_section() {
    let text = read("README.md");
    assert_eq!(text.matches("\n## Status\n").count(), 1);
}

#[test]
fn readme_carries_no_wrong_arc_label() {
    assert!(!read("README.md").contains("Fleet Operations"));
}

#[test]
fn codepoint_scan_counts_an_em_dash() {
    let dir = temp_dir("em-dash");
    std::fs::write(dir.join("note.md"), "a\u{2014}b").unwrap();
    let result = scan_tree(&dir);
    assert_eq!(result.status, 1);
    assert!(result.stdout.contains("em dash U+2014 count 1"));
    assert!(result.stdout.contains("note.md"));
}

#[test]
fn codepoint_scan_counts_an_en_dash() {
    let dir = temp_dir("en-dash");
    std::fs::write(dir.join("note.md"), "a\u{2013}b").unwrap();
    let result = scan_tree(&dir);
    assert_eq!(result.status, 1);
    assert!(result.stdout.contains("en dash U+2013 count 1"));
}

// WS-P62: the per file report and the independence of the
// two tallies stand pinned by the whole output. A scanner
// whose per file line swapped the two counts, printed the
// tree's totals in the file's place, or tallied each dash
// into both counts passes the standing count pins and
// fails this one.

#[test]
fn codepoint_scan_reports_each_files_counts_and_the_totals() {
    let dir = temp_dir("per-file-counts");
    std::fs::write(dir.join("alpha.md"), "a\u{2014}b\u{2014}c\u{2013}d").unwrap();
    std::fs::write(dir.join("beta.md"), "x\u{2014}y").unwrap();
    let result = scan_tree(&dir);
    assert_eq!(result.status, 1);
    let expected = format!(
        "{}: em dash 2, en dash 1\n{}: em dash 1, en dash 0\nem dash U+2014 count 3\nen dash U+2013 count 1\n",
        dir.join("alpha.md").display(),
        dir.join("beta.md").display()
    );
    assert_eq!(result.stdout, expected);
}

#[test]
fn codepoint_scan_scans_a_root_under_an_excluded_ancestor() {
    let dir = temp_dir("excluded-ancestor").join("venv").join("tree");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("note.md"), "a\u{2014}b").unwrap();
    let result = scan_tree(&dir);
    assert_eq!(result.status, 1);
    assert!(result.stdout.contains("em dash U+2014 count 1"));
}

#[test]
fn codepoint_scan_skips_excluded_dirs_below_the_root() {
    let dir = temp_dir("excluded-dirs");
    let cache = dir.join("build");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache.join("note.md"), "a\u{2014}b").unwrap();
    std::fs::write(dir.join("clean.md"), "clean").unwrap();
    let result = scan_tree(&dir);
    assert_eq!(result.status, 0);
    assert!(result.stdout.contains("em dash U+2014 count 0"));
}

#[test]
fn codepoint_scan_skips_every_excluded_dir_below_the_root() {
    let dir = temp_dir("excluded-dirs-all");
    std::fs::write(dir.join("clean.md"), "clean").unwrap();
    for name in [
        ".git",
        "__pycache__",
        ".pytest_cache",
        ".venv",
        "venv",
        "node_modules",
        "build",
        "dist",
        "target",
    ] {
        let excluded = dir.join(name);
        std::fs::create_dir_all(&excluded).unwrap();
        std::fs::write(excluded.join("note.md"), "a\u{2014}b").unwrap();
    }
    let result = scan_tree(&dir);
    assert_eq!(result.status, 0, "{}", result.stdout);
    assert!(result.stdout.contains("em dash U+2014 count 0"));
    assert!(result.stdout.contains("en dash U+2013 count 0"));

    std::fs::rename(dir.join("target"), dir.join("targets")).unwrap();
    let result = scan_tree(&dir);
    assert_eq!(result.status, 1);
    assert!(result.stdout.contains("em dash U+2014 count 1"));
}

#[test]
fn contract_obligations_carry_their_substance() {
    let text = read("CONTRACT.md");
    for phrase in [
        "present from the first commit",
        "every later change lands through a pull request",
        "no paid service",
        "cargo test plus the codepoint scan run through cargo",
        "0 of 100 until a full cycle",
        "no file carries a real trace or a real transcript or a real name",
        "SHA-256 digest of the part above the marked line",
    ] {
        assert!(text.contains(phrase), "{phrase}");
    }
}

#[test]
fn codepoint_scan_fails_loudly_on_undecodable_input() {
    let dir = temp_dir("undecodable");
    std::fs::write(dir.join("note.md"), b"\xff\xfe not utf-8").unwrap();
    let result = scan_tree(&dir);
    assert_eq!(result.status, 2);
    assert!(result.stderr.contains("note.md"));
}

#[test]
fn codepoint_scan_counts_a_dash_in_a_file_with_no_suffix() {
    let dir = temp_dir("no-suffix");
    std::fs::write(dir.join("LICENSE"), "a\u{2014}b").unwrap();
    let result = scan_tree(&dir);
    assert_eq!(result.status, 1);
    assert!(result.stdout.contains("em dash U+2014 count 1"));
}

#[test]
fn codepoint_scan_counts_a_dash_in_an_uppercase_suffix_file() {
    let dir = temp_dir("upper-suffix");
    std::fs::write(dir.join("UPPER.MD"), "a\u{2014}b").unwrap();
    let result = scan_tree(&dir);
    assert_eq!(result.status, 1);
    assert!(result.stdout.contains("em dash U+2014 count 1"));
}

#[test]
fn codepoint_scan_skips_a_file_with_an_uppercase_binary_suffix() {
    let dir = temp_dir("upper-binary");
    std::fs::write(dir.join("FAKE.PNG"), "a\u{2014}b").unwrap();
    let result = scan_tree(&dir);
    assert_eq!(result.status, 0);
    assert!(result.stdout.contains("em dash U+2014 count 0"));
    assert!(result.stdout.contains("en dash U+2013 count 0"));
}

#[test]
fn codepoint_scan_skips_every_binary_suffix() {
    let dir = temp_dir("binary-suffixes-all");
    for suffix in [
        "png", "jpg", "jpeg", "gif", "ico", "webp", "pdf", "zip", "gz", "tar", "whl", "so",
        "dylib", "bin",
    ] {
        std::fs::write(dir.join(format!("payload.{suffix}")), "a\u{2014}b").unwrap();
    }
    let result = scan_tree(&dir);
    assert_eq!(result.status, 0, "{}", result.stdout);
    assert!(result.stdout.contains("em dash U+2014 count 0"));
    assert!(result.stdout.contains("en dash U+2013 count 0"));

    let control = temp_dir("binary-suffixes-all-control");
    std::fs::write(control.join("payload.txt"), "a\u{2014}b").unwrap();
    let result = scan_tree(&control);
    assert_eq!(result.status, 1);
    assert!(result.stdout.contains("em dash U+2014 count 1"));
}

#[cfg(unix)]
#[test]
fn codepoint_scan_passes_over_a_pipe_instead_of_reading_it() {
    let dir = temp_dir("pipe");
    std::fs::write(dir.join("clean.md"), "clean").unwrap();
    let pipe = dir.join("pipe");
    let status = Command::new("mkfifo").arg(&pipe).status().unwrap();
    assert!(status.success());
    // A scan that collected the pipe as a file would block on
    // reading it and never return, so the scan completing at
    // all is the pin; it reports the clean file only.
    let result = scan_tree(&dir);
    assert_eq!(result.status, 0, "{}", result.stderr);
    assert!(result.stdout.contains("em dash U+2014 count 0"));
    assert!(result.stdout.contains("en dash U+2013 count 0"));
}

/// WS-A15: the scanner takes its root as a path, never as
/// text, so a root whose name is not valid Unicode scans
/// like any other. On the code this pin was written
/// against, the root was taken through std::env::args and
/// the scanner panicked on this root before scanning.
#[cfg(unix)]
#[test]
fn codepoint_scan_takes_a_root_whose_name_is_not_unicode() {
    use std::os::unix::ffi::OsStrExt;

    let base = temp_dir("non-unicode-root");
    let root = base.join(std::ffi::OsStr::from_bytes(b"scan-\xff-root"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("clean.md"), "clean").unwrap();
    let result = scan_tree(&root);
    assert_eq!(result.status, 0, "{}", result.stderr);
    assert!(result.stdout.contains("em dash U+2014 count 0"));
    assert!(result.stdout.contains("en dash U+2013 count 0"));
    assert!(!result.stderr.contains("panicked"), "{}", result.stderr);
}

// WS-P63: the walk's coverage of a file whose own name is
// not valid Unicode stands pinned inside the tree. The
// root pin above stages the non Unicode name at the scan
// root; this pin stages it on a file entry, the seat a
// walk that rendered each file's name as text and passed
// over the names that fail to render would skip, reporting
// zero at exit 0 where this pin requires the dash counted.

#[cfg(unix)]
#[test]
fn codepoint_scan_scans_a_file_whose_name_is_not_unicode() {
    use std::os::unix::ffi::OsStrExt;

    let dir = temp_dir("non-unicode-file");
    let name = std::ffi::OsStr::from_bytes(b"bad-\xff-name.md");
    std::fs::write(dir.join(name), "a\u{2014}b").unwrap();
    std::fs::write(dir.join("clean.md"), "clean").unwrap();
    let result = scan_tree(&dir);
    assert_eq!(result.status, 1);
    assert!(result.stdout.contains("em dash U+2014 count 1"));
}

/// WS-A53: an argument past the root is a scan error at
/// exit 2, refused before any scan. On the code this pin
/// was written against, the scanner took its root from
/// the first argument and never examined the rest, so a
/// call naming two roots scanned the first and reported
/// its counts as if the call had named one root only;
/// here the first root holds an em dash, so the standing
/// code exits 1 on the dash it finds.
#[test]
fn codepoint_scan_refuses_an_argument_past_the_root() {
    let dir = temp_dir("extra-argument");
    std::fs::write(dir.join("note.md"), "a\u{2014}b").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_codepoint_scan"))
        .arg(&dir)
        .arg("a-second-root")
        .output()
        .unwrap();
    assert_eq!(output.status.code().unwrap_or(-1), 2);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(stderr.contains("scan error"), "{stderr}");
    assert!(stderr.contains("one root"), "{stderr}");
    assert!(
        output.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// WS-A26: a scan whose output cannot be written is a scan
/// error at exit 2, never a panic. The pin points the
/// scanner's standard output at /dev/full, whose every write
/// fails. On the code this pin was written against, the count
/// prints panicked and the process died at exit 101.
#[cfg(unix)]
#[test]
fn codepoint_scan_with_an_unwritable_output_is_a_scan_error() {
    use std::io::Read;

    let dir = temp_dir("full-output");
    std::fs::write(dir.join("clean.md"), "clean").unwrap();
    let full = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_codepoint_scan"))
        .arg(&dir)
        .stdout(std::process::Stdio::from(full))
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    let status = child.wait().unwrap();
    assert_eq!(status.code().unwrap_or(-1), 2, "{stderr}");
    assert!(stderr.contains("scan error"), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
}

/// WS-A26, the both streams shape: with stderr also
/// redirected to /dev/full, the scan error line cannot be
/// written, and the exit code still carries the failure.
/// On the code this pin was written against, eprintln!
/// panicked on the failed stderr write and the scanner
/// exited 101.
#[cfg(unix)]
#[test]
fn codepoint_scan_with_both_streams_unwritable_keeps_exit_2() {
    use std::process::Stdio;

    let root = temp_dir("scan-both-full");
    std::fs::write(root.join("a.txt"), "plain\n").unwrap();
    let out_full = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .unwrap();
    let err_full = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .unwrap();
    let status = Command::new(env!("CARGO_BIN_EXE_codepoint_scan"))
        .arg(&root)
        .stdout(Stdio::from(out_full))
        .stderr(Stdio::from(err_full))
        .status()
        .unwrap();
    assert_eq!(status.code().unwrap_or(-1), 2);
}

// The port's deliberate divergence: a scan root that does not
// stand is a scan error at exit 2, where the setup scanner this
// binary ports exited 0 with zero counts. The root here is
// never created, so the refusal names the root itself.
#[test]
fn codepoint_scan_refuses_a_root_that_does_not_stand() {
    let root = temp_dir("scan-missing-root").join("no-such-tree");
    let result = scan_tree(&root);
    assert_eq!(result.status, 2);
    assert!(result.stderr.contains("scan error"), "{}", result.stderr);
    assert!(result.stderr.contains("no-such-tree"), "{}", result.stderr);
}

/// WS-A73: a symlink to a directory below the scan root is
/// passed over, never followed. The link names a directory
/// outside the root holding a dash file; a walk that
/// classified by followed metadata would descend through
/// the link and count that dash, and on a cyclic link would
/// recurse without bound. The standing collect rule takes
/// an entry only when it is a regular file or a link to
/// one, so the scan stays inside the root it was given.
#[cfg(unix)]
#[test]
fn codepoint_scan_does_not_follow_a_symlink_to_a_directory() {
    let base = temp_dir("linked-dir");
    let outside = base.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("note.md"), "a\u{2014}b").unwrap();
    let root = base.join("root");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("clean.md"), "clean").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("linked")).unwrap();
    let result = scan_tree(&root);
    assert_eq!(result.status, 0, "{}", result.stderr);
    assert!(result.stdout.contains("em dash U+2014 count 0"));
    assert!(result.stdout.contains("en dash U+2013 count 0"));
}

/// WS-A73: a symlink to a regular file is scanned through,
/// the module doc's link clause standing on this pin alone.
/// The link inside the root names a dash file outside it; a
/// collect rule that dropped the symlink arm would skip the
/// link silently and report zero counts at exit 0, so a
/// dash standing behind a link would pass the gate unseen.
#[cfg(unix)]
#[test]
fn codepoint_scan_scans_through_a_symlink_to_a_file() {
    let base = temp_dir("linked-file");
    let target = base.join("outside.md");
    std::fs::write(&target, "a\u{2014}b").unwrap();
    let root = base.join("root");
    std::fs::create_dir_all(&root).unwrap();
    std::os::unix::fs::symlink(&target, root.join("linked.md")).unwrap();
    let result = scan_tree(&root);
    assert_eq!(result.status, 1);
    assert!(result.stdout.contains("em dash U+2014 count 1"));
    assert!(result.stdout.contains("linked.md"));
}
