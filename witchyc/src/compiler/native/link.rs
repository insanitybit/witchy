use super::{NativeObject, Times};
use std::{
    fs,
    io,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

static NEXT: AtomicU64 = AtomicU64::new(0);
const RUNTIME_ARCHIVE: &str = env!("WITCHY_NATIVE_ARCHIVE_FILE");
const ARCHIVE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/", env!("WITCHY_NATIVE_ARCHIVE_FILE")));

/// All linker writes are private, on the output filesystem, until final publication.
pub(in crate::compiler) struct Image(PathBuf);

impl Image {
    fn stage(parent: &Path) -> io::Result<Self> {
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        for _ in 0..64 {
            let path = parent.join(format!(
                ".witchy-native-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match builder.create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not reserve a private native link directory",
        ))
    }

    pub fn publish(&self, output: &Path) -> Result<(), String> {
        fs::rename(self.0.join("image"), output).map_err(|e| format!("{}: {e}", output.display()))
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(in crate::compiler) fn link(
    output: &Path,
    object: &NativeObject,
    times: &mut Times,
) -> Result<Image, String> {
    let phase = Instant::now();
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let image = Image::stage(parent).map_err(|e| e.to_string())?;
    fs::write(image.0.join("program.o"), &object.bytes).map_err(|e| e.to_string())?;
    stage_runtime(&image.0.join(RUNTIME_ARCHIVE), object.requires_aws_lc)
        .map_err(|e| e.to_string())?;
    times.object_ns += phase.elapsed().as_nanos();

    let phase = Instant::now();
    #[cfg(target_os = "macos")]
    let mut linker =
        Command::new(option_env!("WITCHY_NATIVE_SANITIZER_CLANG").unwrap_or("/usr/bin/clang"));
    #[cfg(target_os = "macos")]
    linker.args(["-arch", "arm64", "-Wl,-dead_strip"]);
    #[cfg(witchy_native_sanitized)]
    linker.args(["--ld-path=/usr/bin/ld", "-fsanitize=address,undefined"]);
    #[cfg(all(witchy_native_aws_lc, target_os = "macos"))]
    if object.requires_aws_lc {
        let lib = Path::new(env!("WITCHY_NATIVE_AWS_LC_INSTALLED")).join("lib");
        linker
            .arg(lib.join("libaws_lc_fips_0_14_4_crypto.dylib"))
            .args(["-Xlinker", "-rpath", "-Xlinker"])
            .arg(&lib)
            .arg("-Wl,-dead_strip_dylibs");
    }
    #[cfg(target_os = "linux")]
    let mut linker = Command::new("cc");
    #[cfg(target_os = "linux")]
    linker.args(["-pie", "-Wl,-z,now,-z,noexecstack"]);
    linker
        .current_dir(&image.0)
        .args([RUNTIME_ARCHIVE, "program.o", "-o", "image"]);
    #[cfg(all(witchy_native_aws_lc, target_os = "linux"))]
    {
        fs::write(
            image.0.join("aws_lc_fips.a"),
            include_bytes!(concat!(
                env!("WITCHY_NATIVE_AWS_LC_INSTALLED"),
                "/lib/libaws_lc_fips_0_14_4_crypto.a"
            )),
        )
        .map_err(|e| e.to_string())?;
        linker.arg("aws_lc_fips.a").arg("-lpthread");
    }
    let linked = linker
        .output()
        .map_err(|e| format!("start native linker: {e}"))?;
    times.link_ns = phase.elapsed().as_nanos();
    if !linked.status.success() {
        return Err(format!(
            "native link failed: {}",
            String::from_utf8_lossy(&linked.stderr).trim()
        ));
    }
    Ok(image)
}

fn stage_runtime(path: &Path, _requires_aws_lc: bool) -> io::Result<()> {
    #[cfg(all(witchy_native_aws_lc, target_os = "macos"))]
    if !_requires_aws_lc {
        use std::io::Write;
        const PREFIX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/runtime_plain_prefix"));
        const SUFFIX_LEN: usize = u64::from_le_bytes(*include_bytes!(concat!(
            env!("OUT_DIR"), "/runtime_shared_suffix_len"
        ))) as usize;
        let mut file = fs::File::create(path)?;
        file.write_all(PREFIX)?;
        return file.write_all(&ARCHIVE[ARCHIVE.len() - SUFFIX_LEN..]);
    }
    fs::write(path, ARCHIVE)
}
