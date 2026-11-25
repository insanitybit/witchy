//! Content identities only; this module never grants proof authority.
use std::{fs::{self, File}, io::{self, Read}, path::Path};
mod reader;
#[cfg(unix)]
mod packed;

#[cfg(target_os = "macos")]
mod platform {
    use std::{ffi::c_void, io};
    #[repr(C)]
    pub(super) struct Hash([u32; 26]);
    #[link(name = "System")]
    unsafe extern "C" {
        fn CC_SHA256_Init(context: *mut Hash) -> i32;
        fn CC_SHA256_Update(context: *mut Hash, bytes: *const c_void, count: u32) -> i32;
        fn CC_SHA256_Final(bytes: *mut u8, context: *mut Hash) -> i32;
        fn CC_SHA384_Init(context: *mut Wide) -> i32;
        fn CC_SHA384_Update(context: *mut Wide, bytes: *const c_void, count: u32) -> i32;
        fn CC_SHA384_Final(bytes: *mut u8, context: *mut Wide) -> i32;
    }
    /// CommonCrypto's `CC_SHA512_CTX`, which SHA-384 uses: 26 64-bit words.
    #[repr(C)]
    struct Wide([u64; 26]);
    /// SHA-384 of `bytes` (the `witchy-2` profile's security pins), as 48 bytes.
    pub(super) fn sha384(bytes: &[u8]) -> io::Result<[u8; 48]> {
        let mut context = Wide([0; 26]);
        // CommonCrypto initializes exactly its documented SHA-384 context.
        if unsafe { CC_SHA384_Init(&mut context) } != 1 { return Err(io::Error::other("SHA384 initialization")); }
        for chunk in bytes.chunks(1 << 20) {
            // The live slice contains exactly count readable bytes.
            if unsafe { CC_SHA384_Update(&mut context, chunk.as_ptr().cast(), chunk.len() as u32) } != 1 { return Err(io::Error::other("SHA384 update")); }
        }
        let mut out = [0u8; 48];
        // CommonCrypto writes precisely 48 digest bytes.
        if unsafe { CC_SHA384_Final(out.as_mut_ptr(), &mut context) } != 1 { return Err(io::Error::other("SHA384 finalization")); }
        Ok(out)
    }
    impl Hash {
        pub(super) fn new() -> io::Result<Self> {
            let mut hash = Self([0; 26]);
            // CommonCrypto initializes exactly its documented SHA256 context.
            if unsafe { CC_SHA256_Init(&mut hash) } != 1 { return Err(io::Error::other("SHA256 initialization")); }
            Ok(hash)
        }
        pub(super) fn update(&mut self, bytes: &[u8]) -> io::Result<()> {
            for chunk in bytes.chunks(1 << 20) {
                // The live slice contains exactly count readable bytes.
                if unsafe { CC_SHA256_Update(self, chunk.as_ptr().cast(), chunk.len() as u32) } != 1 {
                    return Err(io::Error::other("SHA256 update"));
                }
            }
            Ok(())
        }
        pub(super) fn finish(mut self) -> io::Result<String> {
            let mut bytes = [0u8; 32];
            // CommonCrypto writes precisely 32 digest bytes.
            if unsafe { CC_SHA256_Final(bytes.as_mut_ptr(), &mut self) } != 1 { return Err(io::Error::other("SHA256 finalization")); }
            Ok(super::hex(&bytes))
        }
    }
}
#[cfg(target_os = "linux")]
#[path = "proof_digest/linux.rs"]
mod platform;

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod platform {
    use std::io;
    pub(super) struct Hash;
    impl Hash {
        pub(super) fn new() -> io::Result<Self> { Err(io::Error::other("fresh proof identity is currently supported on macOS and Linux only")) }
        pub(super) fn update(&mut self, _: &[u8]) -> io::Result<()> { unreachable!() }
        pub(super) fn finish(self) -> io::Result<String> { unreachable!() }
    }
    pub(super) fn sha384(_: &[u8]) -> io::Result<[u8; 48]> { Err(io::Error::other("security pins are currently supported on macOS and Linux only")) }
}
/// Lowercase hex, two digits a byte (a `format!` per byte was most of a digest's cost).
fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[usize::from(byte >> 4)] as char);
        out.push(DIGITS[usize::from(byte & 15)] as char);
    }
    out
}
/// A security pin of `bytes` (a policy's `; digest`): `sha384:` and the SHA-384 in lowercase hex.
/// The algorithm is in the text, so a pin of another algorithm is never compared as this one
/// (the `witchy-2` profile, docs/decisions.md 2026-11-20).
pub(super) fn pin(bytes: &[u8]) -> io::Result<String> { Ok(format!("sha384:{}", hex(&platform::sha384(bytes)?))) }
/// Whether `text` is a pin as `pin` writes it.
pub(super) fn is_pin(text: &str) -> bool {
    text.strip_prefix("sha384:").is_some_and(|hex| hex.len() == 96 && hex.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()))
}
pub(super) fn bytes(bytes: &[u8]) -> io::Result<String> {
    let mut hash = platform::Hash::new()?;
    hash.update(bytes)?;
    hash.finish()
}
pub(super) fn file(path: &Path) -> io::Result<String> {
    let mut hash = platform::Hash::new()?;
    let mut file = File::open(path)?;
    let mut size = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 { return hash.finish(); }
        size += read as u64;
        if size > 1024 * 1024 * 1024 { return Err(io::Error::other("compiler identity size limit")); }
        hash.update(&buffer[..read])?;
    }
}
fn frame(hash: &mut reader::Ordered<'_>, bytes: &[u8]) -> io::Result<()> {
    hash.update(&(bytes.len() as u64).to_le_bytes())?;
    hash.update(bytes)
}
/// Snapshot the entire explicitly trusted installation, not a writable cache
/// or artifact-provided manifest. Only in-root regular-file symlinks are allowed;
/// their link text and target identity are included. Special files fail closed. The caller
/// assumes no malicious concurrent installation edits and snapshots twice.
pub(super) fn installation(root: &Path) -> io::Result<String> {
    installation_identity(root).map(|(_, digest)| digest)
}

pub(super) fn installation_identity(root: &Path) -> io::Result<(&'static str, String)> {
    #[cfg(unix)]
    match fs::symlink_metadata(root.join(packed::MARKER)) {
        Ok(m) if m.is_file() && m.len() == 0 =>
            return packed::identity(root).map(|digest| (packed::DOMAIN, digest)),
        Ok(_) => return Err(io::Error::other("invalid packed runtime marker")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    stream_identity(root).map(|digest| ("witchy.scalar.lean-installation.v1", digest))
}

fn check_cancelled() -> io::Result<()> {
    if super::compiler_session::cancelled() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "Lean installation fingerprint cancelled"));
    }
    Ok(())
}

fn stream_identity(root: &Path) -> io::Result<String> {
    fn walk(root: &Path, path: &Path, hash: &mut reader::Ordered<'_>, count: &mut usize, size: &mut u64, depth: usize) -> io::Result<()> {
        check_cancelled()?;
        if depth > 32 { return Err(io::Error::other("Lean installation directory depth")); }
        let mut entries = fs::read_dir(path)?.take(100_001).collect::<io::Result<Vec<_>>>()?;
        if entries.len() > 100_000 { return Err(io::Error::other("Lean installation directory entry limit")); }
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            check_cancelled()?;
            *count += 1;
            if *count > 100_000 { return Err(io::Error::other("Lean installation entry limit")); }
            let path = entry.path();
            let kind = entry.file_type()?;
            let relative = path.strip_prefix(root).map_err(io::Error::other)?;
            let relative = relative.to_str().ok_or_else(|| io::Error::other("non UTF8 Lean installation name"))?;
            frame(hash, relative.as_bytes())?;
            if kind.is_dir() {
                hash.update(b"directory")?;
                walk(root, &path, hash, count, size, depth + 1)?;
            } else if kind.is_file() {
                let length = entry.metadata()?.len();
                *size = size.checked_add(length).ok_or_else(|| io::Error::other("Lean installation size overflow"))?;
                if *size > 8 * 1024 * 1024 * 1024 { return Err(io::Error::other("Lean installation exceeds 8 GiB")); }
                hash.update(b"file")?;
                hash.update(&length.to_le_bytes())?;
                hash.file(path,length)?;
            } else if kind.is_symlink() {
                let target = fs::canonicalize(&path)?;
                if !target.starts_with(root) || !fs::metadata(&target)?.is_file() {
                    return Err(io::Error::other("Lean installation link escapes root or targets non-file"));
                }
                let link = fs::read_link(&path)?;
                let text = link.to_str().ok_or_else(|| io::Error::other("non UTF8 Lean installation link"))?;
                hash.update(b"symlink")?;
                frame(hash, text.as_bytes())?;
                let resolved = target.strip_prefix(root).map_err(io::Error::other)?;
                frame(hash, resolved.to_str().ok_or_else(|| io::Error::other("non UTF8 Lean link target"))?.as_bytes())?;
                // The regular target itself is visited and content-hashed by
                // this same complete tree walk. No linked directory is traversed.
            } else { return Err(io::Error::other("Lean installation special file refused")); }
        }
        Ok(())
    }
    let mut hash = platform::Hash::new()?;
    hash.update(b"witchy.scalar.lean-installation.v1")?;
    let mut ordered=reader::Ordered::new(&mut hash)?;
    walk(root, root, &mut ordered, &mut 0, &mut 0, 0)?;
    ordered.finish()?;
    check_cancelled()?;
    hash.finish()
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::platform;
    use std::{fs::{self,File},io::{self,Read},path::{Path,PathBuf},sync::atomic::{AtomicU64,Ordering}};
    fn serial_frame(hash:&mut platform::Hash,bytes:&[u8])->io::Result<()>{hash.update(&(bytes.len() as u64).to_le_bytes())?;hash.update(bytes)}
    fn serial(root: &Path) -> io::Result<String> {
        fn walk(root: &Path, path: &Path, hash: &mut platform::Hash, count: &mut usize, size: &mut u64, depth: usize) -> io::Result<()> {
            if depth > 32 { return Err(io::Error::other("Lean installation directory depth")); }
            let mut entries = fs::read_dir(path)?.take(100_001).collect::<io::Result<Vec<_>>>()?;
            if entries.len() > 100_000 { return Err(io::Error::other("Lean installation directory entry limit")); }
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                *count += 1;
                if *count > 100_000 { return Err(io::Error::other("Lean installation entry limit")); }
                let path = entry.path();
                let kind = entry.file_type()?;
                let relative = path.strip_prefix(root).map_err(io::Error::other)?;
                let relative = relative.to_str().ok_or_else(|| io::Error::other("non UTF8 Lean installation name"))?;
                serial_frame(hash, relative.as_bytes())?;
                if kind.is_dir() {
                    hash.update(b"directory")?;
                    walk(root, &path, hash, count, size, depth + 1)?;
                } else if kind.is_file() {
                    let length = entry.metadata()?.len();
                    *size = size.checked_add(length).ok_or_else(|| io::Error::other("Lean installation size overflow"))?;
                    if *size > 8 * 1024 * 1024 * 1024 { return Err(io::Error::other("Lean installation exceeds 8 GiB")); }
                    hash.update(b"file")?;
                    hash.update(&length.to_le_bytes())?;
                    let mut file = File::open(path)?;
                    let mut remaining = length;
                    let mut buffer = vec![0u8; 65536];
                    while remaining > 0 {
                        let limit = buffer.len().min(remaining as usize);
                        let read = file.read(&mut buffer[..limit])?;
                        if read == 0 { return Err(io::Error::other("Lean installation changed while reading")); }
                        hash.update(&buffer[..read])?;
                        remaining -= read as u64;
                    }
                    if file.read(&mut buffer[..1])? != 0 { return Err(io::Error::other("Lean installation grew while reading")); }
                } else if kind.is_symlink() {
                    let target = fs::canonicalize(&path)?;
                    if !target.starts_with(root) || !fs::metadata(&target)?.is_file() {
                        return Err(io::Error::other("Lean installation link escapes root or targets non-file"));
                    }
                    let link = fs::read_link(&path)?;
                    let text = link.to_str().ok_or_else(|| io::Error::other("non UTF8 Lean installation link"))?;
                    hash.update(b"symlink")?;
                    serial_frame(hash, text.as_bytes())?;
                    let resolved = target.strip_prefix(root).map_err(io::Error::other)?;
                    serial_frame(hash, resolved.to_str().ok_or_else(|| io::Error::other("non UTF8 Lean link target"))?.as_bytes())?;
                    // The regular target itself is visited and content-hashed by
                    // this same complete tree walk. No linked directory is traversed.
                } else { return Err(io::Error::other("Lean installation special file refused")); }
            }
            Ok(())
        }
        let mut hash = platform::Hash::new()?;
        hash.update(b"witchy.scalar.lean-installation.v1")?;
        walk(root, root, &mut hash, &mut 0, &mut 0, 0)?;
        hash.finish()
    }
    struct Tree(PathBuf);
    impl Tree{
        fn new()->Self{static NEXT:AtomicU64=AtomicU64::new(0);let path=std::env::temp_dir().join(format!("witchy-prefetch-{}-{}",std::process::id(),NEXT.fetch_add(1,Ordering::Relaxed)));fs::create_dir(&path).unwrap();Self(fs::canonicalize(path).unwrap())}
        fn parity(&self)->String{let actual=super::installation(&self.0).unwrap();assert_eq!(actual,serial(&self.0).unwrap());actual}
    }
    impl Drop for Tree{fn drop(&mut self){let _=fs::remove_dir_all(&self.0);}}
    #[test]
    fn serial_v1_parity_empty_nested_large_links_and_mutations(){
        use std::os::unix::fs::symlink;
        let tree=Tree::new();tree.parity();fs::create_dir_all(tree.0.join("a/deep/empty")).unwrap();
        fs::write(tree.0.join("empty"),[]).unwrap();fs::write(tree.0.join("b"),b"framed small file").unwrap();
        let bytes=(0..65_536*19+11).map(|i|(i%251)as u8).collect::<Vec<_>>();fs::write(tree.0.join("a/large"),bytes).unwrap();
        for i in 0..9{fs::write(tree.0.join(format!("z{i}")),vec![i as u8;65_536*(i+1)+17]).unwrap();}
        symlink("a/large",tree.0.join("link")).unwrap();symlink("../empty",tree.0.join("a/empty-link")).unwrap();
        let original=tree.parity();fs::write(tree.0.join("b"),b"changed content").unwrap();let content=tree.parity();assert_ne!(original,content);
        fs::rename(tree.0.join("b"),tree.0.join("renamed")).unwrap();let renamed=tree.parity();assert_ne!(content,renamed);
        fs::remove_file(tree.0.join("link")).unwrap();symlink("empty",tree.0.join("link")).unwrap();assert_ne!(renamed,tree.parity());
    }
    #[test]
    fn invalid_links_special_names_and_depth_remain_fail_closed(){
        use std::os::{unix::{fs::symlink,ffi::OsStringExt,net::UnixListener}};
        let tree=Tree::new();let outside=Tree::new();fs::write(outside.0.join("file"),b"outside").unwrap();
        fs::create_dir(tree.0.join("directory")).unwrap();
        for target in [outside.0.join("file"),PathBuf::from("directory"),PathBuf::from("missing")]{
            symlink(target,tree.0.join("link")).unwrap();assert!(super::installation(&tree.0).is_err());assert!(serial(&tree.0).is_err());fs::remove_file(tree.0.join("link")).unwrap();
        }
        let socket=tree.0.join("socket");let listener=UnixListener::bind(&socket).unwrap();assert!(super::installation(&tree.0).is_err());assert!(serial(&tree.0).is_err());drop(listener);fs::remove_file(socket).unwrap();
        let invalid=tree.0.join(std::ffi::OsString::from_vec(vec![255]));
        match fs::write(&invalid,b"name") {
            Ok(())=>{assert!(super::installation(&tree.0).is_err());assert!(serial(&tree.0).is_err());fs::remove_file(invalid).unwrap();},
            // APFS can reject the fixture itself with EILSEQ before our walker.
            Err(error)=>assert_eq!(error.raw_os_error(),Some(92),"unexpected fixture creation failure"),
        }
        let mut path=tree.0.clone();for _ in 0..33{path=path.join("d");fs::create_dir(&path).unwrap();}assert!(super::installation(&tree.0).is_err());assert!(serial(&tree.0).is_err());
    }
    #[test]
    fn changed_lengths_and_queued_errors_join_readers_on_repeated_calls(){
        let tree=Tree::new();let path=tree.0.join("file");fs::write(&path,vec![73;65_536*19]).unwrap();
        for expected in [1,65_536*19+1]{let mut hash=platform::Hash::new().unwrap();let mut reader=super::reader::Ordered::new(&mut hash).unwrap();reader.file(path.clone(),expected).unwrap();assert!(reader.finish().is_err());}
        for _ in 0..8{
            let mut hash=platform::Hash::new().unwrap();let mut reader=super::reader::Ordered::new(&mut hash).unwrap();reader.file(tree.0.join("missing"),1).unwrap();
            for _ in 0..3{reader.file(path.clone(),65_536*19).unwrap();}assert!(reader.finish().is_err());
        }
        tree.parity();
    }
    #[test]
    fn early_walk_error_cancels_four_pending_large_files(){
        use std::os::unix::net::UnixListener;
        let tree=Tree::new();for i in 0..4{fs::write(tree.0.join(format!("a{i}")),vec![i as u8;65_536*19]).unwrap();}
        let socket=tree.0.join("zzsocket");let listener=UnixListener::bind(&socket).unwrap();
        // Sorted order schedules four multi-chunk files before the walker error.
        // Readers may be blocked on full queues; installation must join them all.
        for _ in 0..4{assert!(super::installation(&tree.0).is_err());}
        drop(listener);fs::remove_file(socket).unwrap();tree.parity();
    }
    #[test]
    fn unwinding_drops_and_joins_pending_readers(){
        let tree=Tree::new();let path=tree.0.join("file");fs::write(&path,vec![17;65_536*19]).unwrap();
        let mut hash=platform::Hash::new().unwrap();let unwind=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||{
            let mut reader=super::reader::Ordered::new(&mut hash).unwrap();for _ in 0..4{reader.file(path.clone(),65_536*19).unwrap();}panic!("exercise reader cleanup");
        }));assert!(unwind.is_err());tree.parity();
    }
    #[test]
    fn sha384_policy_pin_matches_the_standard_abc_vector() {
        assert_eq!(super::pin(b"abc").unwrap(),
            concat!("sha384:cb00753f45a35e8bb5a03d699ac65007272c32ab0eded163",
                    "1a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7"));
    }
    #[test]
    fn standard_sha256_vectors() {
        assert_eq!(super::bytes(b"").unwrap(), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(super::bytes(b"abc").unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
