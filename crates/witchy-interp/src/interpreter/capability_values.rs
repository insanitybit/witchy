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
