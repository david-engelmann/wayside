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

/// A validated checkpoint manifest, in the shape of the
/// wayside-checkpoint/1 format.
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub format: String,
    pub session: String,
    pub sequence: i64,
    pub created_at: String,
    pub env: BTreeMap<String, String>,
    pub memory: MemoryEntry,
    pub files: Vec<FileEntry>,
    pub forked_from: Option<ForkRef>,
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
    let keys_sound = MANIFEST_KEYS
        .iter()
        .all(|key| object_get(entries, key).is_some())
        && entries
            .iter()
            .all(|(key, _)| MANIFEST_KEYS.contains(&key.as_str()) || key == "forked_from");
    if !keys_sound {
        return Err(corrupt(path, "the field set is not the recorded one"));
    }
    match object_get(entries, "format") {
        Some(StrictValue::Str(text)) if text == FORMAT_NAME => {}
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
            if chrono::DateTime::parse_from_rfc3339(text).is_err() {
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
        format: FORMAT_NAME.to_string(),
        session: session.to_string(),
        sequence,
        created_at,
        env,
        memory,
        files,
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

/// Render a manifest exactly as the Python implementation's
/// sorted, two space indented json rendering renders it, with
/// no trailing newline. The stored manifest file carries this
/// text plus one newline, and the show command prints this text
/// plus one newline, so the two are byte identical.
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

/// Read every payload of a checkpoint and check it against the
/// manifest. Returns the file entries as relative path and
/// bytes pairs in manifest order together with the memory
/// bytes. Nothing is placed anywhere by this read, so a caller
/// can verify a whole checkpoint before it writes a single
/// byte of it.
fn read_verified(
    root: &Path,
    session: &str,
    sequence: i64,
    manifest: &Manifest,
) -> Result<VerifiedPayloads> {
    let ck = checkpoint_dir(root, session, sequence);
    let mut entries = Vec::new();
    for entry in &manifest.files {
        let mut payload = ck.join(FILES_NAME);
        for part in entry.path.split('/') {
            payload.push(part);
        }
        if !payload.is_file() {
            return Err(Error::Corrupt(format!(
                "checkpoint {sequence} of session {session} is corrupt, \
                 the payload for {} is missing",
                entry.path
            )));
        }
        let data = fs::read(&payload).map_err(|exc| {
            Error::Corrupt(format!(
                "checkpoint {sequence} of session {session} is corrupt, \
                 the payload for {} cannot be read, {exc}",
                entry.path
            ))
        })?;
        if data.len() as i128 != entry.size || sha256_hex(&data) != entry.sha256 {
            return Err(Error::Corrupt(format!(
                "checkpoint {sequence} of session {session} is corrupt, \
                 the payload for {} does not match its recorded hash and size",
                entry.path
            )));
        }
        entries.push((entry.path.clone(), data));
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

/// The reader the state tree walk lists directories through.
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
pub struct StateTreeReader {
    lister: Box<Lister>,
}

impl StateTreeReader {
    /// The production reader, listing with fs::read_dir.
    pub fn new() -> StateTreeReader {
        StateTreeReader {
            lister: Box::new(|dir| {
                let mut paths = Vec::new();
                for entry in fs::read_dir(dir)? {
                    paths.push(entry?.path());
                }
                Ok(paths)
            }),
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
        }
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<PathBuf>> {
        (self.lister)(dir)
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
    memory: &[u8],
    forked_from: Option<ForkRef>,
) -> Manifest {
    Manifest {
        format: FORMAT_NAME.to_string(),
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
        forked_from,
    }
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
    validate_session_id(session)?;
    let env_map = env.cloned().unwrap_or_default();
    validate_env(&env_map)?;
    if !state_dir.is_dir() {
        return Err(Error::wayside(format!(
            "the state directory at {} does not exist or is not a directory",
            state_dir.display()
        )));
    }
    let entries = read_state_tree(state_dir, reader)?;
    let _lock = locked(store)?;
    check_session_links(store, session)?;
    let ck_dir = checkpoints_dir(store, session);
    fs::create_dir_all(&ck_dir).map_err(|exc| {
        Error::wayside(format!(
            "the store at {} cannot be written, {exc}",
            store.display()
        ))
    })?;
    let sequences = published_sequences(store, session);
    let sequence = sequences.last().map(|last| last + 1).unwrap_or(1);
    let manifest = new_manifest(session, sequence, &env_map, &entries, memory, None);
    publish(store, session, sequence, &manifest, &entries, memory)?;
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
    let new_manifest = new_manifest(
        new_session,
        1,
        &manifest.env,
        &entries,
        &memory,
        Some(ForkRef {
            session: source_session.to_string(),
            sequence: sequence as i128,
        }),
    );
    publish(store, new_session, 1, &new_manifest, &entries, &memory)?;
    Ok(new_manifest)
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
