//! The interpreter's in-memory capability-VALUE operations: the mock `Dir`
//! filesystem (RFC-0077 `testing.mock_dir`), Dir/File navigation, file read/
//! write, and `Net` allowlist narrowing. Pure value helpers, no evaluator state.

use std::collections::BTreeMap;
use std::path::Path;

use super::{err, DirValue, FileValue, RuntimeError, Value};

pub(super) fn mock_normalize(rel: &str) -> Result<String, RuntimeError> {
    let path = Path::new(rel);
    if path.is_absolute() {
        return err(format!("mock Dir path `{rel}` must be relative"));
    }
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(part) => {
                let Some(part) = part.to_str() else {
                    return err(format!("mock Dir path `{rel}` is not valid UTF-8"));
                };
                parts.push(part.to_string());
            }
            std::path::Component::ParentDir => {
                return err(format!("mock Dir path `{rel}` may not contain `..`"));
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return err(format!("mock Dir path `{rel}` must be relative"));
            }
        }
    }
    Ok(parts.join("/"))
}

pub(super) fn mock_join(root: &str, rel: &str) -> Result<String, RuntimeError> {
    let rel = mock_normalize(rel)?;
    Ok(match (root.is_empty(), rel.is_empty()) {
        (_, true) => root.to_string(),
        (true, false) => rel,
        (false, false) => format!("{root}/{rel}"),
    })
}

pub(super) fn mock_is_dir(files: &BTreeMap<String, String>, path: &str) -> bool {
    if path.is_empty() {
        return true;
    }
    let prefix = format!("{path}/");
    files.keys().any(|entry| entry.starts_with(&prefix))
}

pub(super) fn mock_exists(files: &BTreeMap<String, String>, path: &str) -> bool {
    files.contains_key(path) || mock_is_dir(files, path)
}

pub(super) fn mock_list(files: &BTreeMap<String, String>, root: &str) -> Result<Vec<String>, RuntimeError> {
    if !mock_is_dir(files, root) {
        return err(format!("list failed for mock Dir `{root}`: not a directory"));
    }
    let mut names = std::collections::BTreeSet::new();
    let prefix = if root.is_empty() {
        String::new()
    } else {
        format!("{root}/")
    };
    for path in files.keys() {
        let Some(rest) = path.strip_prefix(&prefix) else {
            continue;
        };
        if rest.is_empty() {
            continue;
        }
        let name = rest.split('/').next().unwrap_or(rest);
        names.insert(name.to_string());
    }
    Ok(names.into_iter().collect())
}

pub(super) fn dir_child_value(dir: &DirValue, name: &str) -> Result<DirValue, RuntimeError> {
    match dir {
        DirValue::Fs(base) => base
            .open_dir(name)
            .map(DirValue::Fs)
            .map_err(|error| RuntimeError { message: error.0 }),
        DirValue::Mock { root, files } => {
            Ok(DirValue::Mock { root: mock_join(root, name)?, files: files.clone() })
        }
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
        DirValue::Mock { root, files } => {
            Ok(FileValue::Mock { path: mock_join(root, rel)?, files: files.clone() })
        }
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
        FileValue::Mock { path, files } => files
            .get(path)
            .cloned()
            .ok_or_else(|| RuntimeError {
                message: format!("read failed for mock Dir `{path}`: no such file"),
            }),
        #[cfg(feature = "test-fixtures")]
        FileValue::Fixture(_) => {
            err("internal error: fixture File bypassed fixture dispatch")
        }
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
        FileValue::Mock { path, .. } => err(format!(
            "write failed for mock Dir `{path}`: mock directories are read-only"
        )),
        #[cfg(feature = "test-fixtures")]
        FileValue::Fixture(_) => {
            err("internal error: fixture File bypassed fixture dispatch")
        }
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
