//! The thin Python binding over the wayside crate.
//!
//! Every rule lives in the library this crate wraps. The functions
//! here translate Python values into the library arguments, call
//! the one library function each one names, and translate the
//! result or the refusal back, and nothing else. Each call runs
//! detached from the interpreter, so the filesystem and lock work
//! inside a call never stalls the other threads of the calling
//! process while it waits. A library refusal becomes a Python
//! exception carrying the same message text the command line
//! prints, under a subclass of WaysideError picked by the exit
//! class of the refusal, so a Python caller can tell a plain
//! refusal from an invalid name from a missing checkpoint from a
//! corrupt store from an unusable target the way a shell caller
//! tells them apart by exit code.

use std::collections::BTreeMap;
use std::path::PathBuf;

use pyo3::create_exception;
use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict};

use wayside_core::{CheckpointInfo, Comparison, DatabaseStanding, Error, Manifest};

create_exception!(wayside, WaysideError, pyo3::exceptions::PyException);
create_exception!(wayside, RefusalError, WaysideError);
create_exception!(wayside, InvalidNameError, WaysideError);
create_exception!(wayside, NotFoundError, WaysideError);
create_exception!(wayside, CorruptError, WaysideError);
create_exception!(wayside, TargetError, WaysideError);

/// Translate a library refusal into the Python exception of its
/// exit class, carrying the message the command line prints.
fn to_py_error(error: Error) -> PyErr {
    let message = error.message().to_string();
    match error {
        Error::Wayside(_) => RefusalError::new_err(message),
        Error::InvalidName(_) => InvalidNameError::new_err(message),
        Error::NotFound(_) => NotFoundError::new_err(message),
        Error::Corrupt(_) => CorruptError::new_err(message),
        Error::Target(_) => TargetError::new_err(message),
    }
}

/// Resolve the sequence argument of one call.
///
/// A boolean is refused outright with a TypeError naming the
/// argument, because Python spells True and False as the integers
/// 1 and 0 and a sequence is never meant as a truth value. Any
/// other value converts the way Python converts an integer, and
/// the library itself judges whether the number names a sequence
/// it can use.
fn sequence_argument(value: &Bound<'_, PyAny>, name: &str) -> PyResult<i64> {
    if value.is_instance_of::<PyBool>() {
        Err(PyTypeError::new_err(format!(
            "{name} must be an integer, not a boolean"
        )))
    } else {
        value.extract::<i64>()
    }
}

/// Translate one validated manifest into a plain dictionary
/// carrying every field the manifest carries, in the shape of
/// the wayside-checkpoint formats themselves. The databases
/// list stands empty for a wayside-checkpoint/1 manifest, as
/// the forked_from entry stands empty for an unforked one.
fn manifest_dict(py: Python<'_>, manifest: &Manifest) -> PyResult<Py<PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("format", &manifest.format)?;
    dict.set_item("session", &manifest.session)?;
    dict.set_item("sequence", manifest.sequence)?;
    dict.set_item("created_at", &manifest.created_at)?;
    let env = PyDict::new(py);
    for (key, value) in &manifest.env {
        env.set_item(key, value)?;
    }
    dict.set_item("env", env)?;
    let memory = PyDict::new(py);
    memory.set_item("sha256", &manifest.memory.sha256)?;
    memory.set_item("size", manifest.memory.size)?;
    dict.set_item("memory", memory)?;
    let mut files = Vec::new();
    for entry in &manifest.files {
        let file = PyDict::new(py);
        file.set_item("path", &entry.path)?;
        file.set_item("sha256", &entry.sha256)?;
        file.set_item("size", entry.size)?;
        files.push(file.unbind());
    }
    dict.set_item("files", files)?;
    let mut databases = Vec::new();
    for entry in &manifest.databases {
        let database = PyDict::new(py);
        database.set_item("path", &entry.path)?;
        database.set_item("bundle_sha256", &entry.bundle_sha256)?;
        let mut members = Vec::new();
        for member in &entry.members {
            let member_dict = PyDict::new(py);
            member_dict.set_item("path", &member.path)?;
            member_dict.set_item("sha256", &member.sha256)?;
            member_dict.set_item("size", member.size)?;
            members.push(member_dict.unbind());
        }
        database.set_item("members", members)?;
        databases.push(database.unbind());
    }
    dict.set_item("databases", databases)?;
    let forked_from = match &manifest.forked_from {
        Some(lineage) => {
            let fork = PyDict::new(py);
            fork.set_item("session", &lineage.session)?;
            fork.set_item("sequence", lineage.sequence)?;
            Some(fork.unbind())
        }
        None => None,
    };
    dict.set_item("forked_from", forked_from)?;
    Ok(dict.unbind())
}

/// Translate one list entry into a plain dictionary carrying the
/// sequence and the creation time list reports for it.
fn info_dict(py: Python<'_>, info: &CheckpointInfo) -> PyResult<Py<PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("sequence", info.sequence)?;
    dict.set_item("created_at", &info.created_at)?;
    Ok(dict.unbind())
}

/// Record a new checkpoint of the session from the state
/// directory and return its manifest.
///
/// The env argument is a mapping of declared env pairs and
/// defaults to no pairs. The memory argument is the memory
/// document as bytes and defaults to an empty document. The
/// databases argument is a list of database paths declared
/// inside the state directory and defaults to no declarations.
#[pyfunction]
#[pyo3(signature = (store, session, state, env = None, memory = None, databases = None))]
fn checkpoint(
    py: Python<'_>,
    store: PathBuf,
    session: &str,
    state: PathBuf,
    env: Option<BTreeMap<String, String>>,
    memory: Option<Vec<u8>>,
    databases: Option<Vec<String>>,
) -> PyResult<Py<PyDict>> {
    let memory = memory.unwrap_or_default();
    let databases = databases.unwrap_or_default();
    let manifest = py
        .detach(|| {
            wayside_core::checkpoint_with_databases(
                &store,
                session,
                &state,
                env.as_ref(),
                &memory,
                &databases,
            )
        })
        .map_err(to_py_error)?;
    manifest_dict(py, &manifest)
}

/// Materialize one checkpoint into the target directory and
/// return its manifest.
#[pyfunction]
fn restore(
    py: Python<'_>,
    store: PathBuf,
    session: &str,
    sequence: &Bound<'_, PyAny>,
    target: PathBuf,
) -> PyResult<Py<PyDict>> {
    let sequence = sequence_argument(sequence, "sequence")?;
    let manifest = py
        .detach(|| wayside_core::restore(&store, session, sequence, &target))
        .map_err(to_py_error)?;
    manifest_dict(py, &manifest)
}

/// Restore a checkpoint that stands earlier than the newest one
/// into the target directory and return its manifest.
#[pyfunction]
fn rewind(
    py: Python<'_>,
    store: PathBuf,
    session: &str,
    sequence: &Bound<'_, PyAny>,
    target: PathBuf,
) -> PyResult<Py<PyDict>> {
    let sequence = sequence_argument(sequence, "sequence")?;
    let manifest = py
        .detach(|| wayside_core::rewind(&store, session, sequence, &target))
        .map_err(to_py_error)?;
    manifest_dict(py, &manifest)
}

/// Start a new session whose first checkpoint copies a named
/// one and return the manifest of the copy.
#[pyfunction]
fn fork(
    py: Python<'_>,
    store: PathBuf,
    session: &str,
    sequence: &Bound<'_, PyAny>,
    new_session: &str,
) -> PyResult<Py<PyDict>> {
    let sequence = sequence_argument(sequence, "sequence")?;
    let manifest = py
        .detach(|| wayside_core::fork(&store, session, sequence, new_session))
        .map_err(to_py_error)?;
    manifest_dict(py, &manifest)
}

/// List the checkpoints of the session as dictionaries carrying
/// the sequence and the creation time of each one, in order.
#[pyfunction(name = "list")]
fn list_checkpoints(py: Python<'_>, store: PathBuf, session: &str) -> PyResult<Vec<Py<PyDict>>> {
    let infos = py
        .detach(|| wayside_core::list_checkpoints(&store, session))
        .map_err(to_py_error)?;
    infos.iter().map(|info| info_dict(py, info)).collect()
}

/// Return the validated manifest of one checkpoint.
#[pyfunction]
fn show(
    py: Python<'_>,
    store: PathBuf,
    session: &str,
    sequence: &Bound<'_, PyAny>,
) -> PyResult<Py<PyDict>> {
    let sequence = sequence_argument(sequence, "sequence")?;
    let manifest = py
        .detach(|| wayside_core::show(&store, session, sequence))
        .map_err(to_py_error)?;
    manifest_dict(py, &manifest)
}

/// Translate one comparison into a plain dictionary carrying
/// the session, the two sequences in the order they were
/// named, and one entry per database path recorded on either
/// side, each entry carrying the path and its standing as one
/// of the words equal, different, only_first, and only_second.
fn comparison_dict(py: Python<'_>, comparison: &Comparison) -> PyResult<Py<PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("session", &comparison.session)?;
    dict.set_item("first", comparison.first)?;
    dict.set_item("second", comparison.second)?;
    let mut databases = Vec::new();
    for entry in &comparison.databases {
        let database = PyDict::new(py);
        database.set_item("path", &entry.path)?;
        database.set_item(
            "standing",
            match entry.standing {
                DatabaseStanding::Equal => "equal",
                DatabaseStanding::Different => "different",
                DatabaseStanding::OnlyFirst => "only_first",
                DatabaseStanding::OnlySecond => "only_second",
            },
        )?;
        databases.push(database.unbind());
    }
    dict.set_item("databases", databases)?;
    Ok(dict.unbind())
}

/// Verify one checkpoint against its recorded hashes and
/// return its manifest when every payload matches.
#[pyfunction]
fn verify(
    py: Python<'_>,
    store: PathBuf,
    session: &str,
    sequence: &Bound<'_, PyAny>,
) -> PyResult<Py<PyDict>> {
    let sequence = sequence_argument(sequence, "sequence")?;
    let manifest = py
        .detach(|| wayside_core::verify_checkpoint(&store, session, sequence))
        .map_err(to_py_error)?;
    manifest_dict(py, &manifest)
}

/// Compare the recorded databases of two checkpoints of one
/// session and return the comparison as a plain dictionary.
#[pyfunction]
fn compare(
    py: Python<'_>,
    store: PathBuf,
    session: &str,
    first: &Bound<'_, PyAny>,
    second: &Bound<'_, PyAny>,
) -> PyResult<Py<PyDict>> {
    let first = sequence_argument(first, "first")?;
    let second = sequence_argument(second, "second")?;
    let comparison = py
        .detach(|| wayside_core::compare_checkpoints(&store, session, first, second))
        .map_err(to_py_error)?;
    comparison_dict(py, &comparison)
}

#[pymodule]
fn _wayside(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("WaysideError", m.py().get_type::<WaysideError>())?;
    m.add("RefusalError", m.py().get_type::<RefusalError>())?;
    m.add("InvalidNameError", m.py().get_type::<InvalidNameError>())?;
    m.add("NotFoundError", m.py().get_type::<NotFoundError>())?;
    m.add("CorruptError", m.py().get_type::<CorruptError>())?;
    m.add("TargetError", m.py().get_type::<TargetError>())?;
    m.add_function(wrap_pyfunction!(checkpoint, m)?)?;
    m.add_function(wrap_pyfunction!(restore, m)?)?;
    m.add_function(wrap_pyfunction!(rewind, m)?)?;
    m.add_function(wrap_pyfunction!(fork, m)?)?;
    m.add_function(wrap_pyfunction!(list_checkpoints, m)?)?;
    m.add_function(wrap_pyfunction!(show, m)?)?;
    m.add_function(wrap_pyfunction!(verify, m)?)?;
    m.add_function(wrap_pyfunction!(compare, m)?)?;
    Ok(())
}
