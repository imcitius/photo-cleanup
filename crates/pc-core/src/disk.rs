//! Physical-disk awareness.
//!
//! Two things depend on knowing which filesystem a path lives on:
//!
//!  * the walk shards its readers per spindle, because an Unraid array is not
//!    striped — each file lives entirely on one disk, and parallel readers pay
//!    off only across disks, not within one;
//!  * quarantine is written to the *same* filesystem as the source, so moving
//!    a bundle is a `rename(2)` instead of a copy.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::volume::device_of;
use crate::QUARANTINE_DIR;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disk {
    /// Identity of the filesystem: `st_dev` on Unix, the drive or share on
    /// Windows. What everything else keys off.
    pub dev: u64,
    /// Mount point, i.e. the highest ancestor still on the same `st_dev`.
    pub mount: PathBuf,
    /// Short human label: `disk3` for `/mnt/disk3`, `root` for `/`.
    pub label: String,
}

impl Disk {
    pub fn quarantine_root(&self) -> PathBuf {
        self.mount.join(QUARANTINE_DIR)
    }

    /// Path of `src` relative to this disk's mount point, used to mirror the
    /// original layout inside the quarantine tree.
    pub fn relative<'a>(&self, src: &'a Path) -> &'a Path {
        src.strip_prefix(&self.mount).unwrap_or(src)
    }
}

/// Highest ancestor of `path` that still lives on the same filesystem.
pub fn mount_root(path: &Path) -> io::Result<PathBuf> {
    let path = path.canonicalize()?;
    let md = fs::metadata(&path)?;
    let dev = device_of(&md, &path);

    let mut cur = if md.is_dir() {
        path.clone()
    } else {
        path.parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| path.clone())
    };

    loop {
        let Some(parent) = cur.parent() else {
            return Ok(cur);
        };
        match fs::metadata(parent) {
            Ok(m) if device_of(&m, parent) == dev => cur = parent.to_path_buf(),
            // A different device, or an unreadable parent: `cur` is the top.
            _ => return Ok(cur),
        }
    }
}

/// `st_dev` of `path`, or of its nearest existing ancestor.
///
/// A destination directory usually does not exist yet — the point of asking
/// is to find out which filesystem it *will* be created on, so that a move
/// into it is a rename and not a copy of the whole archive.
pub fn dev_of_nearest_existing(path: &Path) -> io::Result<u64> {
    let mut cur = path;
    loop {
        if let Ok(md) = fs::metadata(cur) {
            return Ok(device_of(&md, cur));
        }
        cur = cur.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                crate::tf!(
                    "не найти существующий предок для {0}",
                    "cannot find an existing ancestor of {0}",
                    path.display()
                ),
            )
        })?;
    }
}

fn label_for(mount: &Path) -> String {
    match mount.file_name().and_then(|s| s.to_str()) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => "root".to_string(),
    }
}

/// Caches the mount lookup, which costs a `stat` per ancestor.
#[derive(Debug, Default)]
pub struct DiskMap {
    by_dev: HashMap<u64, Disk>,
}

impl DiskMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn resolve(&mut self, path: &Path) -> io::Result<Disk> {
        let dev = device_of(&fs::metadata(path)?, path);
        if let Some(d) = self.by_dev.get(&dev) {
            return Ok(d.clone());
        }
        let mount = mount_root(path)?;
        let disk = Disk {
            dev,
            mount: mount.clone(),
            label: label_for(&mount),
        };
        self.by_dev.insert(dev, disk.clone());
        Ok(disk)
    }

    pub fn known(&self) -> impl Iterator<Item = &Disk> {
        self.by_dev.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_and_caches() {
        let tmp = tempfile::tempdir().unwrap();
        let mut map = DiskMap::new();
        let a = map.resolve(tmp.path()).unwrap();
        let b = map.resolve(tmp.path()).unwrap();
        assert_eq!(a, b);
        assert_eq!(map.known().count(), 1);
    }

    #[test]
    fn quarantine_is_on_the_same_mount() {
        let tmp = tempfile::tempdir().unwrap();
        let mut map = DiskMap::new();
        let disk = map.resolve(tmp.path()).unwrap();
        assert!(disk.quarantine_root().starts_with(&disk.mount));
    }

    #[test]
    fn relative_strips_the_mount_prefix() {
        let disk = Disk {
            dev: 1,
            mount: PathBuf::from("/mnt/disk3"),
            label: "disk3".into(),
        };
        assert_eq!(
            disk.relative(Path::new("/mnt/disk3/data/foto/X")),
            Path::new("data/foto/X")
        );
    }
}
