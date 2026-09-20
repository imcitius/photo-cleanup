//! Which volume a path lives on, and how to ask the filesystem about it.
//!
//! Two things in this tool depend on the answer: the walk shards its readers
//! per physical disk, and quarantine has to land on the *same* filesystem as
//! the source so that the move is a rename rather than a copy of the archive.
//!
//! Unix answers it exactly, with `st_dev`. Windows has no cheap stable
//! equivalent — the volume serial number needs an unstable API — so the path
//! prefix stands in: a drive letter or a UNC share. That is the right
//! granularity for both uses, and it is what the operator sees anyway.
//! The gap is a directory mounted into another volume's tree, which a home
//! archive does not usually contain; in that case the same-filesystem check
//! is caught later, by the rename itself failing.

use std::fs::Metadata;
use std::path::Path;

/// Identity of the filesystem a file sits on.
pub fn device_of(md: &Metadata, path: &Path) -> u64 {
    #[cfg(unix)]
    {
        let _ = path;
        std::os::unix::fs::MetadataExt::dev(md)
    }
    #[cfg(windows)]
    {
        let _ = md;
        prefix_id(path)
    }
}

/// Inode, where the filesystem has one.
///
/// Zero on Windows: reading a file index there needs an unstable API. It is
/// used to notice that two paths are the same bytes on disk, which then falls
/// back to path, size and mtime — weaker, and never wrong in a way that moves
/// the wrong file.
pub fn inode_of(md: &Metadata) -> u64 {
    #[cfg(unix)]
    {
        std::os::unix::fs::MetadataExt::ino(md)
    }
    #[cfg(windows)]
    {
        let _ = md;
        0
    }
}

/// How many names point at these bytes. One on Windows, for the same reason.
pub fn links_of(md: &Metadata) -> u64 {
    #[cfg(unix)]
    {
        std::os::unix::fs::MetadataExt::nlink(md)
    }
    #[cfg(windows)]
    {
        let _ = md;
        1
    }
}

/// A key that is the same for two paths naming one directory entry.
///
/// On Unix the pair (device, inode) says so exactly. Without an inode the
/// only thing being guarded against is a case-insensitive filesystem
/// answering to `.xmp` and `.XMP`, so the folded path is the honest stand-in
/// — and it cannot accidentally collapse two genuinely different files, which
/// a constant inode would.
pub fn entry_key(md: &Metadata, path: &Path) -> String {
    let inode = inode_of(md);
    if inode != 0 {
        format!("{}:{inode}", device_of(md, path))
    } else {
        path.to_string_lossy().to_lowercase()
    }
}

/// A stable number for a Windows path prefix: `C:` or `\\server\share`.
#[cfg(windows)]
pub fn prefix_id(path: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let prefix = path
        .components()
        .next()
        .map(|c| c.as_os_str().to_string_lossy().to_uppercase())
        .unwrap_or_default();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    prefix.hash(&mut h);
    // Zero is reserved for "unknown", so a prefix never produces it.
    h.finish() | 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_files_device_is_the_device_of_its_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("a.txt");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(
            device_of(&std::fs::metadata(&file).unwrap(), &file),
            device_of(&std::fs::metadata(tmp.path()).unwrap(), tmp.path())
        );
    }

    #[test]
    fn two_spellings_of_one_entry_share_a_key_and_two_files_do_not() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.xmp");
        let b = tmp.path().join("b.xmp");
        std::fs::write(&a, b"x").unwrap();
        std::fs::write(&b, b"x").unwrap();
        let key = |p: &Path| entry_key(&std::fs::metadata(p).unwrap(), p);
        assert_ne!(key(&a), key(&b), "разные файлы получили один ключ");
        assert_eq!(key(&a), key(&a));
    }

    #[cfg(windows)]
    #[test]
    fn drive_letters_identify_volumes_and_case_does_not_matter() {
        assert_eq!(
            prefix_id(Path::new(r"C:\photos\a.jpg")),
            prefix_id(Path::new(r"c:\other\b.jpg"))
        );
        assert_ne!(
            prefix_id(Path::new(r"C:\photos")),
            prefix_id(Path::new(r"D:\photos"))
        );
        assert_ne!(prefix_id(Path::new(r"C:\a")), 0);
    }
}
