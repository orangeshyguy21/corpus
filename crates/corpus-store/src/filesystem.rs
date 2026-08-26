//! Filesystem mutation primitives shared by persisted store records.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::Result;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Replace one file atomically using a unique temporary file in the same
/// directory. `create_new` prevents concurrent writers from sharing a staging
/// file; the guard removes incomplete staging files on every error path.
pub(crate) fn atomic_write(path: &Path, contents: impl AsRef<[u8]>) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "file has no parent"))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "file has no name"))?;
    let (mut temporary, mut file) = create_temporary(parent, file_name)?;

    file.write_all(contents.as_ref())?;
    file.sync_all()?;
    drop(file);
    fs::rename(temporary.path(), path)?;
    temporary.committed = true;
    Ok(())
}

fn create_temporary(parent: &Path, file_name: &std::ffi::OsStr) -> io::Result<(TempFile, File)> {
    for _ in 0..32 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let mut temporary_name = OsString::from(".");
        temporary_name.push(file_name);
        temporary_name.push(format!(".tmp-{}-{sequence}", std::process::id()));
        let path = parent.join(temporary_name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => {
                return Ok((
                    TempFile {
                        path,
                        committed: false,
                    },
                    file,
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique atomic-write staging file",
    ))
}

struct TempFile {
    path: PathBuf,
    committed: bool,
}

impl TempFile {
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    fn test_dir(label: &str) -> PathBuf {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "corpus-store-atomic-{label}-{}-{sequence}",
            std::process::id()
        ))
    }

    #[test]
    fn replacement_is_complete_and_leaves_no_staging_file() {
        let dir = test_dir("replace");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("record.yaml");
        fs::write(&path, "old").unwrap();

        atomic_write(&path, "new record").unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "new record");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_replacement_preserves_the_target_and_cleans_staging() {
        let dir = test_dir("failure");
        let target = dir.join("record.yaml");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("sentinel"), "still here").unwrap();

        assert!(atomic_write(&target, "replacement").is_err());

        assert_eq!(
            fs::read_to_string(target.join("sentinel")).unwrap(),
            "still here"
        );
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_replacements_never_share_staging_or_publish_partial_bytes() {
        let dir = test_dir("concurrent");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("record.json");
        let payloads: Vec<String> = (0..8)
            .map(|index| format!("{index}:{}", "x".repeat(64 * 1024)))
            .collect();
        let barrier = Arc::new(Barrier::new(payloads.len()));
        let writers: Vec<_> = payloads
            .iter()
            .cloned()
            .map(|payload| {
                let path = path.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    atomic_write(&path, payload).unwrap();
                })
            })
            .collect();

        for writer in writers {
            writer.join().unwrap();
        }

        let published = fs::read_to_string(&path).unwrap();
        assert!(payloads.contains(&published));
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        let _ = fs::remove_dir_all(&dir);
    }
}
