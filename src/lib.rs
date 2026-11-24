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

pub mod capture;
pub mod errors;
pub mod helpers;
pub mod manifest;
pub mod model;
pub mod operations;
pub mod reader;
pub mod render;
pub mod store;

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

pub use capture::StateTreeReader;
pub use errors::{Error, Result};
pub use helpers::{py_repr, validate_session_id};
pub use model::{
    CheckpointInfo, Comparison, DatabaseComparison, DatabaseEntry, DatabaseMember,
    DatabaseStanding, FileEntry, ForkRef, Manifest, MemoryEntry,
};
pub use operations::{
    checkpoint, checkpoint_with_databases, checkpoint_with_databases_and_reader,
    checkpoint_with_reader, compare_checkpoints, fork, list_checkpoints, restore, rewind, show,
    verify_checkpoint,
};
pub use render::render_manifest;
