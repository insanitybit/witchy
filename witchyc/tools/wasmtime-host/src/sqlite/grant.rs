use std::collections::BTreeSet;
use std::ffi::CString;
#[cfg(target_os = "macos")]
use std::ffi::{c_char, c_int, c_void};
use std::fs;
use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
use std::path::Path;
#[cfg(target_os = "macos")]
use std::ptr::null_mut;

use crate::reject;

#[derive(Default)]
pub(crate) struct Rights {
    pub(super) read: BTreeSet<String>,
    pub(super) insert: BTreeSet<String>,
    pub(super) delete: BTreeSet<String>,
    pub(super) create: BTreeSet<String>,
}

pub(super) fn name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64
        && s.as_bytes()[0].is_ascii_lowercase()
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        && !s.starts_with("sqlite_")
}

impl Rights {
    pub(crate) fn add(&mut self, verb: &str, names: &str) -> wasmtime::Result<()> {
        let set = match verb {
            "read" => &mut self.read,
            "insert" => &mut self.insert,
            "delete" => &mut self.delete,
            "create" => &mut self.create,
            _ => return Err(reject("unknown SQLite table right")),
        };
        for table in names.split(',') {
            if !name(table) || set.len() == 16 || !set.insert(table.to_owned()) {
                return Err(reject("invalid SQLite table grant"));
            }
        }
        Ok(())
    }

    pub(crate) fn empty(&self) -> bool { self.data_empty() && self.create.is_empty() }

    pub(crate) fn provision(&self) -> bool { !self.create.is_empty() }

    pub(super) fn read_only(&self) -> bool {
        self.insert.is_empty() && self.delete.is_empty() && self.create.is_empty()
    }

    fn data_empty(&self) -> bool {
        self.read.is_empty() && self.insert.is_empty() && self.delete.is_empty()
    }
}

pub(crate) struct Grant {
    pub(super) path: CString,
    pub(super) rights: Rights,
    pub(super) secret_objects: Vec<(u64, u64)>,
    pub(super) reply_bytes: usize,
    pub(super) authorized: bool,
    pub(super) used: bool,
}

impl Grant {
    pub(crate) fn new(filename: &str, rights: Rights, secret_objects: &[(u64, u64)]) -> wasmtime::Result<Self> {
        if rights.provision() && (!rights.data_empty() || !secret_objects.is_empty()) {
            return Err(reject("SQLite table creation requires a separate grant without data rights or secrets"));
        }
        let path = Path::new(filename);
        let refused = || reject("SQLite needs an existing database in a private, owner-controlled directory");
        if !path.is_absolute() || fs::canonicalize(path).ok().as_deref() != Some(path) || rights.empty() {
            return Err(refused());
        }
        let root = path.parent().ok_or_else(refused)?;
        let owner = crate::sys::effective_user();
        let info = fs::symlink_metadata(root).map_err(|_| refused())?;
        if !info.is_dir() || info.uid() != owner || info.mode() & 0o077 != 0 || extended_acl(root) {
            return Err(refused());
        }
        for ancestor in root.ancestors().skip(1) {
            let info = fs::symlink_metadata(ancestor).map_err(|_| refused())?;
            if !info.is_dir() || info.uid() != 0 && info.uid() != owner
                || info.mode() & 0o022 != 0 && info.mode() & 0o1000 == 0
                || extended_acl(ancestor)
            {
                return Err(refused());
            }
        }
        check_files(path, secret_objects).ok_or_else(refused)?;
        Ok(Self { path: CString::new(filename).map_err(|_| refused())?, rights, secret_objects: secret_objects.to_vec(),
            reply_bytes: super::DEFAULT_REPLY_BYTES, authorized: false, used: false })
    }

    pub(crate) fn with_reply_bytes(mut self, limit: Option<usize>) -> Self {
        if let Some(limit) = limit { self.reply_bytes = limit; }
        self
    }
}

#[cfg(not(target_os = "macos"))]
fn extended_acl(_: &Path) -> bool { false }

#[cfg(target_os = "macos")]
fn extended_acl(path: &Path) -> bool {
    unsafe extern "C" {
        fn acl_get_file(path: *const c_char, kind: c_int) -> *mut c_void;
        fn acl_get_entry(acl: *mut c_void, index: c_int, entry: *mut *mut c_void) -> c_int;
        fn acl_get_tag_type(entry: *mut c_void, tag: *mut u32) -> c_int;
        fn acl_free(acl: *mut c_void) -> c_int;
    }
    let Ok(path) = CString::new(path.as_os_str().as_bytes()) else { return true };
    // In Darwin, ACL_TYPE_EXTENDED is 0x100 and a missing ACL is ENOENT.
    unsafe {
        let acl = acl_get_file(path.as_ptr(), 0x100);
        if acl.is_null() { return std::io::Error::last_os_error().raw_os_error() != Some(2); }
        let mut entry = null_mut();
        let mut index = 0; // ACL_FIRST_ENTRY, then ACL_NEXT_ENTRY.
        let unsafe_acl = loop {
            let status = acl_get_entry(acl, index, &mut entry);
            if status != 0 {
                break std::io::Error::last_os_error().raw_os_error() != Some(22); // end of entries
            }
            let mut tag = 0;
            if acl_get_tag_type(entry, &mut tag) != 0 || tag != 2 { break true; } // ACL_EXTENDED_DENY
            index = -1;
        };
        acl_free(acl);
        unsafe_acl
    }
}

pub(super) fn check_files(path: &Path, secret_objects: &[(u64, u64)]) -> Option<()> {
    let safe = |m: &fs::Metadata| m.is_file() && m.uid() == crate::sys::effective_user()
        && m.nlink() == 1 && m.mode() & 0o022 == 0 && !secret_objects.contains(&(m.dev(), m.ino()));
    if !safe(&fs::symlink_metadata(path).ok()?) { return None; }
    for suffix in ["-journal", "-wal", "-shm"] {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(suffix);
        match fs::symlink_metadata(sidecar) {
            Ok(info) if safe(&info) => (),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => (),
            _ => return None,
        }
    }
    Some(())
}
