//! The desktop shell's own small files: window geometry and the instance
//! endpoint. They live in the profile, or beside a portable archive's data.
//!
//! The writer lock refuses a link at its name (`pc_core::lock`), and these
//! follow the same rule: a symbolic link or a second name put at
//! `window.json` or `desktop-instance.port` must not have somebody else's
//! file truncated and overwritten. So the bytes go into a fresh file under a
//! name nobody else has (`create_new` never follows a link), and that file is
//! renamed over the name — which replaces the link itself and leaves what it
//! pointed to as it was.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Replace `path` with `bytes`, never writing through whatever is there.
///
/// A temporary left behind by a failure keeps its unique name and is not
/// removed by path: only this process knew it, and the next write uses a new
/// one. Something else may have taken that name since, so deleting it could
/// delete somebody else's file. Kept, it is reported instead: the error names
/// the destination, the temporary and the fact that it was kept, so the
/// shell's log or dialog says what is left on disk (el-5cvv6, B2). The
/// error kind stays that of the failed operation.
pub(crate) fn replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let refused = |why: &str| io::Error::other(format!("cannot replace {}: {why}", path.display()));
    let dir = path.parent().ok_or_else(|| refused("no parent folder"))?;
    let name = path
        .file_name()
        .ok_or_else(|| refused("no file name"))?
        .to_string_lossy();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = dir.join(format!(
        ".{name}.{}.{nanos}.{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // Nothing of ours exists yet if this fails: `create_new` made no file.
    let mut file = options.open(&tmp).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!(
                "cannot replace {}: cannot create the temporary file {}: {e}",
                path.display(),
                tmp.display()
            ),
        )
    })?;
    let retained = |step: &str, e: io::Error| {
        io::Error::new(
            e.kind(),
            format!(
                "cannot replace {}: {step} failed: {e}; the temporary file {} was retained and must be removed by hand",
                path.display(),
                tmp.display()
            ),
        )
    };
    file.write_all(bytes).map_err(|e| retained("write", e))?;
    file.sync_all().map_err(|e| retained("sync", e))?;
    drop(file);
    std::fs::rename(&tmp, path).map_err(|e| retained("rename", e))
}

#[cfg(all(test, unix))]
mod tests {
    use crate::geometry::Geometry;
    use crate::instance::Instance;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    /// A folder put at the sidecar's name: rename fails with EISDIR after
    /// the temporary is written. The folder and its content stay, the
    /// temporary stays, and the caller's error names both.
    fn check_reported(dir: &Path, name: &str, error: String, bytes: &[u8]) {
        let destination = dir.join(name);
        assert_eq!(
            std::fs::read(destination.join("foreign-payload")).unwrap(),
            b"foreign 7741"
        );
        let left: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                let n = p.file_name().unwrap().to_string_lossy();
                n.starts_with(&format!(".{name}.")) && n.ends_with(".tmp")
            })
            .collect();
        assert_eq!(left.len(), 1, "{left:?}");
        let tmp = &left[0];
        assert_eq!(std::fs::read(tmp).unwrap(), bytes);
        let mode = std::fs::metadata(tmp).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(
            error.contains(&destination.display().to_string()),
            "{error}"
        );
        assert!(error.contains(&tmp.display().to_string()), "{error}");
        assert!(error.contains("rename failed"), "{error}");
        assert!(error.contains("retained"), "{error}");
    }

    fn occupied(dir: &Path, name: &str) {
        std::fs::create_dir(dir.join(name)).unwrap();
        std::fs::write(dir.join(name).join("foreign-payload"), b"foreign 7741").unwrap();
    }

    #[test]
    fn a_geometry_that_cannot_be_published_reports_the_kept_temporary() {
        let tmp = tempfile::tempdir().unwrap();
        occupied(tmp.path(), "window.json");
        let geometry = Geometry {
            x: 23,
            y: 41,
            width: 913,
            height: 677,
            maximized: false,
        };
        let error = geometry.save(&tmp.path().join("window.json")).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::IsADirectory);
        check_reported(
            tmp.path(),
            "window.json",
            error.to_string(),
            &serde_json::to_vec(&geometry).unwrap(),
        );
    }

    #[test]
    fn an_instance_endpoint_that_cannot_be_published_reports_the_kept_temporary() {
        let tmp = tempfile::tempdir().unwrap();
        occupied(tmp.path(), "desktop-instance.port");
        let Err(error) = Instance::claim(tmp.path(), false, Duration::from_millis(200)) else {
            panic!("the claim must fail while its endpoint cannot be written");
        };
        let error = format!("{error:#}");
        let left: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".desktop-instance.port."))
            .collect();
        let port = std::fs::read(tmp.path().join(&left[0])).unwrap();
        check_reported(tmp.path(), "desktop-instance.port", error, &port);
    }
}
