//! The operations, checkpoint and restore and rewind and fork and the readers.

use crate::capture::{
    bound_error, bundle_names, changed_during_capture, join_rel, new_manifest, publish,
    read_bundle_pass, read_state_tree, readings_agree, validate_declarations, write_entries,
    StateTreeReader,
};
use crate::errors::{CapturedBundle, Error, Result};
use crate::helpers::{
    hex_lowercase, sha256_hex, validate_env, validate_sequence, validate_session_id,
};
use crate::model::{
    CheckpointInfo, Comparison, DatabaseComparison, DatabaseEntry, DatabaseMember,
    DatabaseStanding, ForkRef, Manifest,
};
use crate::store::{
    check_session_links, checkpoints_dir, load_manifest, locked, now_isoformat,
    published_sequences, read_verified, require_session,
};
use crate::{BUNDLE_BOUND, ENV_FILE_NAME, ENV_HEADER, MEMORY_FILE_NAME};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::Path;

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
/// proof of WS-13. The checkpoint is recorded in
/// wayside-checkpoint/2 when it declares a database or records
/// an empty directory, and in wayside-checkpoint/1 otherwise
/// (WS-21). With no declaration the call writes byte for byte
/// what the landed checkpoint writes.
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
    let (mut walked, walked_directories) = read_state_tree(state_dir, reader)?;
    // Standing and bound validation against the walked list:
    // each declared path must name a regular file the walk
    // read, and the standing members of each bundle must sum
    // within the bound, all before the lock is taken.
    let mut walked_index: BTreeMap<&str, &[u8]> = BTreeMap::new();
    for (seen, data) in &walked {
        walked_index.entry(seen.as_str()).or_insert(data.as_slice());
    }
    let mut member_paths: BTreeSet<String> = BTreeSet::new();
    let mut shm_paths: BTreeSet<String> = BTreeSet::new();
    for rel in &declared {
        if !walked_index.contains_key(rel.as_str()) {
            return Err(Error::wayside(format!(
                "the declared database path {rel} does not name a regular \
                 file standing in the state directory"
            )));
        }
        let mut total: u128 = 0;
        for name in bundle_names(rel) {
            if let Some(data) = walked_index.get(name.as_str()) {
                total += data.len() as u128;
                member_paths.insert(name);
            }
        }
        if total > BUNDLE_BOUND as u128 {
            return Err(bound_error(rel));
        }
        shm_paths.insert(format!("{rel}-shm"));
    }
    // The walked copies of the bundle member bytes and of the
    // -shm companions are never read again. The two pass proof
    // below reads every member fresh from the state tree, and
    // the payload set built after the proof drops both kinds
    // from the walked list unexamined. The validation above
    // has already taken every length it needs from them, so
    // their buffers are released here, before the lock, and a
    // capture's peak no longer holds the walked copy of a
    // bundle beside the two readings of its proof (WS-A4).
    for (rel, data) in &mut walked {
        if member_paths.contains(rel) || shm_paths.contains(rel) {
            *data = Vec::new();
        }
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
        let first_hashes: Vec<String> = first.iter().map(|(_, data)| sha256_hex(data)).collect();
        if !readings_agree(&first, &first_hashes, &second) {
            return Err(changed_during_capture(rel));
        }
        bundles.push((rel.clone(), first, first_hashes));
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
    // The recorded directories of WS-20: a walked directory is
    // recorded exactly when no recorded payload stands at or
    // below it, and the recorded payloads are the file entries
    // of the manifest and the members of its bundles, so the
    // judgement runs on the payload set and the member set,
    // with the -shm companions already out of both. The
    // payload set stands in path order here, the walked
    // entries sorted and the filter above preserving their
    // order, and the member set is a BTreeSet, so each
    // judgement is decided by the first payload at or after
    // the directory's prefix and the first member at or
    // after it: if neither carries the prefix, no later one
    // does (WS-A36). The standing form scanned both sets
    // for every directory.
    let recorded_directories: Vec<String> = walked_directories
        .iter()
        .filter(|directory| {
            let prefix = format!("{directory}/");
            let first_payload = payloads.partition_point(|(rel, _)| rel.as_str() < prefix.as_str());
            let payload_below = payloads
                .get(first_payload)
                .is_some_and(|(rel, _)| rel.starts_with(prefix.as_str()));
            let member_below = member_paths
                .range(prefix.clone()..)
                .next()
                .is_some_and(|rel| rel.starts_with(prefix.as_str()));
            !payload_below && !member_below
        })
        .cloned()
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
        .map(|(rel, members, hashes)| {
            // The bundle digest is the hash of the member
            // bytes in bundle order, fed to one hasher member
            // by member: the joined copy this replaces was a
            // further copy of the bundle's bytes held only to
            // be hashed once (WS-A4).
            let mut hasher = Sha256::new();
            for (_, data) in members {
                hasher.update(data);
            }
            DatabaseEntry {
                path: rel.clone(),
                bundle_sha256: hex_lowercase(&hasher.finalize()),
                members: members
                    .iter()
                    .zip(hashes.iter())
                    .map(|((member_rel, data), hash)| DatabaseMember {
                        path: member_rel.clone(),
                        sha256: hash.clone(),
                        size: data.len() as i128,
                    })
                    .collect(),
            }
        })
        .collect();
    let manifest = new_manifest(
        session,
        sequence,
        &env_map,
        &payloads,
        databases,
        recorded_directories,
        memory,
        None,
    );
    for (_, members, _) in bundles {
        for (member_rel, data) in members {
            payloads.push((member_rel, data));
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
        match fs::read_dir(target) {
            Ok(mut read) => {
                if read.next().is_some() {
                    return Err(Error::Target(format!(
                        "the restore target at {} is not empty, \
                         and restore never overwrites a non empty directory",
                        target.display()
                    )));
                }
            }
            Err(exc) => {
                return Err(Error::Target(format!(
                    "the restore target at {} cannot be listed, \
                     and restore never writes into a directory it \
                     cannot show is empty, {exc}",
                    target.display()
                )));
            }
        }
    }
    let placed = (|| -> io::Result<()> {
        fs::create_dir_all(target)?;
        write_entries(target, entries)?;
        fs::write(target.join(MEMORY_FILE_NAME), memory)?;
        fs::write(target.join(ENV_FILE_NAME), env_text(&manifest.env))?;
        for directory in &manifest.directories {
            fs::create_dir_all(join_rel(target, directory))?;
        }
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
        session: new_session.to_string(),
        sequence: 1,
        created_at: now_isoformat(),
        forked_from: Some(ForkRef {
            session: source_session.to_string(),
            sequence: sequence as i128,
        }),
        ..manifest
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
