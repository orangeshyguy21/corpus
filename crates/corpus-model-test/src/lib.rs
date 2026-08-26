//! Cross-process serialization for tests using the single local model.

use std::fs::{File, OpenOptions};
use std::io::{Error, ErrorKind, Result, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const MODEL_LOCK_ENV: &str = "CORPUS_QWEN38_LOCK";

#[derive(Debug)]
pub struct ModelLease {
    file: File,
    path: PathBuf,
}

impl ModelLease {
    pub fn acquire(scenario: &str) -> Result<Self> {
        let path = lock_path();
        let file = open_lock(&path)?;
        file.lock()?;
        write_owner(&file, scenario)?;
        Ok(Self { file, path })
    }

    pub fn try_acquire(scenario: &str) -> Result<Self> {
        let path = lock_path();
        let file = open_lock(&path)?;
        file.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => Error::new(
                ErrorKind::WouldBlock,
                format!("Qwen3.8 integration lease is held: {}", path.display()),
            ),
            std::fs::TryLockError::Error(error) => error,
        })?;
        write_owner(&file, scenario)?;
        Ok(Self { file, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ModelLease {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn lock_path() -> PathBuf {
    std::env::var_os(MODEL_LOCK_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("corpus-qwen38-integration.lock"))
}

fn open_lock(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
}

fn write_owner(mut file: &File, scenario: &str) -> Result<()> {
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    writeln!(
        file,
        "pid={} scenario={} acquired_epoch={}",
        std::process::id(),
        scenario,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    )?;
    file.sync_data()
}
