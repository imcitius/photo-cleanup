//! Tests only: a derived bundle as an earlier version left it in quarantine.
//!
//! `derived clean` moves nothing any more (el-126jk, el-2rpxq), and this
//! crate has no forward move for a bundle (el-1bzcw). What earlier versions
//! moved still sits in people's quarantines, and `undo` and `purge` answer
//! for it — so their tests lay such an entry down the way those versions
//! did, on disposable fixtures only: the journal row with its manifest first,
//! then a plain rename beside the bundle, then the row closed and the index
//! told. Compiled into this crate's own tests and nowhere else.

use crate::files;
use anyhow::Result;
use pc_db::{Bundle, BundleState, Db, JournalStatus};
use std::fs;
use std::path::{Path, PathBuf};

/// Quarantine `b` as an earlier version did; the journal entry's id.
///
/// The destination is the hidden folder beside the bundle, the one those
/// versions used by default; something already there is a broken fixture,
/// not a race to stage, so the rename refuses it.
pub(crate) fn moved_by_an_earlier_version(db: &Db, run_id: i64, b: &Bundle) -> Result<i64> {
    let src = Path::new(&b.path);
    let dst = src
        .parent()
        .unwrap()
        .join(pc_core::QUARANTINE_DIR)
        .join(src.file_name().unwrap());
    assert!(
        fs::symlink_metadata(&dst).is_err(),
        "fixture: {} is taken",
        dst.display()
    );
    let dst_str = dst.display().to_string();
    let manifest = [pc_db::Moved {
        src: b.path.clone(),
        dst: dst_str.clone(),
        proof: files::evidence(src)?,
    }];
    let jid = db.journal_begin(&pc_db::NewJournalEntry {
        run_id,
        op: "quarantine",
        target_id: Some(b.id),
        src: &b.path,
        dst: Some(&dst_str),
        size: b.size,
        file_count: b.file_count,
        manifest: &manifest,
    })?;
    fs::create_dir_all(dst.parent().unwrap())?;
    fs::rename(src, &dst)?;
    db.journal_close(
        jid,
        JournalStatus::Done,
        &pc_db::Event {
            moved: &manifest,
            ..pc_db::Event::new("forward", "done")
        },
    )?;
    db.set_bundle_state(b.id, BundleState::Quarantined)?;
    Ok(jid)
}

/// Files, bytes and newest mtime under `root`, as a scan records a bundle.
pub(crate) fn dir_stats(root: &Path) -> (u64, u64, i64) {
    let mut count = 0u64;
    let mut size = 0u64;
    let mut newest = 0i64;
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            let Ok(md) = e.metadata() else { continue };
            newest = newest.max(pc_core::time::mtime_unix(&md));
            if ft.is_dir() {
                stack.push(e.path());
            } else if ft.is_file() {
                count += 1;
                size += md.len();
            }
        }
    }
    if let Ok(md) = fs::metadata(root) {
        newest = newest.max(pc_core::time::mtime_unix(&md));
    }
    (count, size, newest)
}
