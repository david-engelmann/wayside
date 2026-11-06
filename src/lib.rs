//! Core checkpoint store logic for wayside.
//!
//! A store is a plain directory. Inside it live a lock file and
//! one directory per session, and inside each session one
//! directory per published checkpoint named by its sequence
//! number. A checkpoint holds a manifest, the payload files it
//! recorded, and the memory document. Every mutating operation
//! holds an exclusive flock on the lock file across the whole
//! read and assign and write and rename, so racing processes
//! cannot reuse a sequence number or interleave a publication. A
//! checkpoint is assembled under a temporary name and renamed
//! into place only when it is complete, and it is never modified
//! afterwards.
//!
//! This crate is the Rust port of the Python implementation that
//! landed Round 1, and it reproduces that behaviour, including
//! the error message texts, which are part of the recorded
//! behaviour. Where the port's mechanisms differ from the Python
//! mechanisms the port amendment in CONTRACT.md names the
//! difference.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const FORMAT_NAME: &str = "wayside-checkpoint/1";
pub const FORMAT_V2: &str = "wayside-checkpoint/2";
/// The largest summed size of the members of one declared
/// database bundle, in bytes (WS-14). Capture reads its bundle
/// twice under the store lock, so an unbounded bundle would be
/// an unbounded lock hold against every other writer.
pub const BUNDLE_BOUND: u64 = 268435456;
const LOCK_NAME: &str = "wayside.lock";
const SESSIONS_NAME: &str = "sessions";
const CHECKPOINTS_NAME: &str = "checkpoints";
const MANIFEST_NAME: &str = "manifest.json";
const FILES_NAME: &str = "files";
const MEMORY_NAME: &str = "memory.bin";
const ENV_FILE_NAME: &str = "wayside.env";
const MEMORY_FILE_NAME: &str = "wayside.memory";
const RESERVED_TOP_LEVEL: [&str; 2] = [ENV_FILE_NAME, MEMORY_FILE_NAME];
const ENV_HEADER: &str =
    "# Written by wayside. Env recorded by the checkpoint, restored as a file.";
const SESSION_MAX: usize = 64;

/// An error the CLI reports with a nonzero exit and no traceback.
///
/// The variants mirror the Python exception classes one for one,
/// and each carries the message text the Python class carried.
#[derive(Debug)]
pub enum Error {
    /// The base error, exit code 1.
    Wayside(String),
    /// A session id or a sequence is not in the accepted shape.
    InvalidName(String),
    /// The named session or checkpoint is not in the store.
    NotFound(String),
    /// A manifest or a payload cannot be read as it was recorded.
    Corrupt(String),
    /// The restore target is not a directory the store may write.
    Target(String),
}

impl Error {
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::Wayside(_) => 1,
            _ => 2,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Error::Wayside(message)
            | Error::InvalidName(message)
            | Error::NotFound(message)
            | Error::Corrupt(message)
            | Error::Target(message) => message,
        }
    }

    fn wayside(message: String) -> Error {
        Error::Wayside(message)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// The payloads of one verified checkpoint: the file entries
/// as relative path and bytes pairs in manifest order,
/// together with the memory document bytes.
type VerifiedPayloads = (Vec<(String, Vec<u8>)>, Vec<u8>);

/// One captured database bundle: the declared path together
/// with its member readings as relative path and bytes pairs
/// in bundle order.
type CapturedBundle = (String, Vec<(String, Vec<u8>)>);

/// The directory listing the state tree walk reads through.
type Lister = dyn Fn(&Path) -> io::Result<Vec<PathBuf>> + Send + Sync;

fn sha256_hex(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Render a string the way Python's repr renders it, for the
/// error messages that interpolate a value with repr formatting.
/// Printable text passes through, the quote and the backslash
/// and the common control characters are escaped, and any other
/// control character is rendered as a hex escape.
pub fn py_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::new();
    out.push(quote);
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || (c as u32) == 0x7f => {
                let n = c as u32;
                if n <= 0xff {
                    out.push_str(&format!("\\x{n:02x}"));
                } else if n <= 0xffff {
                    out.push_str(&format!("\\u{n:04x}"));
                } else {
                    out.push_str(&format!("\\U{n:08x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// A session id is one path safe component of lowercase letters
/// and digits and hyphens and underscores, at most 64 characters
/// long and beginning and ending with a letter or a digit.
pub fn validate_session_id(session: &str) -> Result<()> {
    let sound = session.chars().count() <= SESSION_MAX
        && !session.is_empty()
        && session
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        && session
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && session
            .chars()
            .last()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    if sound {
        Ok(())
    } else {
        Err(Error::InvalidName(format!(
            "invalid session id {}, use one path safe component of lowercase \
             letters and digits and hyphens and underscores, at most \
             {SESSION_MAX} characters long",
            py_repr(session)
        )))
    }
}

fn validate_sequence(sequence: i64) -> Result<i64> {
    if sequence < 1 {
        Err(Error::InvalidName(format!(
            "invalid sequence {sequence}, use a positive integer"
        )))
    } else {
        Ok(sequence)
    }
}

fn env_key_sound(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Check env pairs, raising through the given error factory,
/// which is the invalid env error at checkpoint time and the
/// corrupt error at manifest validation time.
fn validate_env_pairs(
    pairs: &[(String, StrictValue)],
    corrupt: &dyn Fn(String) -> Error,
) -> Result<()> {
    for (key, value) in pairs {
        if !env_key_sound(key) {
            return Err(corrupt(format!(
                "the env key {} is not a sound env name",
                py_repr(key)
            )));
        }
        let sound = match value {
            StrictValue::Str(text) => !text.contains('\n') && !text.contains('\0'),
            _ => false,
        };
        if !sound {
            return Err(corrupt(format!(
                "the env value for {key} is not a single line of text"
            )));
        }
    }
    Ok(())
}

fn validate_env(env: &BTreeMap<String, String>) -> Result<()> {
    let pairs: Vec<(String, StrictValue)> = env
        .iter()
        .map(|(key, value)| (key.clone(), StrictValue::Str(value.clone())))
        .collect();
    validate_env_pairs(&pairs, &|why| Error::wayside(format!("invalid env, {why}")))
}

/// A JSON value read by the strict manifest reader.
///
/// The reader is a hand written Visitor over serde_json whose
/// visit_map sees every key as it arrives and refuses a repeat
/// at any level, because serde_json alone keeps the last value
/// of a repeated key and says nothing, where the Python reader's
/// object pairs hook refused it. Numbers keep their integer
/// value at i128 width; a number with a fraction or an exponent
/// lands in the Float variant and fails every integer check, as
/// it did under the Python type checks.
#[derive(Debug, Clone, PartialEq)]
pub enum StrictValue {
    Null,
    Bool(bool),
    Int(i128),
    Float,
    Str(String),
    Array(Vec<StrictValue>),
    Object(Vec<(String, StrictValue)>),
}

impl<'de> serde::Deserialize<'de> for StrictValue {
    fn deserialize<D>(deserializer: D) -> std::result::Result<StrictValue, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct StrictVisitor;

        impl<'de> serde::de::Visitor<'de> for StrictVisitor {
            type Value = StrictValue;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a JSON value")
            }

            fn visit_bool<E>(self, value: bool) -> std::result::Result<StrictValue, E> {
                Ok(StrictValue::Bool(value))
            }

            fn visit_i64<E>(self, value: i64) -> std::result::Result<StrictValue, E> {
                Ok(StrictValue::Int(value as i128))
            }

            fn visit_u64<E>(self, value: u64) -> std::result::Result<StrictValue, E> {
                Ok(StrictValue::Int(value as i128))
            }

            fn visit_i128<E>(self, value: i128) -> std::result::Result<StrictValue, E>
            where
                E: serde::de::Error,
            {
                Ok(StrictValue::Int(value))
            }

            fn visit_f64<E>(self, _value: f64) -> std::result::Result<StrictValue, E> {
                Ok(StrictValue::Float)
            }

            fn visit_str<E>(self, value: &str) -> std::result::Result<StrictValue, E>
            where
                E: serde::de::Error,
            {
                Ok(StrictValue::Str(value.to_string()))
            }

            fn visit_string<E>(self, value: String) -> std::result::Result<StrictValue, E> {
                Ok(StrictValue::Str(value))
            }

            fn visit_none<E>(self) -> std::result::Result<StrictValue, E> {
                Ok(StrictValue::Null)
            }

            fn visit_unit<E>(self) -> std::result::Result<StrictValue, E> {
                Ok(StrictValue::Null)
            }

            fn visit_some<D>(self, deserializer: D) -> std::result::Result<StrictValue, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                StrictValue::deserialize(deserializer)
            }

            fn visit_seq<A>(self, mut seq: A) -> std::result::Result<StrictValue, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element()? {
                    items.push(item);
                }
                Ok(StrictValue::Array(items))
            }

            fn visit_map<A>(self, mut map: A) -> std::result::Result<StrictValue, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut entries: Vec<(String, StrictValue)> = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    if entries.iter().any(|(seen, _)| *seen == key) {
                        return Err(serde::de::Error::custom(format!(
                            "wayside-duplicate-key\x1f{key}"
                        )));
                    }
                    let value = map.next_value()?;
                    entries.push((key, value));
                }
                Ok(StrictValue::Object(entries))
            }
        }

        deserializer.deserialize_any(StrictVisitor)
    }
}

fn duplicate_key_of(error_text: &str) -> Option<String> {
    let marker = "wayside-duplicate-key\x1f";
    let start = error_text.find(marker)? + marker.len();
    let rest = &error_text[start..];
    let end = rest.find(" at line ").unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

/// The memory entry of a manifest.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryEntry {
    pub sha256: String,
    pub size: i128,
}

/// One recorded file of a manifest.
#[derive(Debug, Clone, PartialEq)]
pub struct FileEntry {
    pub path: String,
    pub sha256: String,
    pub size: i128,
}

/// The lineage a forked checkpoint records.
#[derive(Debug, Clone, PartialEq)]
pub struct ForkRef {
    pub session: String,
    pub sequence: i128,
}

/// One recorded member of a database bundle, in the shape of a
/// recorded file of wayside-checkpoint/1.
#[derive(Debug, Clone, PartialEq)]
pub struct DatabaseMember {
    pub path: String,
    pub sha256: String,
    pub size: i128,
}

/// One recorded database of a wayside-checkpoint/2 manifest:
/// its declared path, the digest of its member payload bytes
/// joined in bundle order, and its members in bundle order.
#[derive(Debug, Clone, PartialEq)]
pub struct DatabaseEntry {
    pub path: String,
    pub bundle_sha256: String,
    pub members: Vec<DatabaseMember>,
}

/// A validated checkpoint manifest, in the shape of the
/// wayside-checkpoint formats. The databases list stands empty
/// for every wayside-checkpoint/1 document.
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub format: String,
    pub session: String,
    pub sequence: i64,
    pub created_at: String,
    pub env: BTreeMap<String, String>,
    pub memory: MemoryEntry,
    pub files: Vec<FileEntry>,
    pub databases: Vec<DatabaseEntry>,
    pub forked_from: Option<ForkRef>,
}

/// How one database path stands across a comparison of two
/// checkpoints.
#[derive(Debug, Clone, PartialEq)]
pub enum DatabaseStanding {
    /// Both sides record the path with the same bundle digest.
    Equal,
    /// Both sides record the path with different bundle digests.
    Different,
    /// Only the first sequence records the path.
    OnlyFirst,
    /// Only the second sequence records the path.
    OnlySecond,
}

/// The standing of one database path in a comparison.
#[derive(Debug, Clone, PartialEq)]
pub struct DatabaseComparison {
    pub path: String,
    pub standing: DatabaseStanding,
}

/// The outcome of comparing the recorded databases of two
/// checkpoints of one session: the session, the two sequences
/// in the order they were named, and one standing per database
/// path recorded on either side, in path order.
#[derive(Debug, Clone, PartialEq)]
pub struct Comparison {
    pub session: String,
    pub first: i64,
    pub second: i64,
    pub databases: Vec<DatabaseComparison>,
}

/// The sequence and creation time list reports per checkpoint.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckpointInfo {
    pub sequence: i64,
    pub created_at: String,
}

fn corrupt(path: &Path, why: &str) -> Error {
    Error::Corrupt(format!("manifest at {} is corrupt, {why}", path.display()))
}

fn object_get<'a>(entries: &'a [(String, StrictValue)], key: &str) -> Option<&'a StrictValue> {
    entries.iter().find(|(seen, _)| seen == key).map(|(_, v)| v)
}

fn value_repr(value: &StrictValue) -> String {
    match value {
        StrictValue::Str(text) => py_repr(text),
        StrictValue::Int(number) => number.to_string(),
        StrictValue::Bool(flag) => {
            if *flag {
                "True".to_string()
            } else {
                "False".to_string()
            }
        }
        StrictValue::Null => "None".to_string(),
        StrictValue::Float => "a float value".to_string(),
        StrictValue::Array(_) => "a list value".to_string(),
        StrictValue::Object(_) => "a dict value".to_string(),
    }
}

fn validate_recorded_path(rel: &StrictValue, path: &Path) -> Result<String> {
    let text = match rel {
        StrictValue::Str(text) => text.clone(),
        other => {
            return Err(corrupt(
                path,
                &format!(
                    "the recorded path {} is not a safe relative path",
                    value_repr(other)
                ),
            ))
        }
    };
    let unsafe_path = text.is_empty()
        || text.starts_with('/')
        || text.contains('\\')
        || text.contains('\0')
        || text
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..");
    if unsafe_path {
        return Err(corrupt(
            path,
            &format!(
                "the recorded path {} is not a safe relative path",
                py_repr(&text)
            ),
        ));
    }
    if RESERVED_TOP_LEVEL.contains(&text.as_str()) {
        return Err(corrupt(
            path,
            &format!(
                "the recorded path {} names a file restore writes, \
                 so it is not a path a checkpoint may record",
                py_repr(&text)
            ),
        ));
    }
    Ok(text)
}

fn validate_hash(value: &StrictValue, path: &Path, what: &str) -> Result<String> {
    match value {
        StrictValue::Str(text)
            if text.len() == 64
                && text
                    .chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)) =>
        {
            Ok(text.clone())
        }
        _ => Err(corrupt(
            path,
            &format!("the {what} carries no sound SHA-256"),
        )),
    }
}

fn validate_size(value: &StrictValue, path: &Path, what: &str) -> Result<i128> {
    match value {
        StrictValue::Int(number) if *number >= 0 => Ok(*number),
        _ => Err(corrupt(path, &format!("the {what} carries no sound size"))),
    }
}

const MANIFEST_KEYS: [&str; 7] = [
    "format",
    "session",
    "sequence",
    "created_at",
    "env",
    "memory",
    "files",
];

/// Whether a recorded created_at parses under the reader the
/// port accepts. The parse is chrono's RFC 3339 reader, with
/// tightenings that keep the accepted family inside the Python
/// reader's: chrono takes a lowercase z as the zone marker
/// where Python's fromisoformat refused it, so a text ending
/// in a lowercase z is refused here (in a text the parser
/// accepts, a trailing lowercase z can only stand as the zone
/// marker, so the ending check refuses exactly that shape);
/// chrono folds a leap second into a nanosecond count of one
/// billion or more where Python refused a seconds field of
/// 60 outright; and chrono numbers years from zero where
/// Python's reader starts at year one.
fn created_at_parses(text: &str) -> bool {
    use chrono::Datelike;
    if text.ends_with('z') {
        return false;
    }
    match chrono::DateTime::parse_from_rfc3339(text) {
        Ok(parsed) => parsed.timestamp_subsec_nanos() < 1_000_000_000 && parsed.year() >= 1,
        Err(_) => false,
    }
}

/// Validate the databases field of a wayside-checkpoint/2
/// manifest under the strictness of WS-15: exact field sets at
/// both levels, recorded path rules on the database path and
/// every member path, member paths standing as the database
/// path followed by its -wal and -journal appendages in that
/// order, databases in path order with no path repeated, and
/// no database path or member path standing in the files list.
fn validate_databases(
    value: &StrictValue,
    path: &Path,
    file_paths: &[String],
) -> Result<Vec<DatabaseEntry>> {
    let items = match value {
        StrictValue::Array(items) => items,
        _ => return Err(corrupt(path, "the databases entry is not a list")),
    };
    if items.is_empty() {
        return Err(corrupt(
            path,
            "the databases entry is empty, and a manifest that records \
             databases names at least one",
        ));
    }
    let mut databases = Vec::new();
    let mut database_paths: Vec<String> = Vec::new();
    let mut recorded_paths: Vec<String> = Vec::new();
    for item in items {
        let pairs = match item {
            StrictValue::Object(pairs)
                if pairs.len() == 3
                    && object_get(pairs, "path").is_some()
                    && object_get(pairs, "bundle_sha256").is_some()
                    && object_get(pairs, "members").is_some() =>
            {
                pairs
            }
            _ => {
                return Err(corrupt(
                    path,
                    "a database entry does not carry exactly path and \
                     bundle_sha256 and members",
                ))
            }
        };
        let rel = validate_recorded_path(object_get(pairs, "path").unwrap(), path)?;
        let bundle_sha256 = validate_hash(
            object_get(pairs, "bundle_sha256").unwrap(),
            path,
            &format!("database entry for {rel}"),
        )?;
        let member_values = match object_get(pairs, "members") {
            Some(StrictValue::Array(items)) => items,
            _ => {
                return Err(corrupt(
                    path,
                    &format!("the members entry of the database at {rel} is not a list"),
                ))
            }
        };
        if member_values.is_empty() {
            return Err(corrupt(
                path,
                &format!("the database at {rel} carries no members"),
            ));
        }
        let bundle_slots = [rel.clone(), format!("{rel}-wal"), format!("{rel}-journal")];
        let mut next_slot = 0usize;
        let mut members = Vec::new();
        for member in member_values.iter() {
            let member_pairs = match member {
                StrictValue::Object(pairs)
                    if pairs.len() == 3
                        && object_get(pairs, "path").is_some()
                        && object_get(pairs, "sha256").is_some()
                        && object_get(pairs, "size").is_some() =>
                {
                    pairs
                }
                _ => {
                    return Err(corrupt(
                        path,
                        "a database member entry does not carry exactly path \
                         and sha256 and size",
                    ))
                }
            };
            let member_rel =
                validate_recorded_path(object_get(member_pairs, "path").unwrap(), path)?;
            // The members stand in bundle order, the database path
            // first, then its -wal, then its -journal, and a
            // companion that does not stand is simply absent, so the
            // recorded members are an ordered subsequence of the
            // three slots (WS-15). A member is in order when it
            // names a slot the earlier members have not passed, and
            // the first member must take the first slot.
            let in_order = match bundle_slots[next_slot..]
                .iter()
                .position(|slot| slot == &member_rel)
            {
                Some(offset) if !members.is_empty() || offset == 0 => {
                    next_slot += offset + 1;
                    true
                }
                _ => false,
            };
            if !in_order {
                return Err(corrupt(
                    path,
                    &format!(
                        "the member paths of the database at {rel} are not the \
                         database path followed by its -wal and -journal \
                         appendages in that order"
                    ),
                ));
            }
            let sha = validate_hash(
                object_get(member_pairs, "sha256").unwrap(),
                path,
                &format!("database member entry for {member_rel}"),
            )?;
            let size = validate_size(
                object_get(member_pairs, "size").unwrap(),
                path,
                &format!("database member entry for {member_rel}"),
            )?;
            members.push(DatabaseMember {
                path: member_rel,
                sha256: sha,
                size,
            });
        }
        database_paths.push(rel.clone());
        for member in &members {
            recorded_paths.push(member.path.clone());
        }
        databases.push(DatabaseEntry {
            path: rel,
            bundle_sha256,
            members,
        });
    }
    let mut sorted = database_paths.clone();
    sorted.sort();
    let mut deduped = database_paths.clone();
    deduped.sort();
    deduped.dedup();
    if database_paths != sorted || deduped.len() != database_paths.len() {
        return Err(corrupt(
            path,
            "the database entries are not in path order or repeat a path",
        ));
    }
    let mut recorded_sorted = recorded_paths.clone();
    recorded_sorted.sort();
    recorded_sorted.dedup();
    if recorded_sorted.len() != recorded_paths.len() {
        return Err(corrupt(path, "the database entries repeat a path"));
    }
    for recorded in &recorded_paths {
        if file_paths.contains(recorded) {
            return Err(corrupt(
                path,
                &format!(
                    "the path {recorded} stands both in the files list and \
                     in a database entry"
                ),
            ));
        }
    }
    Ok(databases)
}

fn validate_manifest(
    data: &StrictValue,
    path: &Path,
    session: &str,
    sequence: i64,
) -> Result<Manifest> {
    let entries = match data {
        StrictValue::Object(entries) => entries,
        _ => return Err(corrupt(path, "the top level is not an object")),
    };
    let is_v2 = matches!(
        object_get(entries, "format"),
        Some(StrictValue::Str(text)) if text == FORMAT_V2
    );
    let keys_sound = MANIFEST_KEYS
        .iter()
        .all(|key| object_get(entries, key).is_some())
        && entries.iter().all(|(key, _)| {
            MANIFEST_KEYS.contains(&key.as_str())
                || key == "forked_from"
                || (is_v2 && key == "databases")
        })
        && (!is_v2 || object_get(entries, "databases").is_some());
    if !keys_sound {
        return Err(corrupt(path, "the field set is not the recorded one"));
    }
    match object_get(entries, "format") {
        Some(StrictValue::Str(text)) if text == FORMAT_NAME || text == FORMAT_V2 => {}
        _ => return Err(corrupt(path, &format!("the format is not {FORMAT_NAME}"))),
    }
    match object_get(entries, "session") {
        Some(StrictValue::Str(text)) if text == session => {}
        other => {
            let named = other.map(value_repr).unwrap_or_default();
            return Err(corrupt(
                path,
                &format!("the manifest names session {named} where it sits in session {session}"),
            ));
        }
    }
    match object_get(entries, "sequence") {
        Some(StrictValue::Int(number)) if *number == sequence as i128 => {}
        _ => {
            return Err(corrupt(
                path,
                "the manifest sequence does not match the directory it sits in",
            ))
        }
    }
    let created_at = match object_get(entries, "created_at") {
        Some(StrictValue::Str(text)) => {
            if !created_at_parses(text) {
                return Err(corrupt(
                    path,
                    "the created_at does not parse as a timestamp",
                ));
            }
            text.clone()
        }
        _ => return Err(corrupt(path, "the created_at is not text")),
    };
    let env_entries = match object_get(entries, "env") {
        Some(StrictValue::Object(pairs)) => pairs.clone(),
        _ => {
            return Err(corrupt(
                path,
                "the env is not an object of key and value pairs",
            ))
        }
    };
    validate_env_pairs(&env_entries, &|why| corrupt(path, &why))?;
    let env: BTreeMap<String, String> = env_entries
        .into_iter()
        .map(|(key, value)| match value {
            StrictValue::Str(text) => (key, text),
            _ => (key, String::new()),
        })
        .collect();
    let memory = match object_get(entries, "memory") {
        Some(StrictValue::Object(pairs))
            if pairs.len() == 2
                && object_get(pairs, "sha256").is_some()
                && object_get(pairs, "size").is_some() =>
        {
            MemoryEntry {
                sha256: validate_hash(object_get(pairs, "sha256").unwrap(), path, "memory entry")?,
                size: validate_size(object_get(pairs, "size").unwrap(), path, "memory entry")?,
            }
        }
        _ => {
            return Err(corrupt(
                path,
                "the memory entry does not carry exactly sha256 and size",
            ))
        }
    };
    let file_values = match object_get(entries, "files") {
        Some(StrictValue::Array(items)) => items,
        _ => return Err(corrupt(path, "the files entry is not a list")),
    };
    let mut files = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for entry in file_values {
        let pair = match entry {
            StrictValue::Object(pairs)
                if pairs.len() == 3
                    && object_get(pairs, "path").is_some()
                    && object_get(pairs, "sha256").is_some()
                    && object_get(pairs, "size").is_some() =>
            {
                pairs
            }
            _ => {
                return Err(corrupt(
                    path,
                    "a file entry does not carry exactly path and sha256 and size",
                ))
            }
        };
        let rel = validate_recorded_path(object_get(pair, "path").unwrap(), path)?;
        let sha = validate_hash(
            object_get(pair, "sha256").unwrap(),
            path,
            &format!("file entry for {rel}"),
        )?;
        let size = validate_size(
            object_get(pair, "size").unwrap(),
            path,
            &format!("file entry for {rel}"),
        )?;
        seen.push(rel.clone());
        files.push(FileEntry {
            path: rel,
            sha256: sha,
            size,
        });
    }
    let mut sorted = seen.clone();
    sorted.sort();
    let mut deduped = seen.clone();
    deduped.sort();
    deduped.dedup();
    if seen != sorted || deduped.len() != seen.len() {
        return Err(corrupt(
            path,
            "the file entries are not in path order or repeat a path",
        ));
    }
    let databases = if is_v2 {
        validate_databases(object_get(entries, "databases").unwrap(), path, &seen)?
    } else {
        Vec::new()
    };
    let forked_from = match object_get(entries, "forked_from") {
        None => None,
        Some(StrictValue::Object(pairs))
            if pairs.len() == 2
                && object_get(pairs, "session").is_some()
                && object_get(pairs, "sequence").is_some() =>
        {
            let fork_session = match object_get(pairs, "session") {
                Some(StrictValue::Str(text)) => text.clone(),
                _ => {
                    return Err(corrupt(
                        path,
                        "the forked_from session is not a safe session id",
                    ))
                }
            };
            if validate_session_id(&fork_session).is_err() {
                return Err(corrupt(
                    path,
                    "the forked_from session is not a safe session id",
                ));
            }
            let fork_sequence = match object_get(pairs, "sequence") {
                Some(StrictValue::Int(number)) if *number >= 1 => *number,
                _ => {
                    return Err(corrupt(
                        path,
                        "the forked_from sequence is not a positive integer",
                    ))
                }
            };
            Some(ForkRef {
                session: fork_session,
                sequence: fork_sequence,
            })
        }
        Some(_) => {
            return Err(corrupt(
                path,
                "the forked_from entry does not carry exactly session and sequence",
            ))
        }
    };
    Ok(Manifest {
        format: if is_v2 {
            FORMAT_V2.to_string()
        } else {
            FORMAT_NAME.to_string()
        },
        session: session.to_string(),
        sequence,
        created_at,
        env,
        memory,
        files,
        databases,
        forked_from,
    })
}

/// Escape a string the way Python's json module escapes it with
/// its default ensure_ascii, so a manifest the port writes is
/// byte identical to the manifest the Python implementation
/// wrote for the same content: the quote, the backslash, and
/// every character outside the printable ASCII range is escaped,
/// with a surrogate pair for a character past the basic plane.
fn escape_json_string(text: &str, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let n = c as u32;
                if n <= 0xffff {
                    out.push_str(&format!("\\u{n:04x}"));
                } else {
                    let shifted = n - 0x10000;
                    let high = 0xd800 + (shifted >> 10);
                    let low = 0xdc00 + (shifted & 0x3ff);
                    out.push_str(&format!("\\u{high:04x}\\u{low:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn write_indent(out: &mut String, level: usize) {
    for _ in 0..level {
        out.push_str("  ");
    }
}

fn render_env_object(env: &BTreeMap<String, String>, level: usize, out: &mut String) {
    if env.is_empty() {
        out.push_str("{}");
        return;
    }
    out.push_str("{\n");
    let count = env.len();
    for (index, (key, value)) in env.iter().enumerate() {
        write_indent(out, level + 1);
        escape_json_string(key, out);
        out.push_str(": ");
        escape_json_string(value, out);
        if index + 1 < count {
            out.push(',');
        }
        out.push('\n');
    }
    write_indent(out, level);
    out.push('}');
}

fn render_files_array(files: &[FileEntry], level: usize, out: &mut String) {
    if files.is_empty() {
        out.push_str("[]");
        return;
    }
    out.push_str("[\n");
    let count = files.len();
    for (index, entry) in files.iter().enumerate() {
        write_indent(out, level + 1);
        out.push_str("{\n");
        write_indent(out, level + 2);
        escape_json_string("path", out);
        out.push_str(": ");
        escape_json_string(&entry.path, out);
        out.push_str(",\n");
        write_indent(out, level + 2);
        escape_json_string("sha256", out);
        out.push_str(": ");
        escape_json_string(&entry.sha256, out);
        out.push_str(",\n");
        write_indent(out, level + 2);
        escape_json_string("size", out);
        out.push_str(": ");
        out.push_str(&entry.size.to_string());
        out.push('\n');
        write_indent(out, level + 1);
        out.push('}');
        if index + 1 < count {
            out.push(',');
        }
        out.push('\n');
    }
    write_indent(out, level);
    out.push(']');
}

fn render_members_array(members: &[DatabaseMember], level: usize, out: &mut String) {
    if members.is_empty() {
        out.push_str("[]");
        return;
    }
    out.push_str("[\n");
    let count = members.len();
    for (index, member) in members.iter().enumerate() {
        write_indent(out, level + 1);
        out.push_str("{\n");
        write_indent(out, level + 2);
        escape_json_string("path", out);
        out.push_str(": ");
        escape_json_string(&member.path, out);
        out.push_str(",\n");
        write_indent(out, level + 2);
        escape_json_string("sha256", out);
        out.push_str(": ");
        escape_json_string(&member.sha256, out);
        out.push_str(",\n");
        write_indent(out, level + 2);
        escape_json_string("size", out);
        out.push_str(": ");
        out.push_str(&member.size.to_string());
        out.push('\n');
        write_indent(out, level + 1);
        out.push('}');
        if index + 1 < count {
            out.push(',');
        }
        out.push('\n');
    }
    write_indent(out, level);
    out.push(']');
}

fn render_databases_array(databases: &[DatabaseEntry], level: usize, out: &mut String) {
    if databases.is_empty() {
        out.push_str("[]");
        return;
    }
    out.push_str("[\n");
    let count = databases.len();
    for (index, entry) in databases.iter().enumerate() {
        write_indent(out, level + 1);
        out.push_str("{\n");
        write_indent(out, level + 2);
        escape_json_string("bundle_sha256", out);
        out.push_str(": ");
        escape_json_string(&entry.bundle_sha256, out);
        out.push_str(",\n");
        write_indent(out, level + 2);
        escape_json_string("members", out);
        out.push_str(": ");
        render_members_array(&entry.members, level + 2, out);
        out.push_str(",\n");
        write_indent(out, level + 2);
        escape_json_string("path", out);
        out.push_str(": ");
        escape_json_string(&entry.path, out);
        out.push('\n');
        write_indent(out, level + 1);
        out.push('}');
        if index + 1 < count {
            out.push(',');
        }
        out.push('\n');
    }
    write_indent(out, level);
    out.push(']');
}

/// Render a manifest exactly as the Python implementation's
/// sorted, two space indented json rendering renders it, with
/// no trailing newline. The stored manifest file carries this
/// text plus one newline, and the show command prints this text
/// plus one newline, so the two are byte identical. A
/// wayside-checkpoint/2 manifest renders by its own format
/// value, with the databases field standing in the sorted key
/// order the rendering keeps, after created_at.
pub fn render_manifest(manifest: &Manifest) -> String {
    let mut out = String::new();
    out.push_str("{\n");
    let mut fields: Vec<String> = Vec::new();
    let mut field = String::new();
    write_indent(&mut field, 1);
    escape_json_string("created_at", &mut field);
    field.push_str(": ");
    escape_json_string(&manifest.created_at, &mut field);
    fields.push(field);

    if manifest.format == FORMAT_V2 {
        let mut field = String::new();
        write_indent(&mut field, 1);
        escape_json_string("databases", &mut field);
        field.push_str(": ");
        render_databases_array(&manifest.databases, 1, &mut field);
        fields.push(field);
    }

    let mut field = String::new();
    write_indent(&mut field, 1);
    escape_json_string("env", &mut field);
    field.push_str(": ");
    render_env_object(&manifest.env, 1, &mut field);
    fields.push(field);

    let mut field = String::new();
    write_indent(&mut field, 1);
    escape_json_string("files", &mut field);
    field.push_str(": ");
    render_files_array(&manifest.files, 1, &mut field);
    fields.push(field);

    if let Some(fork) = &manifest.forked_from {
        let mut field = String::new();
        write_indent(&mut field, 1);
        escape_json_string("forked_from", &mut field);
        field.push_str(": {\n");
        write_indent(&mut field, 2);
        escape_json_string("sequence", &mut field);
        field.push_str(": ");
        field.push_str(&fork.sequence.to_string());
        field.push_str(",\n");
        write_indent(&mut field, 2);
        escape_json_string("session", &mut field);
        field.push_str(": ");
        escape_json_string(&fork.session, &mut field);
        field.push('\n');
        write_indent(&mut field, 1);
        field.push('}');
        fields.push(field);
    }

    let mut field = String::new();
    write_indent(&mut field, 1);
    escape_json_string("format", &mut field);
    field.push_str(": ");
    escape_json_string(&manifest.format, &mut field);
    fields.push(field);

    let mut field = String::new();
    write_indent(&mut field, 1);
    escape_json_string("memory", &mut field);
    field.push_str(": {\n");
    write_indent(&mut field, 2);
    escape_json_string("sha256", &mut field);
    field.push_str(": ");
    escape_json_string(&manifest.memory.sha256, &mut field);
    field.push_str(",\n");
    write_indent(&mut field, 2);
    escape_json_string("size", &mut field);
    field.push_str(": ");
    field.push_str(&manifest.memory.size.to_string());
    field.push('\n');
    write_indent(&mut field, 1);
    field.push('}');
    fields.push(field);

    let mut field = String::new();
    write_indent(&mut field, 1);
    escape_json_string("sequence", &mut field);
    field.push_str(": ");
    field.push_str(&manifest.sequence.to_string());
    fields.push(field);

    let mut field = String::new();
    write_indent(&mut field, 1);
    escape_json_string("session", &mut field);
    field.push_str(": ");
    escape_json_string(&manifest.session, &mut field);
    fields.push(field);

    out.push_str(&fields.join(",\n"));
    out.push_str("\n}");
    out
}

/// The current instant in the shape Python's isoformat writes
/// for a UTC datetime: a T separator, a +00:00 offset, and a six
/// digit fraction present exactly when the microsecond part is
/// nonzero.
fn now_isoformat() -> String {
    let now = chrono::Utc::now();
    let base = now.format("%Y-%m-%dT%H:%M:%S").to_string();
    let micros = now.timestamp_subsec_micros();
    if micros == 0 {
        format!("{base}+00:00")
    } else {
        format!("{base}.{micros:06}+00:00")
    }
}

fn sessions_dir(root: &Path) -> PathBuf {
    root.join(SESSIONS_NAME)
}

fn session_dir(root: &Path, session: &str) -> PathBuf {
    sessions_dir(root).join(session)
}

fn checkpoints_dir(root: &Path, session: &str) -> PathBuf {
    session_dir(root, session).join(CHECKPOINTS_NAME)
}

fn sequence_name(sequence: i64) -> String {
    format!("{sequence:06}")
}

fn checkpoint_dir(root: &Path, session: &str, sequence: i64) -> PathBuf {
    checkpoints_dir(root, session).join(sequence_name(sequence))
}

fn refuse_store_link(path: &Path, what: &str) -> Result<()> {
    if path.is_symlink() {
        Err(Error::Corrupt(format!(
            "the store is corrupt, {what} at {} is a symbolic link, \
             and the store holds only real directories and regular files",
            path.display()
        )))
    } else {
        Ok(())
    }
}

/// Refuse a planted link at any store component above a
/// checkpoint. The store's own writer creates only real
/// directories inside the store, so a symbolic link standing
/// where the store keeps a session is damage, and following it
/// would write checkpoints outside the store or read outside
/// bytes as authoritative ones.
fn check_session_links(root: &Path, session: &str) -> Result<()> {
    refuse_store_link(&sessions_dir(root), "the sessions directory")?;
    refuse_store_link(
        &session_dir(root, session),
        &format!("the directory of session {session}"),
    )?;
    refuse_store_link(
        &checkpoints_dir(root, session),
        &format!("the checkpoints directory of session {session}"),
    )?;
    Ok(())
}

fn check_checkpoint_links(root: &Path, session: &str, sequence: i64) -> Result<()> {
    check_session_links(root, session)?;
    let ck = checkpoint_dir(root, session, sequence);
    refuse_store_link(&ck, &format!("checkpoint {sequence} of session {session}"))?;
    refuse_links_below(&ck, session, sequence)?;
    Ok(())
}

/// Walk one checkpoint directory refusing every link found, in
/// the shape of the Python walk: a directory that cannot be
/// listed is passed over in silence, as the Python walk with no
/// error callback passed over it.
fn refuse_links_below(dir: &Path, session: &str, sequence: i64) -> Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(()),
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        refuse_store_link(
            &path,
            &format!("an entry of checkpoint {sequence} of session {session}"),
        )?;
        let is_dir = fs::metadata(&path)
            .map(|meta| meta.is_dir())
            .unwrap_or(false);
        if is_dir {
            refuse_links_below(&path, session, sequence)?;
        }
    }
    Ok(())
}

struct StoreLock {
    file: fs::File,
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Take the exclusive flock on the store lock file, creating
/// the store root and the lock file when they do not stand.
fn locked(root: &Path) -> Result<StoreLock> {
    let opened = (|| -> io::Result<fs::File> {
        fs::create_dir_all(root)?;
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join(LOCK_NAME))?;
        file.lock_exclusive()?;
        Ok(file)
    })();
    match opened {
        Ok(file) => Ok(StoreLock { file }),
        Err(exc) => Err(Error::wayside(format!(
            "the store at {} cannot be opened, {exc}",
            root.display()
        ))),
    }
}

fn is_sequence_dir_name(name: &str) -> bool {
    name.len() >= 6 && name.chars().all(|c| c.is_ascii_digit())
}

/// The published sequences of a session, in order. Following
/// the Python listing, a linked sequence directory counts
/// toward the listing here (the metadata test follows links),
/// even though every read of it is refused by the link checks,
/// and a residue directory under a temporary name never counts.
fn published_sequences(root: &Path, session: &str) -> Vec<i64> {
    let ck_dir = checkpoints_dir(root, session);
    let entries = match fs::read_dir(&ck_dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };
    let mut sequences = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_sequence_dir_name(&name) && entry.path().is_dir() {
            if let Ok(sequence) = name.parse::<i64>() {
                sequences.push(sequence);
            }
        }
    }
    sequences.sort_unstable();
    sequences
}

fn require_session(root: &Path, session: &str) -> Result<()> {
    if session_dir(root, session).is_dir() {
        Ok(())
    } else {
        Err(Error::NotFound(format!(
            "session {session} is not in the store at {}",
            root.display()
        )))
    }
}

/// Read and strictly validate one manifest, changing nothing
/// on disk.
fn load_manifest(root: &Path, session: &str, sequence: i64) -> Result<Manifest> {
    let ck = checkpoint_dir(root, session, sequence);
    check_checkpoint_links(root, session, sequence)?;
    let path = ck.join(MANIFEST_NAME);
    if !path.is_file() {
        if ck.is_dir() {
            return Err(Error::Corrupt(format!(
                "checkpoint {sequence} of session {session} is corrupt, \
                 its manifest is missing or is not a regular file, \
                 so the checkpoint cannot be read"
            )));
        }
        return Err(Error::NotFound(format!(
            "session {session} holds no checkpoint {sequence} in the store at {}",
            root.display()
        )));
    }
    let raw_bytes =
        fs::read(&path).map_err(|exc| corrupt(&path, &format!("it cannot be read, {exc}")))?;
    let raw = String::from_utf8(raw_bytes).map_err(|exc| {
        corrupt(
            &path,
            &format!("it is not valid UTF-8 text, {}", exc.utf8_error()),
        )
    })?;
    let data: StrictValue = serde_json::from_str(&raw).map_err(|exc| {
        let text = exc.to_string();
        if let Some(key) = duplicate_key_of(&text) {
            corrupt(&path, &format!("it repeats the key {key}"))
        } else if text.contains("recursion limit") {
            corrupt(&path, "it nests too deeply to be read")
        } else {
            corrupt(&path, &format!("it is not valid JSON, {text}"))
        }
    })?;
    validate_manifest(&data, &path, session, sequence)
}

/// Read one payload of a checkpoint from its files area and
/// check it against its recorded hash and size.
fn read_payload(
    ck: &Path,
    session: &str,
    sequence: i64,
    rel: &str,
    sha256: &str,
    size: i128,
) -> Result<Vec<u8>> {
    let mut payload = ck.join(FILES_NAME);
    for part in rel.split('/') {
        payload.push(part);
    }
    if !payload.is_file() {
        return Err(Error::Corrupt(format!(
            "checkpoint {sequence} of session {session} is corrupt, \
             the payload for {rel} is missing"
        )));
    }
    let data = fs::read(&payload).map_err(|exc| {
        Error::Corrupt(format!(
            "checkpoint {sequence} of session {session} is corrupt, \
             the payload for {rel} cannot be read, {exc}"
        ))
    })?;
    if data.len() as i128 != size || sha256_hex(&data) != sha256 {
        return Err(Error::Corrupt(format!(
            "checkpoint {sequence} of session {session} is corrupt, \
             the payload for {rel} does not match its recorded hash and size"
        )));
    }
    Ok(data)
}

/// Read every payload of a checkpoint and check it against the
/// manifest: the recorded files, then every database member,
/// then the memory document. Returns the payload entries as
/// relative path and bytes pairs in that order together with
/// the memory bytes. Nothing is placed anywhere by this read,
/// so a caller can verify a whole checkpoint before it writes
/// a single byte of it.
fn read_verified(
    root: &Path,
    session: &str,
    sequence: i64,
    manifest: &Manifest,
) -> Result<VerifiedPayloads> {
    let ck = checkpoint_dir(root, session, sequence);
    let mut entries = Vec::new();
    for entry in &manifest.files {
        let data = read_payload(
            &ck,
            session,
            sequence,
            &entry.path,
            &entry.sha256,
            entry.size,
        )?;
        entries.push((entry.path.clone(), data));
    }
    for database in &manifest.databases {
        for member in &database.members {
            let data = read_payload(
                &ck,
                session,
                sequence,
                &member.path,
                &member.sha256,
                member.size,
            )?;
            entries.push((member.path.clone(), data));
        }
    }
    let memory_path = ck.join(MEMORY_NAME);
    if !memory_path.is_file() {
        return Err(Error::Corrupt(format!(
            "checkpoint {sequence} of session {session} is corrupt, \
             the memory document is missing"
        )));
    }
    let memory = fs::read(&memory_path).map_err(|exc| {
        Error::Corrupt(format!(
            "checkpoint {sequence} of session {session} is corrupt, \
             the memory document cannot be read, {exc}"
        ))
    })?;
    if memory.len() as i128 != manifest.memory.size || sha256_hex(&memory) != manifest.memory.sha256
    {
        return Err(Error::Corrupt(format!(
            "checkpoint {sequence} of session {session} is corrupt, \
             the memory document does not match its recorded hash and size"
        )));
    }
    Ok((entries, memory))
}

/// What a staged byte rule serves for one path once its read
/// count reaches the rule's first staged read.
enum ByteFate {
    /// Serve these bytes instead of the bytes on disk.
    Serve(Vec<u8>),
    /// Fail the read as if the file did not stand.
    Missing,
}

/// A staged override of the bytes of one path, standing from a
/// named read of that path onward.
struct ByteRule {
    path: PathBuf,
    from_read: usize,
    fate: ByteFate,
}

/// What a staged standing rule reports for one path once its
/// probe count reaches the rule's first staged probe.
enum ProbeFate {
    /// Report the path standing with this size.
    Appear(u64),
    /// Report the path not standing.
    Vanish,
}

/// A staged override of the standing of one path, standing
/// from a named probe of that path onward.
struct ProbeRule {
    path: PathBuf,
    from_probe: usize,
    fate: ProbeFate,
}

/// The reader the state tree walk lists directories through,
/// and the reader a database capture reads its bundle members
/// through.
///
/// The default reader lists with fs::read_dir. The seam exists
/// for the two walk error pins: the Python suite staged a
/// listing failure by patching the directory listing call,
/// which Rust cannot do, and the build machines run the suite
/// as root, where permission bits do not produce a listing
/// failure either, so the pins inject a listing that fails for
/// one named path through this reader instead. The seam
/// changes no default behaviour, and the port amendment in
/// CONTRACT.md names it as a reshaping.
///
/// Round 4 extends the seam to the reads a bundle capture
/// makes: the bytes of a member and the standing of a sidecar
/// name. Staged readers override one path from its second read
/// or its second standing probe onward, so a pin can stage a
/// bundle that changes between the two passes of WS-13
/// deterministically, without racing a real writer. The
/// default reader reads and stats the files themselves.
pub struct StateTreeReader {
    lister: Box<Lister>,
    byte_rules: Vec<ByteRule>,
    probe_rules: Vec<ProbeRule>,
    counts: std::sync::Mutex<BTreeMap<PathBuf, [usize; 2]>>,
}

impl StateTreeReader {
    /// The production reader, listing with fs::read_dir and
    /// reading and statting files as they stand.
    pub fn new() -> StateTreeReader {
        StateTreeReader {
            lister: Box::new(|dir| {
                let mut paths = Vec::new();
                for entry in fs::read_dir(dir)? {
                    paths.push(entry?.path());
                }
                Ok(paths)
            }),
            byte_rules: Vec::new(),
            probe_rules: Vec::new(),
            counts: std::sync::Mutex::new(BTreeMap::new()),
        }
    }

    /// A reader whose listing fails for exactly the named
    /// directory, staging the walk error pins.
    pub fn failing_at(blocked: &Path) -> StateTreeReader {
        let blocked = blocked.to_path_buf();
        let inner = StateTreeReader::new();
        StateTreeReader {
            lister: Box::new(move |dir| {
                if dir == blocked {
                    Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "permission denied",
                    ))
                } else {
                    (inner.lister)(dir)
                }
            }),
            byte_rules: Vec::new(),
            probe_rules: Vec::new(),
            counts: std::sync::Mutex::new(BTreeMap::new()),
        }
    }

    /// A reader that serves the given bytes for the named path
    /// on every bundle read of it, staging a bundle whose bytes
    /// at capture differ from the bytes the tree walk read.
    pub fn serving_bytes(path: &Path, bytes: Vec<u8>) -> StateTreeReader {
        let mut reader = StateTreeReader::new();
        reader.byte_rules.push(ByteRule {
            path: path.to_path_buf(),
            from_read: 1,
            fate: ByteFate::Serve(bytes),
        });
        reader
    }

    /// A reader that serves the given bytes for the named path
    /// from its second bundle read onward, staging a member
    /// that changes between the two passes of a capture.
    pub fn altering_bytes_on_second_read(path: &Path, bytes: Vec<u8>) -> StateTreeReader {
        let mut reader = StateTreeReader::new();
        reader.byte_rules.push(ByteRule {
            path: path.to_path_buf(),
            from_read: 2,
            fate: ByteFate::Serve(bytes),
        });
        reader
    }

    /// A reader whose bundle read of the named path fails as
    /// if the file did not stand from its second read onward,
    /// staging a member that vanishes between the two passes.
    pub fn dropping_on_second_read(path: &Path) -> StateTreeReader {
        let mut reader = StateTreeReader::new();
        reader.byte_rules.push(ByteRule {
            path: path.to_path_buf(),
            from_read: 2,
            fate: ByteFate::Missing,
        });
        reader
    }

    /// A reader that reports the named path standing with the
    /// given size from its second standing probe onward,
    /// staging a sidecar that appears between the two passes.
    pub fn appearing_on_second_probe(path: &Path, size: u64) -> StateTreeReader {
        let mut reader = StateTreeReader::new();
        reader.probe_rules.push(ProbeRule {
            path: path.to_path_buf(),
            from_probe: 2,
            fate: ProbeFate::Appear(size),
        });
        reader
    }

    /// A reader that reports the named path not standing from
    /// its second standing probe onward, staging a sidecar
    /// that disappears between the two passes.
    pub fn vanishing_on_second_probe(path: &Path) -> StateTreeReader {
        let mut reader = StateTreeReader::new();
        reader.probe_rules.push(ProbeRule {
            path: path.to_path_buf(),
            from_probe: 2,
            fate: ProbeFate::Vanish,
        });
        reader
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<PathBuf>> {
        (self.lister)(dir)
    }

    /// Count one read or one standing probe of a path, in the
    /// slot the caller names, and return the new count.
    fn bump(&self, path: &Path, slot: usize) -> usize {
        let mut counts = self
            .counts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entry = counts.entry(path.to_path_buf()).or_insert([0, 0]);
        entry[slot] += 1;
        entry[slot]
    }

    /// Read the bytes of one bundle member, through any staged
    /// rule that stands for the path.
    pub fn read_bytes(&self, path: &Path) -> io::Result<Vec<u8>> {
        let read = self.bump(path, 0);
        for rule in &self.byte_rules {
            if rule.path == path && read >= rule.from_read {
                match &rule.fate {
                    ByteFate::Serve(bytes) => return Ok(bytes.clone()),
                    ByteFate::Missing => {
                        return Err(io::Error::new(
                            io::ErrorKind::NotFound,
                            "the file does not stand",
                        ))
                    }
                }
            }
        }
        fs::read(path)
    }

    /// Report the standing of one bundle member name: the size
    /// of the file when the path stands as a regular file, and
    /// nothing when it does not stand, through any staged rule
    /// that stands for the path.
    pub fn member_len(&self, path: &Path) -> io::Result<Option<u64>> {
        let probe = self.bump(path, 1);
        for rule in &self.probe_rules {
            if rule.path == path && probe >= rule.from_probe {
                match rule.fate {
                    ProbeFate::Appear(size) => return Ok(Some(size)),
                    ProbeFate::Vanish => return Ok(None),
                }
            }
        }
        match fs::metadata(path) {
            Ok(meta) if meta.is_file() => Ok(Some(meta.len())),
            Ok(_) => Ok(None),
            Err(exc) if exc.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(exc) => Err(exc),
        }
    }
}

impl Default for StateTreeReader {
    fn default() -> StateTreeReader {
        StateTreeReader::new()
    }
}

/// Read every regular file under the state directory, sorted
/// by path. Symbolic links and non regular files are refused,
/// because a link can name bytes outside the state directory
/// and a checkpoint records only what the caller can see in
/// the tree itself. A directory that cannot be listed fails
/// the read, naming that directory, instead of publishing a
/// partial tree as if it were complete.
fn read_state_tree(state: &Path, reader: &StateTreeReader) -> Result<Vec<(String, Vec<u8>)>> {
    let mut entries = Vec::new();
    read_state_dir(state, state, reader, &mut entries)?;
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(entries)
}

fn read_state_dir(
    state: &Path,
    dir: &Path,
    reader: &StateTreeReader,
    entries: &mut Vec<(String, Vec<u8>)>,
) -> Result<()> {
    let listed = reader.list(dir).map_err(|exc| {
        Error::wayside(format!(
            "the state directory at {} cannot be listed, {exc}",
            dir.display()
        ))
    })?;
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut files: Vec<PathBuf> = Vec::new();
    for path in listed {
        if fs::metadata(&path)
            .map(|meta| meta.is_dir())
            .unwrap_or(false)
        {
            dirs.push(path);
        } else {
            files.push(path);
        }
    }
    let by_name = |a: &PathBuf, b: &PathBuf| {
        a.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .cmp(
                &b.file_name()
                    .map(|name| name.to_string_lossy().into_owned()),
            )
    };
    dirs.sort_by(by_name);
    files.sort_by(by_name);
    let rel_of = |path: &Path| -> String {
        path.strip_prefix(state)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    };
    for path in &dirs {
        let rel = rel_of(path);
        if path.is_symlink() {
            return Err(Error::wayside(format!(
                "the state directory holds a symbolic link at {rel}, \
                 which a checkpoint does not record"
            )));
        }
        read_state_dir(state, path, reader, entries)?;
    }
    for path in &files {
        let rel = rel_of(path);
        if path.is_symlink() {
            return Err(Error::wayside(format!(
                "the state directory holds a symbolic link at {rel}, \
                 which a checkpoint does not record"
            )));
        }
        if !path.is_file() {
            return Err(Error::wayside(format!(
                "the state directory holds a non regular file at {rel}, \
                 which a checkpoint does not record"
            )));
        }
        if !rel.contains('/') && RESERVED_TOP_LEVEL.contains(&rel.as_str()) {
            return Err(Error::wayside(format!(
                "the state directory holds a top level file named {rel}, \
                 which is a name restore writes, so a checkpoint does \
                 not record it"
            )));
        }
        let data = fs::read(path).map_err(|exc| {
            Error::wayside(format!(
                "the state file at {} cannot be read, {exc}",
                path.display()
            ))
        })?;
        entries.push((rel, data));
    }
    Ok(())
}

/// Assemble a checkpoint under a temporary name and rename it
/// into place.
fn publish(
    root: &Path,
    session: &str,
    sequence: i64,
    manifest: &Manifest,
    entries: &[(String, Vec<u8>)],
    memory: &[u8],
) -> Result<()> {
    let ck_dir = checkpoints_dir(root, session);
    let final_dir = ck_dir.join(sequence_name(sequence));
    if final_dir.is_symlink() {
        return Err(Error::Corrupt(format!(
            "checkpoint {sequence} of session {session} cannot be \
             published, a symbolic link stands at its name in the store"
        )));
    }
    if final_dir.exists() {
        return Err(Error::wayside(format!(
            "checkpoint {sequence} of session {session} already stands \
             in the store, which the lock should have prevented"
        )));
    }
    let tmp = ck_dir.join(format!(".tmp-{}", temporary_hex()));
    let assembled = (|| -> io::Result<()> {
        fs::create_dir_all(tmp.join(FILES_NAME))?;
        for (rel, data) in entries {
            let mut dest = tmp.join(FILES_NAME);
            for part in rel.split('/') {
                dest.push(part);
            }
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&dest, data)?;
        }
        fs::write(tmp.join(MEMORY_NAME), memory)?;
        fs::write(
            tmp.join(MANIFEST_NAME),
            format!("{}\n", render_manifest(manifest)),
        )?;
        fs::rename(&tmp, &final_dir)?;
        Ok(())
    })();
    if let Err(exc) = assembled {
        let _ = fs::remove_dir_all(&tmp);
        return Err(Error::wayside(format!(
            "the checkpoint cannot be written in the store at {}, {exc}",
            root.display()
        )));
    }
    Ok(())
}

/// Sixteen random bytes in the uuid4 shape, as lowercase hex:
/// the temporary assembly names inside the store.
fn temporary_hex() -> String {
    let mut bytes = [0u8; 16];
    match getrandom::fill(&mut bytes) {
        Ok(()) => {}
        Err(_) => {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            for (index, byte) in bytes.iter_mut().enumerate() {
                *byte = (nanos >> ((index % 8) * 8)) as u8 ^ (index as u8).wrapping_mul(37);
            }
        }
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn new_manifest(
    session: &str,
    sequence: i64,
    env: &BTreeMap<String, String>,
    entries: &[(String, Vec<u8>)],
    databases: Vec<DatabaseEntry>,
    memory: &[u8],
    forked_from: Option<ForkRef>,
) -> Manifest {
    Manifest {
        format: if databases.is_empty() {
            FORMAT_NAME.to_string()
        } else {
            FORMAT_V2.to_string()
        },
        session: session.to_string(),
        sequence,
        created_at: now_isoformat(),
        env: env.clone(),
        memory: MemoryEntry {
            sha256: sha256_hex(memory),
            size: memory.len() as i128,
        },
        files: entries
            .iter()
            .map(|(rel, data)| FileEntry {
                path: rel.clone(),
                sha256: sha256_hex(data),
                size: data.len() as i128,
            })
            .collect(),
        databases,
        forked_from,
    }
}

/// Check one declared database path against the recorded
/// path rules of WS-5 and the reservation of WS-1, as a caller
/// error naming the path and the reason (WS-11).
fn validate_declared_path(rel: &str) -> Result<()> {
    let unsafe_path = rel.is_empty()
        || rel.starts_with('/')
        || rel.contains('\\')
        || rel.contains('\0')
        || rel
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..");
    if unsafe_path {
        return Err(Error::wayside(format!(
            "the declared database path {} is not a safe relative path \
             inside the state directory",
            py_repr(rel)
        )));
    }
    if RESERVED_TOP_LEVEL.contains(&rel) {
        return Err(Error::wayside(format!(
            "the declared database path {rel} names a top level file \
             restore writes, so it cannot name a database"
        )));
    }
    Ok(())
}

/// Check the whole declaration list of a checkpoint (WS-11)
/// and return the declared paths in path order. Every refusal
/// here lands before any byte is copied and before any
/// sequence number is assigned.
fn validate_declarations(declared: &[String]) -> Result<Vec<String>> {
    for rel in declared {
        validate_declared_path(rel)?;
    }
    for (index, rel) in declared.iter().enumerate() {
        if declared[..index].contains(rel) {
            return Err(Error::wayside(format!(
                "the database path {rel} is declared twice"
            )));
        }
    }
    for rel in declared {
        for other in declared {
            if rel != other
                && (rel.as_str() == format!("{other}-wal")
                    || rel.as_str() == format!("{other}-journal"))
            {
                return Err(Error::wayside(format!(
                    "the declared database path {rel} is the sidecar of \
                     the declared database path {other}, and a sidecar \
                     rides in the bundle of its main file"
                )));
            }
        }
    }
    let mut sorted = declared.to_vec();
    sorted.sort();
    Ok(sorted)
}

/// The bundle bound refusal of WS-14, naming the bound.
fn bound_error(rel: &str) -> Error {
    Error::wayside(format!(
        "the bundle of the database at {rel} sums past the bundle \
         bound of {BUNDLE_BOUND} bytes, so the checkpoint is refused"
    ))
}

/// The state directory path a recorded relative path names.
fn join_rel(base: &Path, rel: &str) -> PathBuf {
    let mut path = base.to_path_buf();
    for part in rel.split('/') {
        path.push(part);
    }
    path
}

/// The bytes the tree walk read for one relative path, when
/// the walk read that path.
fn walked_bytes<'a>(entries: &'a [(String, Vec<u8>)], rel: &str) -> Option<&'a [u8]> {
    entries
        .iter()
        .find(|(seen, _)| seen == rel)
        .map(|(_, data)| data.as_slice())
}

/// The three names of one bundle, in bundle order: the main
/// file, its -wal appendage, and its -journal appendage.
fn bundle_names(rel: &str) -> [String; 3] {
    [
        rel.to_string(),
        format!("{rel}-wal"),
        format!("{rel}-journal"),
    ]
}

/// One reading of a bundle at the source (WS-13): the main
/// file's bytes, and the bytes of each sidecar that stands,
/// read through the reader seam, with the counted copy
/// refusing the bundle as it passes the bound of WS-14. A
/// member that cannot be read whole fails the reading as a
/// change during capture, which is the true summary of every
/// case that reaches a pass: the tree walk read every member
/// whole before the lock, so a member that cannot be read
/// whole now is a member that moved.
fn read_bundle_pass(
    state_dir: &Path,
    declared_rel: &str,
    reader: &StateTreeReader,
) -> Result<Vec<(String, Vec<u8>)>> {
    let changed = || {
        Error::wayside(format!(
            "the database at {declared_rel} changed during capture, so the \
             checkpoint is refused"
        ))
    };
    let mut members: Vec<(String, Vec<u8>)> = Vec::new();
    let mut total: u128 = 0;
    let names = bundle_names(declared_rel);
    for (index, member_rel) in names.iter().enumerate() {
        let member_path = join_rel(state_dir, member_rel);
        if index > 0 {
            match reader.member_len(&member_path) {
                Ok(Some(_)) => {}
                Ok(None) => continue,
                Err(_) => return Err(changed()),
            }
        }
        let data = reader.read_bytes(&member_path).map_err(|_| changed())?;
        total += data.len() as u128;
        if total > BUNDLE_BOUND as u128 {
            return Err(bound_error(declared_rel));
        }
        members.push((member_rel.clone(), data));
    }
    Ok(members)
}

/// Whether two readings of one bundle agree: the same members
/// in the same order, each with the same size and the same
/// hash (WS-13).
fn readings_agree(first: &[(String, Vec<u8>)], second: &[(String, Vec<u8>)]) -> bool {
    first.len() == second.len()
        && first
            .iter()
            .zip(second.iter())
            .all(|((rel_a, a), (rel_b, b))| {
                rel_a == rel_b && a.len() == b.len() && sha256_hex(a) == sha256_hex(b)
            })
}

/// Record a new checkpoint of the session from the state
/// directory. The state tree and the declared env pairs and
/// the memory document are read and checked before the lock is
/// taken, and the sequence number is then assigned and the
/// checkpoint published while the store lock is held, so the
/// read that decides the number can never be stale.
pub fn checkpoint(
    store: &Path,
    session: &str,
    state_dir: &Path,
    env: Option<&BTreeMap<String, String>>,
    memory: &[u8],
) -> Result<Manifest> {
    checkpoint_with_reader(
        store,
        session,
        state_dir,
        env,
        memory,
        &StateTreeReader::new(),
    )
}

/// Checkpoint through a named state tree reader, the seam the
/// walk error pins stage through.
pub fn checkpoint_with_reader(
    store: &Path,
    session: &str,
    state_dir: &Path,
    env: Option<&BTreeMap<String, String>>,
    memory: &[u8],
    reader: &StateTreeReader,
) -> Result<Manifest> {
    checkpoint_inner(store, session, state_dir, env, memory, &[], reader)
}

/// Record a new checkpoint declaring database bundles
/// (WS-11). Each declared path names a file inside the state
/// directory whose bundle is captured under the consistency
/// proof of WS-13, and the checkpoint is recorded in
/// wayside-checkpoint/2. With no declaration the result is a
/// wayside-checkpoint/1 checkpoint, byte for byte as the
/// landed checkpoint writes it.
pub fn checkpoint_with_databases(
    store: &Path,
    session: &str,
    state_dir: &Path,
    env: Option<&BTreeMap<String, String>>,
    memory: &[u8],
    databases: &[String],
) -> Result<Manifest> {
    checkpoint_inner(
        store,
        session,
        state_dir,
        env,
        memory,
        databases,
        &StateTreeReader::new(),
    )
}

/// Checkpoint with declared databases through a named state
/// tree reader, the seam the bundle pins stage through.
pub fn checkpoint_with_databases_and_reader(
    store: &Path,
    session: &str,
    state_dir: &Path,
    env: Option<&BTreeMap<String, String>>,
    memory: &[u8],
    databases: &[String],
    reader: &StateTreeReader,
) -> Result<Manifest> {
    checkpoint_inner(store, session, state_dir, env, memory, databases, reader)
}

fn checkpoint_inner(
    store: &Path,
    session: &str,
    state_dir: &Path,
    env: Option<&BTreeMap<String, String>>,
    memory: &[u8],
    declared: &[String],
    reader: &StateTreeReader,
) -> Result<Manifest> {
    validate_session_id(session)?;
    let env_map = env.cloned().unwrap_or_default();
    validate_env(&env_map)?;
    let declared = validate_declarations(declared)?;
    if !state_dir.is_dir() {
        return Err(Error::wayside(format!(
            "the state directory at {} does not exist or is not a directory",
            state_dir.display()
        )));
    }
    let walked = read_state_tree(state_dir, reader)?;
    // Standing and bound validation against the walked list:
    // each declared path must name a regular file the walk
    // read, and the standing members of each bundle must sum
    // within the bound, all before the lock is taken.
    let mut member_paths: Vec<String> = Vec::new();
    let mut shm_paths: Vec<String> = Vec::new();
    for rel in &declared {
        if walked_bytes(&walked, rel).is_none() {
            return Err(Error::wayside(format!(
                "the declared database path {rel} does not name a regular \
                 file standing in the state directory"
            )));
        }
        let mut total: u128 = 0;
        for name in bundle_names(rel) {
            if let Some(data) = walked_bytes(&walked, &name) {
                total += data.len() as u128;
                member_paths.push(name);
            }
        }
        if total > BUNDLE_BOUND as u128 {
            return Err(bound_error(rel));
        }
        shm_paths.push(format!("{rel}-shm"));
    }
    let _lock = locked(store)?;
    check_session_links(store, session)?;
    // The two pass proof of WS-13, under the store lock: read
    // every bundle twice and refuse a bundle whose readings
    // disagree, before any sequence is assigned.
    let mut bundles: Vec<CapturedBundle> = Vec::new();
    for rel in &declared {
        let first = read_bundle_pass(state_dir, rel, reader)?;
        let second = read_bundle_pass(state_dir, rel, reader)?;
        if !readings_agree(&first, &second) {
            return Err(Error::wayside(format!(
                "the database at {rel} changed during capture, so the \
                 checkpoint is refused"
            )));
        }
        bundles.push((rel.clone(), first));
    }
    // Recording is single: bundle member paths leave the file
    // list and the member bytes ride in the payload set once,
    // and the -shm companion of a declared database is left
    // out of the recording entirely. The manifest is built
    // from the payload set before the member bytes join it,
    // so the file list and the payload set share one copy of
    // the walked bytes.
    let mut payloads: Vec<(String, Vec<u8>)> = walked
        .into_iter()
        .filter(|(rel, _)| !member_paths.contains(rel) && !shm_paths.contains(rel))
        .collect();
    let ck_dir = checkpoints_dir(store, session);
    fs::create_dir_all(&ck_dir).map_err(|exc| {
        Error::wayside(format!(
            "the store at {} cannot be written, {exc}",
            store.display()
        ))
    })?;
    let sequences = published_sequences(store, session);
    let sequence = match sequences.last() {
        Some(last) => last.checked_add(1).ok_or_else(|| {
            Error::wayside(format!(
                "session {session} stands at sequence {last} in the \
                 store at {}, and no sequence past it can be \
                 assigned, so the checkpoint is refused",
                store.display()
            ))
        })?,
        None => 1,
    };
    let databases: Vec<DatabaseEntry> = bundles
        .iter()
        .map(|(rel, members)| {
            let mut joined: Vec<u8> = Vec::new();
            for (_, data) in members {
                joined.extend_from_slice(data);
            }
            DatabaseEntry {
                path: rel.clone(),
                bundle_sha256: sha256_hex(&joined),
                members: members
                    .iter()
                    .map(|(member_rel, data)| DatabaseMember {
                        path: member_rel.clone(),
                        sha256: sha256_hex(data),
                        size: data.len() as i128,
                    })
                    .collect(),
            }
        })
        .collect();
    let manifest = new_manifest(
        session, sequence, &env_map, &payloads, databases, memory, None,
    );
    for (_, members) in &bundles {
        for (member_rel, data) in members {
            payloads.push((member_rel.clone(), data.clone()));
        }
    }
    payloads.sort_by(|a, b| a.0.cmp(&b.0));
    publish(store, session, sequence, &manifest, &payloads, memory)?;
    Ok(manifest)
}

fn env_text(env: &BTreeMap<String, String>) -> String {
    let mut text = String::new();
    text.push_str(ENV_HEADER);
    text.push('\n');
    for (key, value) in env {
        text.push_str(&format!("{key}={value}\n"));
    }
    text
}

fn place(
    target: &Path,
    manifest: &Manifest,
    entries: &[(String, Vec<u8>)],
    memory: &[u8],
) -> Result<()> {
    if target.exists() && !target.is_dir() {
        return Err(Error::Target(format!(
            "the restore target at {} exists and is not a directory",
            target.display()
        )));
    }
    if target.is_dir() {
        if let Ok(mut read) = fs::read_dir(target) {
            if read.next().is_some() {
                return Err(Error::Target(format!(
                    "the restore target at {} is not empty, \
                     and restore never overwrites a non empty directory",
                    target.display()
                )));
            }
        }
    }
    let placed = (|| -> io::Result<()> {
        fs::create_dir_all(target)?;
        for (rel, data) in entries {
            let mut dest = target.to_path_buf();
            for part in rel.split('/') {
                dest.push(part);
            }
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&dest, data)?;
        }
        fs::write(target.join(MEMORY_FILE_NAME), memory)?;
        fs::write(target.join(ENV_FILE_NAME), env_text(&manifest.env))?;
        Ok(())
    })();
    placed.map_err(|exc| {
        Error::wayside(format!(
            "the restore target at {} cannot be written, {exc}",
            target.display()
        ))
    })
}

/// Materialize one checkpoint into the target directory.
/// Every recorded hash is verified before anything is placed,
/// so a corrupt checkpoint leaves the target exactly as it
/// was found.
pub fn restore(store: &Path, session: &str, sequence: i64, target: &Path) -> Result<Manifest> {
    validate_session_id(session)?;
    validate_sequence(sequence)?;
    let manifest = load_manifest(store, session, sequence)?;
    let (entries, memory) = read_verified(store, session, sequence, &manifest)?;
    place(target, &manifest, &entries, &memory)?;
    Ok(manifest)
}

/// Restore a checkpoint that stands earlier than the newest
/// one.
pub fn rewind(store: &Path, session: &str, sequence: i64, target: &Path) -> Result<Manifest> {
    validate_session_id(session)?;
    validate_sequence(sequence)?;
    check_session_links(store, session)?;
    require_session(store, session)?;
    let sequences = published_sequences(store, session);
    if sequences.is_empty() {
        return Err(Error::NotFound(format!(
            "session {session} holds no checkpoints in the store at {}",
            store.display()
        )));
    }
    if sequence == *sequences.last().unwrap() {
        return Err(Error::wayside(format!(
            "checkpoint {sequence} is the newest checkpoint of session \
             {session}, and rewind names an earlier one, so use restore \
             for the newest"
        )));
    }
    if !sequences.contains(&sequence) {
        return Err(Error::NotFound(format!(
            "session {session} holds no checkpoint {sequence} in the store at {}",
            store.display()
        )));
    }
    restore(store, session, sequence, target)
}

/// Start a new session whose first checkpoint copies a named
/// one. The source checkpoint is verified against its own
/// hashes before anything is copied, and the new manifest
/// records the lineage.
pub fn fork(
    store: &Path,
    source_session: &str,
    sequence: i64,
    new_session: &str,
) -> Result<Manifest> {
    validate_session_id(source_session)?;
    validate_session_id(new_session)?;
    validate_sequence(sequence)?;
    let _lock = locked(store)?;
    check_session_links(store, new_session)?;
    let manifest = load_manifest(store, source_session, sequence)?;
    let (entries, memory) = read_verified(store, source_session, sequence, &manifest)?;
    if !published_sequences(store, new_session).is_empty() {
        return Err(Error::wayside(format!(
            "session {new_session} already holds a checkpoint in \
             the store at {}, so fork will not overwrite it",
            store.display()
        )));
    }
    let ck_dir = checkpoints_dir(store, new_session);
    fs::create_dir_all(&ck_dir).map_err(|exc| {
        Error::wayside(format!(
            "the store at {} cannot be written, {exc}",
            store.display()
        ))
    })?;
    let forked = Manifest {
        format: manifest.format.clone(),
        session: new_session.to_string(),
        sequence: 1,
        created_at: now_isoformat(),
        env: manifest.env.clone(),
        memory: manifest.memory.clone(),
        files: manifest.files.clone(),
        databases: manifest.databases.clone(),
        forked_from: Some(ForkRef {
            session: source_session.to_string(),
            sequence: sequence as i128,
        }),
    };
    publish(store, new_session, 1, &forked, &entries, &memory)?;
    Ok(forked)
}

/// Return sequence and created_at for each published
/// checkpoint in order.
pub fn list_checkpoints(store: &Path, session: &str) -> Result<Vec<CheckpointInfo>> {
    validate_session_id(session)?;
    check_session_links(store, session)?;
    require_session(store, session)?;
    let mut result = Vec::new();
    for sequence in published_sequences(store, session) {
        let manifest = load_manifest(store, session, sequence)?;
        result.push(CheckpointInfo {
            sequence,
            created_at: manifest.created_at,
        });
    }
    Ok(result)
}

/// Return the validated manifest of one checkpoint.
pub fn show(store: &Path, session: &str, sequence: i64) -> Result<Manifest> {
    validate_session_id(session)?;
    validate_sequence(sequence)?;
    load_manifest(store, session, sequence)
}

/// Verify one checkpoint against its own record (WS-17):
/// read the manifest strictly under its own format's rules,
/// then re-read every recorded payload, the files and every
/// database member and the memory document, and fail with the
/// corrupt checkpoint error naming the first payload whose
/// bytes do not match their recorded hash and size. Verify
/// writes nothing and creates nothing and takes no lock, and
/// it returns the manifest of the clean checkpoint.
pub fn verify_checkpoint(store: &Path, session: &str, sequence: i64) -> Result<Manifest> {
    validate_session_id(session)?;
    validate_sequence(sequence)?;
    let manifest = load_manifest(store, session, sequence)?;
    read_verified(store, session, sequence, &manifest)?;
    Ok(manifest)
}

/// Compare the recorded databases of two checkpoints of one
/// session (WS-18): read both manifests strictly and no
/// payload bytes, and report for every database path recorded
/// on either side whether the two sides stand equal, stand
/// different, or the path stands on one side only. Compare
/// answers from the record alone and says nothing about what
/// stands in the store now.
pub fn compare_checkpoints(
    store: &Path,
    session: &str,
    first: i64,
    second: i64,
) -> Result<Comparison> {
    validate_session_id(session)?;
    validate_sequence(first)?;
    validate_sequence(second)?;
    let first_manifest = load_manifest(store, session, first)?;
    let second_manifest = load_manifest(store, session, second)?;
    let first_digests: BTreeMap<&str, &str> = first_manifest
        .databases
        .iter()
        .map(|entry| (entry.path.as_str(), entry.bundle_sha256.as_str()))
        .collect();
    let second_digests: BTreeMap<&str, &str> = second_manifest
        .databases
        .iter()
        .map(|entry| (entry.path.as_str(), entry.bundle_sha256.as_str()))
        .collect();
    let mut paths: Vec<&str> = first_digests.keys().copied().collect();
    for path in second_digests.keys() {
        if !first_digests.contains_key(path) {
            paths.push(path);
        }
    }
    paths.sort_unstable();
    let databases = paths
        .iter()
        .map(|path| {
            let standing = match (first_digests.get(path), second_digests.get(path)) {
                (Some(left), Some(right)) if left == right => DatabaseStanding::Equal,
                (Some(_), Some(_)) => DatabaseStanding::Different,
                (Some(_), None) => DatabaseStanding::OnlyFirst,
                (None, Some(_)) => DatabaseStanding::OnlySecond,
                (None, None) => DatabaseStanding::Equal,
            };
            DatabaseComparison {
                path: path.to_string(),
                standing,
            }
        })
        .collect();
    Ok(Comparison {
        session: session.to_string(),
        first,
        second,
        databases,
    })
}
