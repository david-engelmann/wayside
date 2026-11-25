//! The manifest validators, a parsed document judged against the recorded shape.

use crate::capture::bundle_names;
use crate::errors::{Error, Result};
use crate::helpers::{py_repr, validate_env_pairs, validate_session_id};
use crate::model::{DatabaseEntry, DatabaseMember, FileEntry, ForkRef, Manifest, MemoryEntry};
use crate::reader::StrictValue;
use crate::{FORMAT_NAME, FORMAT_V2, RESERVED_TOP_LEVEL};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;

pub(crate) fn corrupt(path: &Path, why: &str) -> Error {
    Error::Corrupt(format!("manifest at {} is corrupt, {why}", path.display()))
}

fn object_get<'a>(entries: &'a [(String, StrictValue)], key: &str) -> Option<&'a StrictValue> {
    entries.iter().find(|(seen, _)| seen == key).map(|(_, v)| v)
}

/// Whether an object's pairs carry exactly the named fields
/// (WS-A39): the pair count equals the field count and every
/// named field is present. The strict reader refuses a
/// repeated key as it parses, so count plus presence is
/// exactness, and no key can stand twice to hide a missing
/// one behind the count.
fn has_exact_fields(pairs: &[(String, StrictValue)], keys: &[&str]) -> bool {
    pairs.len() == keys.len() && keys.iter().all(|key| object_get(pairs, key).is_some())
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

/// Whether a relative path is unsafe inside the state
/// directory (WS-5): empty, absolute, carrying a backslash or
/// a NUL, or carrying an empty, dot, or dot dot component.
/// The one statement of the predicate the recorded path check
/// and the declared path check share (WS-A19).
pub(crate) fn path_is_unsafe(rel: &str) -> bool {
    rel.is_empty()
        || rel.starts_with('/')
        || rel.contains('\\')
        || rel.contains('\0')
        || rel
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
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
    if path_is_unsafe(&text) {
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
    if let Some((first, _)) = text.split_once('/') {
        if RESERVED_TOP_LEVEL.contains(&first) {
            return Err(corrupt(
                path,
                &format!(
                    "the recorded path {} stands below the name of a file \
                     restore writes, so it is not a path a checkpoint may record",
                    py_repr(&text)
                ),
            ));
        }
    }
    Ok(text)
}

/// Whether a recorded path list stands in path order with no
/// path repeated (WS-A18): the order and uniqueness judgement
/// the manifest validators share, judged on adjacent entries,
/// which decide the fact directly (WS-A35).
fn paths_in_order_without_repeats(paths: &[String]) -> bool {
    paths.windows(2).all(|pair| pair[0] < pair[1])
}

/// Whether a recorded path list repeats no path, in any order
/// (WS-A18).
fn paths_without_repeats(paths: &[String]) -> bool {
    let mut sorted = paths.to_vec();
    sorted.sort();
    sorted.dedup();
    sorted.len() == paths.len()
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
        StrictValue::Int(number) if *number >= 0 && *number <= i128::from(u64::MAX) => Ok(*number),
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
                if has_exact_fields(pairs, &["path", "bundle_sha256", "members"]) =>
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
        let bundle_slots = bundle_names(&rel);
        let mut next_slot = 0usize;
        let mut members = Vec::new();
        for member in member_values.iter() {
            let member_pairs = match member {
                StrictValue::Object(pairs)
                    if has_exact_fields(pairs, &["path", "sha256", "size"]) =>
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
    if !paths_in_order_without_repeats(&database_paths) {
        return Err(corrupt(
            path,
            "the database entries are not in path order or repeat a path",
        ));
    }
    if !paths_without_repeats(&recorded_paths) {
        return Err(corrupt(path, "the database entries repeat a path"));
    }
    let file_path_set: BTreeSet<&str> = file_paths.iter().map(String::as_str).collect();
    for recorded in &recorded_paths {
        if file_path_set.contains(recorded.as_str()) {
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

/// Validate the directories field of a wayside-checkpoint/2
/// manifest under WS-21: a non empty list of recorded paths,
/// each satisfying the recorded path rules and the reserved
/// name refusal of validate_recorded_path, standing in path
/// order with no path repeated, none of them a recorded file
/// or member path, and none standing as a proper prefix of a
/// recorded file or member path.
fn validate_directories(
    value: &StrictValue,
    path: &Path,
    file_paths: &[String],
    databases: &[DatabaseEntry],
) -> Result<Vec<String>> {
    let items = match value {
        StrictValue::Array(items) => items,
        _ => return Err(corrupt(path, "the directories entry is not a list")),
    };
    if items.is_empty() {
        return Err(corrupt(
            path,
            "the directories entry is empty, and a manifest that records \
             directories names at least one",
        ));
    }
    let mut directories = Vec::new();
    for item in items {
        directories.push(validate_recorded_path(item, path)?);
    }
    if !paths_in_order_without_repeats(&directories) {
        return Err(corrupt(
            path,
            "the directory entries are not in path order or repeat a path",
        ));
    }
    // The files list stands in path order here, refused
    // otherwise by validate_manifest before this call, and
    // the member paths are sorted once here, so each
    // judgement below is decided by the first path at or
    // after the directory or its prefix: if that path is
    // not the directory and does not carry the prefix, no
    // later path does (WS-A36). The standing form scanned
    // both lists for every directory.
    let mut member_paths: Vec<&str> = databases
        .iter()
        .flat_map(|database| database.members.iter().map(|member| member.path.as_str()))
        .collect();
    // The stands below judgements are decided by a
    // directory's ancestors, its prefixes cut at the slash
    // boundaries: a recorded path stands above the directory
    // exactly when it equals one of them. The file list
    // stands in path order here, refused otherwise by
    // validate_manifest before this call, so each ancestor
    // is answered by binary search, and the member paths
    // stand in bundle order, so they are gathered once here
    // into a set of the same slices (WS-A65). The standing
    // form scanned both lists for every directory and built
    // a candidate prefix string for each pair it tested.
    let member_path_set: BTreeSet<&str> = member_paths.iter().copied().collect();
    member_paths.sort_unstable();
    for directory in &directories {
        if file_paths.binary_search(directory).is_ok() {
            return Err(corrupt(
                path,
                &format!(
                    "the path {directory} stands both in the files list and \
                     in the directories list"
                ),
            ));
        }
        if member_paths.binary_search(&directory.as_str()).is_ok() {
            return Err(corrupt(
                path,
                &format!(
                    "the path {directory} stands both in a database entry \
                     and in the directories list"
                ),
            ));
        }
        let prefix = format!("{directory}/");
        let first_file = file_paths.partition_point(|file| file.as_str() < prefix.as_str());
        if file_paths
            .get(first_file)
            .is_some_and(|file| file.starts_with(prefix.as_str()))
        {
            return Err(corrupt(
                path,
                &format!("the directory {directory} stands above a recorded file"),
            ));
        }
        let first_member = member_paths.partition_point(|member| *member < prefix.as_str());
        if member_paths
            .get(first_member)
            .is_some_and(|member| member.starts_with(prefix.as_str()))
        {
            return Err(corrupt(
                path,
                &format!("the directory {directory} stands above a database member"),
            ));
        }
        if directory.match_indices('/').any(|(index, _)| {
            file_paths
                .binary_search_by(|file| file.as_str().cmp(&directory[..index]))
                .is_ok()
        }) {
            return Err(corrupt(
                path,
                &format!("the directory {directory} stands below a recorded file"),
            ));
        }
        if directory
            .match_indices('/')
            .any(|(index, _)| member_path_set.contains(&directory[..index]))
        {
            return Err(corrupt(
                path,
                &format!("the directory {directory} stands below a database member"),
            ));
        }
    }
    Ok(directories)
}

pub(crate) fn validate_manifest(
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
                || (is_v2 && (key == "databases" || key == "directories"))
        })
        && (!is_v2
            || object_get(entries, "databases").is_some()
            || object_get(entries, "directories").is_some());
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
        Some(StrictValue::Object(pairs)) if has_exact_fields(pairs, &["sha256", "size"]) => {
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
            StrictValue::Object(pairs) if has_exact_fields(pairs, &["path", "sha256", "size"]) => {
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
    if !paths_in_order_without_repeats(&seen) {
        return Err(corrupt(
            path,
            "the file entries are not in path order or repeat a path",
        ));
    }
    let databases = if is_v2 && object_get(entries, "databases").is_some() {
        validate_databases(object_get(entries, "databases").unwrap(), path, &seen)?
    } else {
        Vec::new()
    };
    let directories = if is_v2 && object_get(entries, "directories").is_some() {
        validate_directories(
            object_get(entries, "directories").unwrap(),
            path,
            &seen,
            &databases,
        )?
    } else {
        Vec::new()
    };
    let forked_from = match object_get(entries, "forked_from") {
        None => None,
        Some(StrictValue::Object(pairs)) if has_exact_fields(pairs, &["session", "sequence"]) => {
            let fork_session = match object_get(pairs, "session") {
                Some(StrictValue::Str(text)) if validate_session_id(text).is_ok() => text.clone(),
                _ => {
                    return Err(corrupt(
                        path,
                        "the forked_from session is not a safe session id",
                    ))
                }
            };
            let fork_sequence = match object_get(pairs, "sequence") {
                Some(StrictValue::Int(number))
                    if *number >= 1 && *number <= i128::from(u64::MAX) =>
                {
                    *number
                }
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
        directories,
        forked_from,
    })
}
