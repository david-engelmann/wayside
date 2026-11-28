//! The capture pipeline, a state tree read into a bundle under its rules.

use crate::errors::{Error, Lister, Result, WalkedTree};
use crate::helpers::{hex_lowercase, py_repr, sha256_hex};
use crate::manifest::path_is_unsafe;
use crate::model::{DatabaseEntry, FileEntry, ForkRef, Manifest, MemoryEntry};
use crate::render::{manifest_format, render_manifest};
use crate::store::{checkpoints_dir, now_isoformat, sequence_name};
use crate::{BUNDLE_BOUND, FILES_NAME, MANIFEST_NAME, MEMORY_NAME, RESERVED_TOP_LEVEL};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

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
/// name. Staged readers override one path from a named read or
/// a named standing probe onward, from the second read or the
/// second standing probe for the readers staging a change
/// between the passes and from the first bundle read for the
/// serving reader, so a pin can stage a bundle that changes
/// between the two passes of WS-13 deterministically, without
/// racing a real writer. The default reader reads and stats the
/// files themselves.
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
/// by path, and return the directories standing in the tree,
/// sorted by path, beside the files. Symbolic links and non
/// regular files are refused, because a link can name bytes
/// outside the state directory and a checkpoint records only
/// what the caller can see in the tree itself. An entry whose
/// name is not valid UTF-8 is refused too, because the record
/// carries names as text, and a name the record cannot carry
/// exactly it does not carry at all (WS-A27). A directory
/// that cannot be listed fails the read, naming that
/// directory, instead of publishing a partial tree as if it
/// were complete.
pub(crate) fn read_state_tree(state: &Path, reader: &StateTreeReader) -> Result<WalkedTree> {
    let mut entries = Vec::new();
    let mut dirs = Vec::new();
    read_state_dir(state, state, reader, &mut entries, &mut dirs)?;
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    dirs.sort();
    Ok((entries, dirs))
}

fn refuse_unrecordable_entry(path: &Path, rel: &str, is_link: bool, noun: &str) -> Result<()> {
    if is_link {
        return Err(Error::wayside(format!(
            "the state directory holds a symbolic link at {rel}, \
             which a checkpoint does not record"
        )));
    }
    // A name carrying a backslash cannot be recorded:
    // rel_of renders the backslash as a slash, so the
    // entry would stand in the record under a path
    // that is not its own (WS-A33).
    if path
        .file_name()
        .is_some_and(|name| name.to_string_lossy().contains('\\'))
    {
        return Err(Error::wayside(format!(
            "the state directory holds an entry whose name carries \
             a backslash at {rel}, which a checkpoint does not record"
        )));
    }
    if !rel.contains('/') && RESERVED_TOP_LEVEL.contains(&rel) {
        return Err(Error::wayside(format!(
            "the state directory holds a top level {noun} named {rel}, \
             which is a name restore writes, so a checkpoint does \
             not record it"
        )));
    }
    Ok(())
}

fn read_state_dir(
    state: &Path,
    dir: &Path,
    reader: &StateTreeReader,
    entries: &mut Vec<(String, Vec<u8>)>,
    dirs_out: &mut Vec<String>,
) -> Result<()> {
    let listed = reader.list(dir).map_err(|exc| {
        Error::wayside(format!(
            "the state directory at {} cannot be listed, {exc}",
            dir.display()
        ))
    })?;
    // Each entry is classified by the one stat that does not
    // follow links, which answers whether the entry is a link,
    // a directory, or a regular file in the one reading
    // (WS-A37). A link is classified by the metadata that
    // follows it, as the walk has always classified it, and
    // each entry's kind travels through the name sorts into
    // the two loops below, whose checks read the carried kind
    // instead of statting the entry again.
    let mut dirs: Vec<(PathBuf, bool)> = Vec::new();
    let mut files: Vec<(PathBuf, bool, bool)> = Vec::new();
    for path in listed {
        // A name that is not valid UTF-8 cannot be recorded:
        // rel_of renders it lossily, so it would stand in the
        // record under a mangled rendering and be restored
        // under that rendering, silently, and two names
        // mangling to one rendering would publish a manifest
        // repeating a path, which every later read refuses as
        // corrupt (WS-A27). Refuse the entry here, in the
        // family of the link and non regular file refusals
        // below.
        if path.file_name().is_some_and(|name| name.to_str().is_none()) {
            return Err(Error::wayside(format!(
                "the state directory at {} holds an entry whose name is \
                 not valid UTF-8 text, which a checkpoint does not record",
                dir.display()
            )));
        }
        let link_meta = fs::symlink_metadata(&path).ok();
        let is_link = link_meta
            .as_ref()
            .is_some_and(|meta| meta.file_type().is_symlink());
        if is_link {
            if fs::metadata(&path)
                .map(|meta| meta.is_dir())
                .unwrap_or(false)
            {
                dirs.push((path, true));
            } else {
                files.push((path, true, false));
            }
        } else if let Some(meta) = link_meta {
            if meta.is_dir() {
                dirs.push((path, false));
            } else {
                files.push((path, false, meta.is_file()));
            }
        } else {
            files.push((path, false, false));
        }
    }
    let by_name = |a: &Path, b: &Path| {
        a.file_name()
            .map(|name| name.to_string_lossy())
            .cmp(&b.file_name().map(|name| name.to_string_lossy()))
    };
    dirs.sort_by(|a, b| by_name(&a.0, &b.0));
    files.sort_by(|a, b| by_name(&a.0, &b.0));
    let rel_of = |path: &Path| -> String {
        path.strip_prefix(state)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    };
    for (path, is_link) in &dirs {
        let rel = rel_of(path);
        refuse_unrecordable_entry(path, &rel, *is_link, "directory")?;
        dirs_out.push(rel);
        read_state_dir(state, path, reader, entries, dirs_out)?;
    }
    for (path, is_link, is_regular) in &files {
        let rel = rel_of(path);
        refuse_unrecordable_entry(path, &rel, *is_link, "file")?;
        if !is_regular {
            return Err(Error::wayside(format!(
                "the state directory holds a non regular file at {rel}, \
                 which a checkpoint does not record"
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

/// Write each entry under a root, creating the parent
/// directories of each recorded path (WS-A21): the write
/// loop publish and place share, splitting each path on its
/// separators and pushing the parts onto the root.
pub(crate) fn write_entries(root: &Path, entries: &[(String, Vec<u8>)]) -> io::Result<()> {
    let mut created: BTreeSet<PathBuf> = BTreeSet::new();
    for (rel, data) in entries {
        let dest = join_rel(root, rel);
        if let Some(parent) = dest.parent() {
            if created.insert(parent.to_path_buf()) {
                fs::create_dir_all(parent)?;
            }
        }
        fs::write(&dest, data)?;
    }
    Ok(())
}

/// Assemble a checkpoint under a temporary name and rename it
/// into place.
pub(crate) fn publish(
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
        write_entries(&tmp.join(FILES_NAME), entries)?;
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
            // The fallback name also carries the process id and
            // a counter that never repeats inside the process,
            // so two names derived in the same process differ
            // even at a frozen clock (WS-A7).
            bytes[8..12].copy_from_slice(&std::process::id().to_le_bytes());
            static FALLBACK_COUNTER: AtomicU64 = AtomicU64::new(0);
            let count = FALLBACK_COUNTER.fetch_add(1, Ordering::SeqCst);
            bytes[12..16].copy_from_slice(&(count as u32).to_le_bytes());
        }
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    hex_lowercase(&bytes)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn new_manifest(
    session: &str,
    sequence: i64,
    env: &BTreeMap<String, String>,
    entries: &[(String, Vec<u8>)],
    databases: Vec<DatabaseEntry>,
    directories: Vec<String>,
    memory: &[u8],
    forked_from: Option<ForkRef>,
) -> Manifest {
    Manifest {
        format: manifest_format(&databases, &directories).to_string(),
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
        directories,
        forked_from,
    }
}

/// Check one declared database path against the recorded
/// path rules of WS-5 and the reservation of WS-1, as a caller
/// error naming the path and the reason (WS-11).
fn validate_declared_path(rel: &str) -> Result<()> {
    if path_is_unsafe(rel) {
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
pub(crate) fn validate_declarations(declared: &[String]) -> Result<Vec<String>> {
    for rel in declared {
        validate_declared_path(rel)?;
    }
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for rel in declared {
        if !seen.insert(rel.as_str()) {
            return Err(Error::wayside(format!(
                "the database path {rel} is declared twice"
            )));
        }
    }
    for rel in declared {
        let base = rel
            .strip_suffix("-wal")
            .or_else(|| rel.strip_suffix("-journal"));
        if let Some(other) = base {
            if seen.contains(other) {
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
pub(crate) fn bound_error(rel: &str) -> Error {
    Error::wayside(format!(
        "the bundle of the database at {rel} sums past the bundle \
         bound of {BUNDLE_BOUND} bytes, so the checkpoint is refused"
    ))
}

/// The changed during capture refusal of WS-13 (WS-A42): the
/// one statement of the text the bundle pass and the two pass
/// agreement judgement in checkpoint_inner share.
pub(crate) fn changed_during_capture(rel: &str) -> Error {
    Error::wayside(format!(
        "the database at {rel} changed during capture, so the \
         checkpoint is refused"
    ))
}

/// The path a recorded relative path names under a base path,
/// its slash separated parts pushed onto the base one by one.
pub(crate) fn join_rel(base: &Path, rel: &str) -> PathBuf {
    let mut path = base.to_path_buf();
    for part in rel.split('/') {
        path.push(part);
    }
    path
}

/// The three names of one bundle, in bundle order: the main
/// file, its -wal appendage, and its -journal appendage.
pub(crate) fn bundle_names(rel: &str) -> [String; 3] {
    [
        rel.to_string(),
        format!("{rel}-wal"),
        format!("{rel}-journal"),
    ]
}

/// One reading of a bundle at the source (WS-13): the main
/// file's bytes, and the bytes of each sidecar that stands,
/// read through the reader seam, with the counted copy
/// refusing the bundle as it passes the bound of WS-14. Each
/// member's standing size is probed before it is read and
/// judged against the same bound, so a member that has grown
/// past the bound since the walk is refused before its bytes
/// are read whole under the lock (WS-A3). A member that
/// cannot be read whole fails the reading as a change during
/// capture, which is the true summary of every case that
/// reaches a pass: the tree walk read every member whole
/// before the lock, so a member that cannot be read whole
/// now is a member that moved.
pub(crate) fn read_bundle_pass(
    state_dir: &Path,
    declared_rel: &str,
    reader: &StateTreeReader,
) -> Result<Vec<(String, Vec<u8>)>> {
    let mut members: Vec<(String, Vec<u8>)> = Vec::new();
    let mut total: u128 = 0;
    let names = bundle_names(declared_rel);
    for (index, member_rel) in names.iter().enumerate() {
        let member_path = join_rel(state_dir, member_rel);
        match reader.member_len(&member_path) {
            Ok(Some(len)) => {
                if total + len as u128 > BUNDLE_BOUND as u128 {
                    return Err(bound_error(declared_rel));
                }
            }
            Ok(None) => {
                if index == 0 {
                    return Err(changed_during_capture(declared_rel));
                }
                continue;
            }
            Err(_) => return Err(changed_during_capture(declared_rel)),
        }
        let data = reader
            .read_bytes(&member_path)
            .map_err(|_| changed_during_capture(declared_rel))?;
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
/// hash (WS-13). The first reading's hashes are the caller's,
/// computed once and shared with the manifest construction,
/// so no member of the first reading is hashed twice for the
/// two judgements (WS-A4).
pub(crate) fn readings_agree(
    first: &[(String, Vec<u8>)],
    first_hashes: &[String],
    second: &[(String, Vec<u8>)],
) -> bool {
    first.len() == second.len()
        && first
            .iter()
            .zip(second.iter())
            .zip(first_hashes.iter())
            .all(|(((rel_a, a), (rel_b, b)), hash_a)| {
                rel_a == rel_b && a.len() == b.len() && hash_a == &sha256_hex(b)
            })
}
