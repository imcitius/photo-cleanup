//! The layout note at the boundaries where another program can act
//! (el-usdqi, el-5vue3 R1/D1/D2). Each test stages that program through the
//! seam of [`crate::anchored`] at the exact step: before the temporary is
//! created, after it is written, before it is published. Nothing is ever
//! removed (el-1y8uo B1). What is checked is what stays on disk and what
//! the error says.

use super::*;
use crate::anchored::seam::{self, Stage};
use std::fs;
use std::io;

const FOREIGN_TMP: &[u8] = b"foreign temporary payload";
const FOREIGN_DST: &[u8] = b"foreign destination";

fn fresh() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("q");
    fs::create_dir(&root).unwrap();
    (tmp, root)
}

/// Everything in `dir`, by name.
fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn aside(p: &Path) -> PathBuf {
    PathBuf::from(format!("{}.retained-own", p.display()))
}

/// R1. At publication another program moves our temporary aside, puts its
/// own file at the temporary's name and another at the note's name. The
/// rename is refused; the cleanup must not delete the stranger at the
/// temporary name, and the error names it.
#[test]
fn independent_layout_refusal_keeps_substituted_temp() {
    let (_t, root) = fresh();
    let _g = seam::set(|stage, p| {
        if stage == Stage::Publish {
            fs::rename(p, aside(p))?;
            fs::write(p, FOREIGN_TMP)?;
            fs::write(p.with_file_name(crate::QUARANTINE_LAYOUT), FOREIGN_DST)?;
        }
        Ok(())
    });
    let r = note(&root, "disk1", Path::new("/mnt/disk1"));
    let e = r.unwrap_err();
    let shown = e.to_string();
    let tmp = names(&root)
        .into_iter()
        .find(|n| n.ends_with(".tmp"))
        .expect("the stranger at the temporary name was deleted");
    let tmp = root.join(tmp);
    assert_eq!(fs::read(&tmp).unwrap(), FOREIGN_TMP);
    assert_eq!(
        fs::read(root.join(crate::QUARANTINE_LAYOUT)).unwrap(),
        FOREIGN_DST
    );
    assert!(
        aside(&tmp).exists(),
        "our own file is retained where it was put"
    );
    assert!(shown.contains(&tmp.display().to_string()), "{shown}");
    assert!(e.retained.iter().any(|r| r.path == tmp), "{:?}", e.retained);
}

/// A temporary name that is already taken: `create_new` refuses, and that
/// refusal gives no right to remove what is there.
#[test]
fn failed_create_never_cleans_up_the_occupied_name() {
    let (_t, root) = fresh();
    let _g = seam::set(|stage, p| {
        if stage == Stage::Create {
            fs::write(p, FOREIGN_TMP)?;
        }
        Ok(())
    });
    let e = note(&root, "disk1", Path::new("/mnt/disk1")).unwrap_err();
    assert_eq!(e.cause.kind(), io::ErrorKind::AlreadyExists, "{e}");
    let left = names(&root);
    assert_eq!(left.len(), 1, "{left:?}");
    assert_eq!(fs::read(root.join(&left[0])).unwrap(), FOREIGN_TMP);
    assert!(e.retained.is_empty(), "{:?}", e.retained);
}

/// Writing fails. Nothing is removed (el-1y8uo B1): our own temporary
/// stays and is named; one that was swapped for a stranger's file stays
/// too, and both the stranger and our lost file are named.
#[test]
fn write_or_sync_failure_retains_and_reports_unproved_temp() {
    // Ours, unchanged: left in place and named.
    let (_t, root) = fresh();
    let _g = seam::set(|stage, _| match stage {
        Stage::Written => Err(io::Error::other("injected sync failure")),
        _ => Ok(()),
    });
    let e = note(&root, "disk1", Path::new("/mnt/disk1")).unwrap_err();
    assert!(e.to_string().contains("injected sync failure"), "{e}");
    let left = names(&root);
    assert_eq!(left.len(), 1, "{left:?}");
    assert!(
        e.retained.iter().any(|r| r.path == root.join(&left[0])),
        "{e}"
    );
    drop(_g);

    // Swapped before the failure: the stranger stays and is named.
    let (_t, root) = fresh();
    let _g = seam::set(|stage, p| match stage {
        Stage::Written => {
            fs::rename(p, aside(p))?;
            fs::write(p, FOREIGN_TMP)?;
            Err(io::Error::other("injected sync failure"))
        }
        _ => Ok(()),
    });
    let e = note(&root, "disk1", Path::new("/mnt/disk1")).unwrap_err();
    let tmp = root.join(
        names(&root)
            .into_iter()
            .find(|n| n.ends_with(".tmp"))
            .expect("stranger deleted"),
    );
    assert_eq!(fs::read(&tmp).unwrap(), FOREIGN_TMP);
    assert!(aside(&tmp).exists());
    assert!(e.to_string().contains(&tmp.display().to_string()), "{e}");
}

/// D1. The destination is free, but the temporary was swapped for a
/// stranger's file just before publication: the rename publishes that
/// file. It is not accepted as our note — the call fails, names it, and
/// leaves it where it is.
#[test]
fn layout_publication_does_not_accept_a_substituted_source() {
    let (_t, root) = fresh();
    let _g = seam::set(|stage, p| {
        if stage == Stage::Publish {
            fs::rename(p, aside(p))?;
            fs::write(p, b"foreign publication")?;
        }
        Ok(())
    });
    let e = note(&root, "disk1", Path::new("/mnt/disk1")).unwrap_err();
    let layout = root.join(crate::QUARANTINE_LAYOUT);
    assert_eq!(fs::read(&layout).unwrap(), b"foreign publication");
    assert!(e.retained.iter().any(|r| r.path == layout), "{e}");
    assert!(e.to_string().contains(&layout.display().to_string()), "{e}");
    // And the next note does not take that file for a layout either.
    drop(_g);
    assert!(note(&root, "disk1", Path::new("/mnt/disk1")).is_err());
    assert_eq!(fs::read(&layout).unwrap(), b"foreign publication");
}

/// D2. The note creates its folder, then publication is refused. The folder
/// is not removed by name — a stranger's empty folder may bear it by then —
/// it stays, and the error says it was created here.
#[test]
fn layout_failure_does_not_remove_a_substituted_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("collected").join(crate::QUARANTINE_DIR);
    let _g = seam::set(|stage, _| match stage {
        Stage::Publish => Err(io::Error::from_raw_os_error(libc::ENOTSUP)),
        _ => Ok(()),
    });
    let e = note(&root, "disk1", Path::new("/mnt/disk1")).unwrap_err();
    assert!(crate::disk::lacks_exclusive_rename(&e.cause), "{e}");
    assert!(root.is_dir());
    // Our own temporary is not removed either (el-1y8uo B1): named.
    let left = names(&root);
    assert_eq!(left.len(), 1, "{left:?}");
    assert!(
        e.retained.iter().any(|r| r.path == root.join(&left[0])),
        "{e}"
    );
    for created in [&root, root.parent().unwrap()] {
        assert!(
            e.retained.iter().any(|r| r.path == *created),
            "{} not reported: {e}",
            created.display()
        );
    }
}

/// An existing note is admitted by what it says it is. One written by the
/// earlier version is read and accepted when it already records the label,
/// never rewritten; arbitrary JSON that merely parses is not a layout.
#[test]
fn an_existing_layout_is_admitted_by_its_schema_and_never_silently_rewritten() {
    let (_t, root) = fresh();
    let layout = root.join(crate::QUARANTINE_LAYOUT);
    let old = br#"{"disk1": "/mnt/disk1"}"#;
    fs::write(&layout, old).unwrap();
    note(&root, "disk1", Path::new("/mnt/disk1")).unwrap();
    assert_eq!(fs::read(&layout).unwrap(), old);
    assert_eq!(
        read(&root).get("disk1").map(String::as_str),
        Some("/mnt/disk1")
    );
    assert!(note(&root, "disk2", Path::new("/mnt/disk2")).is_err());
    assert_eq!(fs::read(&layout).unwrap(), old, "an old note was rewritten");

    for stranger in [&br#"{"notes": "my own json"}"#[..], br#"{"a": 1}"#, b"[]"] {
        fs::write(&layout, stranger).unwrap();
        assert!(note(&root, "disk1", Path::new("/mnt/disk1")).is_err());
        assert_eq!(fs::read(&layout).unwrap(), stranger);
    }

    // The versioned note grows, and keeps what it had.
    fs::remove_file(&layout).unwrap();
    note(&root, "disk1", Path::new("/mnt/disk1")).unwrap();
    note(&root, "disk2", Path::new("/mnt/disk2")).unwrap();
    let disks = read(&root);
    assert_eq!(disks.len(), 2);
    let raw = fs::read_to_string(&layout).unwrap();
    assert!(raw.contains(FORMAT), "{raw}");
}

/// B1 (el-1y8uo; user decision 2026-10-04 "never delete"). An own
/// temporary that cannot be published — refused publication, failed write
/// or sync — is never removed: it stays where it was, and the refusal names
/// it as retained.
#[test]
fn an_unpublished_own_temporary_is_left_and_reported() {
    for stage_failing in [Stage::Written, Stage::Publish] {
        let (_t, root) = fresh();
        let _g = seam::set(move |stage, _| {
            if stage == stage_failing {
                return Err(io::Error::from_raw_os_error(libc::ENOTSUP));
            }
            Ok(())
        });
        let e = note(&root, "disk1", Path::new("/mnt/disk1")).unwrap_err();
        let tmp = root.join(
            names(&root)
                .into_iter()
                .find(|n| n.ends_with(".tmp"))
                .unwrap_or_else(|| panic!("{stage_failing:?}: own temporary was removed")),
        );
        assert!(
            fs::read_to_string(&tmp).unwrap().contains(FORMAT),
            "{stage_failing:?}: not our own temporary"
        );
        assert!(
            e.retained.iter().any(|r| r.path == tmp),
            "{stage_failing:?}: own temporary not reported: {e}"
        );
        assert!(e.to_string().contains(&tmp.display().to_string()), "{e}");
    }
}

/// Adjacent R1 (el-1y8uo B1). Publication meets a stranger at the layout
/// name; whatever happens to the temporary's name afterwards, nothing is
/// unlinked. The folder name contains `late-temp-review`, the marker of the
/// reviewer's native interposer (`/tmp/el-usdqi-review-87e1804/fault.c`):
/// under it (`PC_REVIEW_INTERPOSER=1`), the destination is planted by the
/// `renameatx_np` hook and any `unlinkat` of the temporary first swaps it
/// for `foreign late temporary`; without it, the seam plants the
/// destination. Either way the stranger at the layout name and the entry
/// at the temporary's name survive, and the temporary is reported.
#[test]
fn late_temp_substitution_preserves_foreign_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("late-temp-review");
    fs::create_dir(&root).unwrap();
    let native = std::env::var_os("PC_REVIEW_INTERPOSER").is_some();
    let _g = seam::set(move |stage, p| {
        if stage == Stage::Publish && !native {
            fs::write(p.with_file_name(crate::QUARANTINE_LAYOUT), FOREIGN_DST)?;
        }
        Ok(())
    });
    let e = note(&root, "disk1", Path::new("/mnt/disk1")).unwrap_err();
    assert_eq!(
        fs::read(root.join(crate::QUARANTINE_LAYOUT)).unwrap(),
        FOREIGN_DST
    );
    let tmp = root.join(
        names(&root)
            .into_iter()
            .find(|n| n.ends_with(".tmp"))
            .expect("the entry at the temporary's name was deleted"),
    );
    if aside(&tmp).exists() {
        assert_eq!(fs::read(&tmp).unwrap(), b"foreign late temporary");
        assert!(fs::read_to_string(aside(&tmp)).unwrap().contains(FORMAT));
    } else {
        assert!(fs::read_to_string(&tmp).unwrap().contains(FORMAT));
    }
    assert!(e.retained.iter().any(|r| r.path == tmp), "{e}");
}
