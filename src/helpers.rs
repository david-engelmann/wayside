//! Shared helpers, the digest forms and the session and environment validators.

use crate::errors::{Error, Result};
use crate::reader::StrictValue;
use crate::SESSION_MAX;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// The lowercase hex rendering of a byte string (WS-A20):
/// the one statement of the idiom the digests and the
/// temporary names share.
pub(crate) fn hex_lowercase(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn sha256_hex(data: &[u8]) -> String {
    hex_lowercase(&Sha256::digest(data))
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
            c if (c as u32) < 0x20 || (0x7f..=0x9f).contains(&(c as u32)) => {
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

pub(crate) fn validate_sequence(sequence: i64) -> Result<i64> {
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
pub(crate) fn validate_env_pairs(
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

pub(crate) fn validate_env(env: &BTreeMap<String, String>) -> Result<()> {
    let pairs: Vec<(String, StrictValue)> = env
        .iter()
        .map(|(key, value)| (key.clone(), StrictValue::Str(value.clone())))
        .collect();
    validate_env_pairs(&pairs, &|why| Error::wayside(format!("invalid env, {why}")))
}
