//! The store, its paths, its lock, and the reading of recorded checkpoints.

use crate::capture::join_rel;
use crate::errors::{Error, Result, VerifiedPayloads};
use crate::helpers::sha256_hex;
use crate::manifest::{corrupt, validate_manifest};
use crate::model::Manifest;
use crate::reader::{duplicate_key_of, StrictValue};
use crate::{CHECKPOINTS_NAME, FILES_NAME, LOCK_NAME, MANIFEST_NAME, MEMORY_NAME, SESSIONS_NAME};
use fs2::FileExt;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;

/// The current instant in the shape Python's isoformat writes
/// for a UTC datetime: a T separator, a +00:00 offset, and a six
/// digit fraction present exactly when the microsecond part is
/// nonzero.
pub(crate) fn now_isoformat() -> String {
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

pub(crate) fn checkpoints_dir(root: &Path, session: &str) -> PathBuf {
    session_dir(root, session).join(CHECKPOINTS_NAME)
}

pub(crate) fn create_checkpoints_dir(root: &Path, session: &str) -> Result<()> {
    let dir = checkpoints_dir(root, session);
    fs::create_dir_all(&dir).map_err(|exc| {
        Error::wayside(format!(
            "the store at {} cannot be written, {exc}",
            root.display()
        ))
    })
}

pub(crate) fn sequence_name(sequence: i64) -> String {
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
pub(crate) fn check_session_links(root: &Path, session: &str) -> Result<()> {
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

pub(crate) struct StoreLock {
    file: fs::File,
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Take the exclusive flock on the store lock file, creating
/// the store root and the lock file when they do not stand.
pub(crate) fn locked(root: &Path) -> Result<StoreLock> {
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
/// A digit name counts only when it is the canonical rendering
/// of its value, the name sequence_name writes, and only when
/// that value is one a writer assigns, which is any value from
/// 1 upward, so an alias spelling with extra leading zeros
/// names no published checkpoint, and neither does the
/// canonical spelling of zero, and both are passed over like
/// any other residue (WS-A56).
pub(crate) fn published_sequences(root: &Path, session: &str) -> Vec<i64> {
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
                if sequence >= 1 && name == sequence_name(sequence) {
                    sequences.push(sequence);
                }
            }
        }
    }
    sequences.sort_unstable();
    sequences
}

pub(crate) fn require_session(root: &Path, session: &str) -> Result<()> {
    if session_dir(root, session).is_dir() {
        Ok(())
    } else {
        Err(Error::NotFound(format!(
            "session {session} is not in the store at {}",
            root.display()
        )))
    }
}

/// The no checkpoint refusal: the one statement of the sentence
/// every operation answers when a session holds no checkpoint of
/// the named sequence in this store.
pub(crate) fn no_checkpoint(root: &Path, session: &str, sequence: i64) -> Error {
    Error::NotFound(format!(
        "session {session} holds no checkpoint {sequence} in the store at {}",
        root.display()
    ))
}

/// The corrupt checkpoint refusal (WS-A52): the one statement
/// of the prefix every refusal that names a checkpoint corrupt
/// from the payload reading family opens with, the sequence
/// and the session named, with the caller's tail stating what
/// stands corrupt about it.
fn corrupt_checkpoint(session: &str, sequence: i64, tail: &str) -> Error {
    Error::Corrupt(format!(
        "checkpoint {sequence} of session {session} is corrupt, {tail}"
    ))
}

/// Read and strictly validate one manifest, changing nothing
/// on disk.
pub(crate) fn load_manifest(root: &Path, session: &str, sequence: i64) -> Result<Manifest> {
    let ck = checkpoint_dir(root, session, sequence);
    check_checkpoint_links(root, session, sequence)?;
    let path = ck.join(MANIFEST_NAME);
    if !path.is_file() {
        if ck.is_dir() {
            return Err(corrupt_checkpoint(
                session,
                sequence,
                "its manifest is missing or is not a regular file, \
                 so the checkpoint cannot be read",
            ));
        }
        return Err(no_checkpoint(root, session, sequence));
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

/// Read one recorded byte string from its path inside a
/// checkpoint and check it against its recorded hash and
/// size. The subject phrase names the byte string in the
/// refusal tails: the payload for a recorded path, or the
/// memory document.
fn read_recorded(
    path: &Path,
    session: &str,
    sequence: i64,
    subject: &str,
    sha256: &str,
    size: i128,
) -> Result<Vec<u8>> {
    if !path.is_file() {
        return Err(corrupt_checkpoint(
            session,
            sequence,
            &format!("{subject} is missing or is not a regular file"),
        ));
    }
    let data = fs::read(path).map_err(|exc| {
        corrupt_checkpoint(
            session,
            sequence,
            &format!("{subject} cannot be read, {exc}"),
        )
    })?;
    if data.len() as i128 != size || sha256_hex(&data) != sha256 {
        return Err(corrupt_checkpoint(
            session,
            sequence,
            &format!("{subject} does not match its recorded hash and size"),
        ));
    }
    Ok(data)
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
    read_recorded(
        &join_rel(&ck.join(FILES_NAME), rel),
        session,
        sequence,
        &format!("the payload for {rel}"),
        sha256,
        size,
    )
}

/// Read every payload of a checkpoint and check it against the
/// manifest: the recorded files, then every database member,
/// then the memory document. Returns the payload entries as
/// relative path and bytes pairs in that order together with
/// the memory bytes. Nothing is placed anywhere by this read,
/// so a caller can verify a whole checkpoint before it writes
/// a single byte of it.
pub(crate) fn read_verified(
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
    let memory = read_recorded(
        &ck.join(MEMORY_NAME),
        session,
        sequence,
        "the memory document",
        &manifest.memory.sha256,
        manifest.memory.size,
    )?;
    Ok((entries, memory))
}

/// Check every payload of a checkpoint against the manifest
/// without holding the payloads: the recorded files, then
/// every database member, then the memory document, each
/// payload's bytes dropped before the next one is read. The
/// refusals, their order, and the first payload named stand
/// as read_verified's own; read_verified keeps its returning
/// form for the callers that place the bytes it reads.
pub(crate) fn verify_payloads(
    root: &Path,
    session: &str,
    sequence: i64,
    manifest: &Manifest,
) -> Result<()> {
    let ck = checkpoint_dir(root, session, sequence);
    for entry in &manifest.files {
        read_payload(
            &ck,
            session,
            sequence,
            &entry.path,
            &entry.sha256,
            entry.size,
        )?;
    }
    for database in &manifest.databases {
        for member in &database.members {
            read_payload(
                &ck,
                session,
                sequence,
                &member.path,
                &member.sha256,
                member.size,
            )?;
        }
    }
    read_recorded(
        &ck.join(MEMORY_NAME),
        session,
        sequence,
        "the memory document",
        &manifest.memory.sha256,
        manifest.memory.size,
    )?;
    Ok(())
}
