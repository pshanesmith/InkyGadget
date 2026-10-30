//! Crash-safe file writing: a power cut must leave the old file or the complete new one, never an empty file
//! (ext4 can persist a rename before the data it points to).

use std::fs::File;
use std::io::{self, Write};
use std::path::Path;

/// Write `bytes` to `path` through a `.tmp` sibling that is flushed to disk before it replaces the target.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    // Persist the rename itself (best effort: some filesystems cannot sync a directory).
    if let Some(dir) = path.parent() {
        if let Ok(d) = File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_the_file_and_leaves_no_temporary_one() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.json");
        write_atomic(&p, b"one").unwrap();
        write_atomic(&p, b"two").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        assert!(!d.path().join("x.tmp").exists());
    }
}
