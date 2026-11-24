//! The renderer, a manifest written out in its recorded byte form.

use crate::model::{DatabaseEntry, Manifest};
use crate::{FORMAT_NAME, FORMAT_V2};
use std::collections::BTreeMap;

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

/// Render one recorded entries array, the files of a manifest
/// or the members of one database, whose entries all carry a
/// path, a hash, and a size in that key order (WS-A17).
fn render_entries_array(entries: &[(&str, &str, i128)], level: usize, out: &mut String) {
    if entries.is_empty() {
        out.push_str("[]");
        return;
    }
    out.push_str("[\n");
    let count = entries.len();
    for (index, entry) in entries.iter().enumerate() {
        write_indent(out, level + 1);
        out.push_str("{\n");
        write_indent(out, level + 2);
        escape_json_string("path", out);
        out.push_str(": ");
        escape_json_string(entry.0, out);
        out.push_str(",\n");
        write_indent(out, level + 2);
        escape_json_string("sha256", out);
        out.push_str(": ");
        escape_json_string(entry.1, out);
        out.push_str(",\n");
        write_indent(out, level + 2);
        escape_json_string("size", out);
        out.push_str(": ");
        out.push_str(&entry.2.to_string());
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
        render_entries_array(
            &entry
                .members
                .iter()
                .map(|member| (member.path.as_str(), member.sha256.as_str(), member.size))
                .collect::<Vec<_>>(),
            level + 2,
            out,
        );
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

fn render_directories_array(directories: &[String], level: usize, out: &mut String) {
    if directories.is_empty() {
        out.push_str("[]");
        return;
    }
    out.push_str("[\n");
    let count = directories.len();
    for (index, directory) in directories.iter().enumerate() {
        write_indent(out, level + 1);
        escape_json_string(directory, out);
        if index + 1 < count {
            out.push(',');
        }
        out.push('\n');
    }
    write_indent(out, level);
    out.push(']');
}

/// Render one manifest field (WS-A38): a fresh string
/// holding the field's indent, its escaped key, and the
/// colon separator, with the value rendered by the caller,
/// the shape every field of render_manifest shares.
fn render_field(key: &str, render: impl FnOnce(&mut String)) -> String {
    let mut field = String::new();
    write_indent(&mut field, 1);
    escape_json_string(key, &mut field);
    field.push_str(": ");
    render(&mut field);
    field
}

/// The format a manifest's content names (WS-A72): /1 when
/// both conditional lists stand empty, /2 when either
/// stands. The derivation stands stated once here, where
/// new_manifest and render_manifest once each stated it
/// under WS-A14's discipline, so a change to the rule is
/// taken up at this one seat or not at all.
pub(crate) fn manifest_format(databases: &[DatabaseEntry], directories: &[String]) -> &'static str {
    if databases.is_empty() && directories.is_empty() {
        FORMAT_NAME
    } else {
        FORMAT_V2
    }
}

/// Render a manifest exactly as the Python implementation's
/// sorted, two space indented json rendering renders it, with
/// no trailing newline. The stored manifest file carries this
/// text plus one newline, and the show command prints this text
/// plus one newline, so the two are byte identical. A
/// wayside-checkpoint/2 manifest renders by its own format
/// value, with the databases field standing in the sorted key
/// order the rendering keeps, after created_at, and the
/// directories field after it when a directory is recorded. A
/// /2 value with both lists empty renders as the /1 rendering
/// of the same value, because WS-21 writes that content as /1
/// and the reader refuses a /2 rendering whose conditional
/// fields stand empty.
pub fn render_manifest(manifest: &Manifest) -> String {
    // The rendering is derived from the content throughout,
    // as new_manifest derives the format (WS-A14): the format
    // line names /2 exactly when a conditional field stands,
    // and each conditional field below stands exactly when
    // its list is non empty, so a hand built value renders
    // the content it carries under the format that content
    // names, and no field it carries is dropped.
    let effective_format = manifest_format(&manifest.databases, &manifest.directories);
    let mut out = String::new();
    out.push_str("{\n");
    let mut fields: Vec<String> = Vec::new();
    fields.push(render_field("created_at", |field| {
        escape_json_string(&manifest.created_at, field);
    }));

    if !manifest.databases.is_empty() {
        fields.push(render_field("databases", |field| {
            render_databases_array(&manifest.databases, 1, field);
        }));
    }

    if !manifest.directories.is_empty() {
        fields.push(render_field("directories", |field| {
            render_directories_array(&manifest.directories, 1, field);
        }));
    }

    fields.push(render_field("env", |field| {
        render_env_object(&manifest.env, 1, field);
    }));

    fields.push(render_field("files", |field| {
        render_entries_array(
            &manifest
                .files
                .iter()
                .map(|entry| (entry.path.as_str(), entry.sha256.as_str(), entry.size))
                .collect::<Vec<_>>(),
            1,
            field,
        );
    }));

    if let Some(fork) = &manifest.forked_from {
        fields.push(render_field("forked_from", |field| {
            field.push_str("{\n");
            write_indent(field, 2);
            escape_json_string("sequence", field);
            field.push_str(": ");
            field.push_str(&fork.sequence.to_string());
            field.push_str(",\n");
            write_indent(field, 2);
            escape_json_string("session", field);
            field.push_str(": ");
            escape_json_string(&fork.session, field);
            field.push('\n');
            write_indent(field, 1);
            field.push('}');
        }));
    }

    fields.push(render_field("format", |field| {
        escape_json_string(effective_format, field);
    }));

    fields.push(render_field("memory", |field| {
        field.push_str("{\n");
        write_indent(field, 2);
        escape_json_string("sha256", field);
        field.push_str(": ");
        escape_json_string(&manifest.memory.sha256, field);
        field.push_str(",\n");
        write_indent(field, 2);
        escape_json_string("size", field);
        field.push_str(": ");
        field.push_str(&manifest.memory.size.to_string());
        field.push('\n');
        write_indent(field, 1);
        field.push('}');
    }));

    fields.push(render_field("sequence", |field| {
        field.push_str(&manifest.sequence.to_string());
    }));

    fields.push(render_field("session", |field| {
        escape_json_string(&manifest.session, field);
    }));

    out.push_str(&fields.join(",\n"));
    out.push_str("\n}");
    out
}
