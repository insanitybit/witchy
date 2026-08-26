//! The interpreter's capability-value operations: Dir/File navigation, file
//! read/write, and `Net` allowlist narrowing. Pure helpers, no evaluator state.

use super::{err, DirValue, FileValue, RuntimeError, Value};

pub(super) fn dir_child_value(dir: &DirValue, name: &str) -> Result<DirValue, RuntimeError> {
    match dir {
        DirValue::Fs(base) => base
            .open_dir(name)
            .map(DirValue::Fs)
            .map_err(|error| RuntimeError { message: error.0 }),
        #[cfg(feature = "test-fixtures")]
        DirValue::Fixture(_) => {
            err("internal error: fixture Dir bypassed fixture dispatch")
        }
    }
}

pub(super) fn dir_file_value(dir: &DirValue, rel: &str, write: bool) -> Result<FileValue, RuntimeError> {
    match dir {
        DirValue::Fs(base) => base
            .file(rel, !write)
            .map(FileValue::Fs)
            .map_err(|error| RuntimeError { message: error.0 }),
        #[cfg(feature = "test-fixtures")]
        DirValue::Fixture(_) => {
            err("internal error: fixture Dir bypassed fixture dispatch")
        }
    }
}

pub(super) fn dir_file_value_for_policy(
    dir: &DirValue,
    policy: &str,
    rel: &str,
    write: bool,
) -> Result<FileValue, RuntimeError> {
    let base = match dir {
        DirValue::Fs(base) => base,
        #[cfg(feature = "test-fixtures")]
        DirValue::Fixture(_) => {
            return err("internal error: fixture Dir bypassed fixture dispatch");
        }
    };
    if policy.is_empty() {
        return dir_file_value(dir, rel, write);
    }
    match base
        .entry_kind_exact(rel)
        .map_err(|error| RuntimeError { message: error.0 })?
    {
        Some(witchy_runtime::confine::ConfinedEntryKind::File) => {}
        None if write => {}
        None => return dir_file_value(dir, rel, write),
        Some(witchy_runtime::confine::ConfinedEntryKind::Directory) => {
            return err(format!("`{rel}` is a directory, not a file"));
        }
        Some(witchy_runtime::confine::ConfinedEntryKind::Other) => {
            return err(format!("`{rel}` is a filesystem alias or unsupported entry"));
        }
    }
    dir_file_value(dir, rel, true)
}

pub(super) fn visible_entry_kind(
    dir: &DirValue,
    policy: &str,
    rel: &str,
) -> Result<Option<bool>, RuntimeError> {
    let base = match dir {
        DirValue::Fs(base) => base,
        #[cfg(feature = "test-fixtures")]
        DirValue::Fixture(_) => {
            return err("internal error: fixture Dir bypassed fixture dispatch");
        }
    };
    if policy.is_empty() {
        if !base.exists(rel).unwrap_or(false) {
            return Ok(None);
        }
        return Ok(Some(base.is_dir(rel).unwrap_or(false)));
    }
    let kind = base
        .entry_kind_exact(rel)
        .map_err(|error| RuntimeError { message: error.0 })?;
    Ok(match kind {
        Some(witchy_runtime::confine::ConfinedEntryKind::File)
            if witchy_caps::capabilities::dir_admits(policy, rel, false) => Some(false),
        Some(witchy_runtime::confine::ConfinedEntryKind::Directory)
            if witchy_caps::capabilities::dir_admits(policy, rel, true) => Some(true),
        _ => None,
    })
}

pub(super) fn exact_entry_kind(
    dir: &DirValue,
    rel: &str,
) -> Result<Option<witchy_runtime::confine::ConfinedEntryKind>, RuntimeError> {
    match dir {
        DirValue::Fs(base) => base
            .entry_kind_exact(rel)
            .map_err(|error| RuntimeError { message: error.0 }),
        #[cfg(feature = "test-fixtures")]
        DirValue::Fixture(_) => err("internal error: fixture Dir bypassed fixture dispatch"),
    }
}

pub(super) fn read_file_value(file: &FileValue) -> Result<String, RuntimeError> {
    match file {
        FileValue::Fs(file) => {
            match file.read_to_string() {
                Ok(contents) => Ok(contents),
                Err(e) => err(format!("read failed for `{}`: {e}", file.display_path().display())),
            }
        }
        #[cfg(feature = "test-fixtures")]
        FileValue::Fixture(_) => {
            err("internal error: fixture File bypassed fixture dispatch")
        }
    }
}

// RFC-0095: read a confined file's RAW bytes (no UTF-8 validation).
pub(super) fn read_file_value_bytes(file: &FileValue) -> Result<Vec<u8>, RuntimeError> {
    match file {
        FileValue::Fs(file) => match file.read_bytes() {
            Ok(bytes) => Ok(bytes),
            Err(e) => err(format!("read failed for `{}`: {e}", file.display_path().display())),
        },
        #[cfg(feature = "test-fixtures")]
        FileValue::Fixture(_) => err("internal error: fixture File bypassed fixture dispatch"),
    }
}

pub(super) fn write_file_value(file: &FileValue, contents: &str) -> Result<(), RuntimeError> {
    match file {
        FileValue::Fs(file) => {
            match file.write_all(contents.as_bytes()) {
                Ok(()) => Ok(()),
                Err(e) => err(format!("write failed for `{}`: {e}", file.display_path().display())),
            }
        }
        #[cfg(feature = "test-fixtures")]
        FileValue::Fixture(_) => {
            err("internal error: fixture File bypassed fixture dispatch")
        }
    }
}

// RFC-0095: write RAW bytes to a confined file (no UTF-8 requirement).
pub(super) fn write_file_value_bytes(file: &FileValue, contents: &[u8]) -> Result<(), RuntimeError> {
    match file {
        FileValue::Fs(file) => match file.write_all(contents) {
            Ok(()) => Ok(()),
            Err(e) => err(format!("write failed for `{}`: {e}", file.display_path().display())),
        },
        #[cfg(feature = "test-fixtures")]
        FileValue::Fixture(_) => err("internal error: fixture File bypassed fixture dispatch"),
    }
}

/// Narrow a `Net`'s allowlist to a `NetPolicy`'s pattern set (`\n`-joined for a union from
/// `confine.union`). Each pattern must already be admitted by the current allowlist
/// (monotone — refinement only shrinks). Returns the narrowed `Net`.
pub(super) fn net_narrow_to(allow: &[String], patterns: &str) -> Result<Value, RuntimeError> {
    match witchy_caps::capabilities::net_only(allow, patterns) {
        Ok(narrowed) => Ok(Value::Net(narrowed)),
        Err(p) => Err(RuntimeError {
            message: format!("`{p}` is not in this Net capability"),
        }),
    }
}
