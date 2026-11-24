//! The error type and the result form every operation returns.

use std::fmt;
use std::io;
use std::path::Path;
use std::path::PathBuf;

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

    pub(crate) fn wayside(message: String) -> Error {
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
pub(crate) type VerifiedPayloads = (Vec<(String, Vec<u8>)>, Vec<u8>);

/// One captured database bundle: the declared path together
/// with its member readings as relative path and bytes pairs
/// in bundle order, and the first reading's member hashes,
/// computed once in the agreement proof and shared with the
/// manifest construction.
pub(crate) type CapturedBundle = (String, Vec<(String, Vec<u8>)>, Vec<String>);

/// The directory listing the state tree walk reads through.
pub(crate) type Lister = dyn Fn(&Path) -> io::Result<Vec<PathBuf>> + Send + Sync;

/// One walked state tree: the file readings as relative path
/// and bytes pairs in sorted order, together with the
/// directories standing in the tree as relative paths in
/// sorted order.
pub(crate) type WalkedTree = (Vec<(String, Vec<u8>)>, Vec<String>);
