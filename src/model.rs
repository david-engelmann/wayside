//! The manifest model, the recorded shape of a checkpoint.

use std::collections::BTreeMap;

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
/// for every wayside-checkpoint/1 document, and the directories
/// list stands empty for every manifest that records no empty
/// directory.
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
    pub directories: Vec<String>,
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
