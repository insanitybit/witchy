use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

pub struct TempDir(PathBuf);

#[allow(dead_code)]
impl TempDir {
    pub fn new(tag: &str) -> Self {
        // Absolute whatever `$TMPDIR` says: a relative value would place this
        // directory inside the shared checkout, where a killed test run leaves
        // it behind (2026-09-19 left 31 such directories in master's tree).
        let root = std::env::temp_dir();
        let root = if root.is_absolute() { root } else { PathBuf::from("/tmp") };
        let path = root.join(format!(
            "witchy-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&path).expect("create test temp directory");
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn mkdir(&self, relative: &str) -> PathBuf {
        let path = self.0.join(relative);
        std::fs::create_dir_all(&path).expect("create test temp subdirectory");
        path
    }

    pub fn write(&self, relative: &str, contents: impl AsRef<[u8]>) -> PathBuf {
        let path = self.0.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create test file parent");
        }
        std::fs::write(&path, contents).expect("write test input");
        path
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        self.path()
    }
}

impl Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        self.path()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
