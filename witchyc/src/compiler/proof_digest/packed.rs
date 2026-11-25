//! Identity of the fixed Lean import runtime. Frame the entire tree, then hash
//! every regular file as ordered, fixed-size SHA-256 leaves. No digest is read
//! from a manifest. The ordinary SDK still uses its original stream identity.
use super::platform::Hash;
use std::{fs::{self, File}, io, os::unix::fs::FileExt, path::Path, thread};

pub(super) const MARKER: &str = "fixed-header.chunked-v1";
pub(super) const DOMAIN: &str = "witchy.scalar.packed-imports.v1";
const CHUNK: usize = 4 * 1024 * 1024;

fn leaves(file: &File, size: u64, workers: usize) -> io::Result<Vec<String>> {
    let count = size.div_ceil(CHUNK as u64) as usize;
    let range = |first: usize, last: usize| -> io::Result<Vec<String>> {
        let mut buf = vec![0; CHUNK.min(size as usize)];
        let mut result = Vec::with_capacity(last - first);
        for index in first..last {
            let offset = index as u64 * CHUNK as u64;
            let part = &mut buf[..(size - offset).min(CHUNK as u64) as usize];
            file.read_exact_at(part, offset)?;
            result.push(super::bytes(part)?);
        }
        Ok(result)
    };
    let slots = count.min(workers);
    if slots < 2 { return range(0, count); }
    thread::scope(|scope| {
        let mut tasks = Vec::new();
        for i in 0..slots {
            tasks.push(thread::Builder::new().spawn_scoped(scope, move || {
                range(i * count / slots, (i + 1) * count / slots)
            })?);
        }
        let mut result = Vec::with_capacity(count);
        for task in tasks {
            result.extend(task.join().map_err(|_| io::Error::other("packed identity worker failed"))??);
        }
        Ok(result)
    })
}

pub(super) fn identity(root: &Path) -> io::Result<String> {
    // This version of the fixed runtime has two directories and flat library
    // files. Refuse unrecognized entries instead of omitting them from the pin.
    let mut hash = Hash::new()?;
    hash.update(DOMAIN.as_bytes())?;
    let mut total = 0u64;
    for relative in ["", "lib", "lib/lean"] {
        let mut entries = fs::read_dir(root.join(relative))?.take(129).collect::<io::Result<Vec<_>>>()?;
        if entries.len() > 128 { return Err(io::Error::other("packed runtime entry limit")); }
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            let name = entry.file_name().into_string().map_err(|_| io::Error::other("invalid runtime path"))?;
            let rel = path.strip_prefix(root).unwrap().to_str().ok_or_else(|| io::Error::other("invalid runtime path"))?;
            hash.update(&(rel.len() as u64).to_le_bytes())?;
            hash.update(rel.as_bytes())?;
            let kind = entry.file_type()?;
            if matches!(rel, "lib" | "lib/lean") && kind.is_dir() {
                hash.update(b"D")?;
                continue;
            }
            let allowed = matches!(rel, MARKER | "fixed-header.snap" | "fixed-header.modules")
                || (relative == "lib/lean" && name.ends_with(".dylib"));
            if !allowed || !kind.is_file() { return Err(io::Error::other("unexpected packed runtime entry")); }
            let file = File::open(path)?;
            let size = file.metadata()?.len();
            total = total.checked_add(size).ok_or_else(|| io::Error::other("packed runtime size overflow"))?;
            if size > 1 << 30 || total > 8 << 30 { return Err(io::Error::other("packed runtime size limit")); }
            hash.update(b"F")?;
            hash.update(&size.to_le_bytes())?;
            for leaf in leaves(&file, size, 4)? { hash.update(leaf.as_bytes())?; }
            if file.metadata()?.len() != size { return Err(io::Error::other("packed runtime length changed")); }
        }
    }
    hash.finish()
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    #[test]
    fn parallel_leaves_bind_order_and_the_last_partial_chunk() {
        let path = std::env::temp_dir().join(format!("witchy-packed-leaves-{}", std::process::id()));
        let mut data = vec![0; 2 * CHUNK + 13];
        data[CHUNK] = 1;
        let f = File::options().create_new(true).read(true).write(true).open(&path).unwrap();
        fs::remove_file(path).unwrap();
        f.write_all_at(&data, 0).unwrap();
        let expected: Vec<_> = data.chunks(CHUNK).map(|p| super::super::bytes(p).unwrap()).collect();
        assert_eq!(leaves(&f, data.len() as u64, 4).unwrap(), expected);
        f.write_all_at(b"x", data.len() as u64 - 1).unwrap();
        assert_ne!(leaves(&f, data.len() as u64, 4).unwrap(), expected);
    }
}
