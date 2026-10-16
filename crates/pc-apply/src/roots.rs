//! The folders a run works in, held from its start (director decision D2 of
//! el-lvtmk).
//!
//! A sync client or Finder that moves the archive folder while a run goes
//! on turns every later path of the run into a guess: the next photograph's
//! path leads into whatever now bears the folder's name. Each move already
//! notices its own folders moving; this notices the run's roots moving
//! between two moves — before the next file is even opened — and stops the
//! run there instead of collecting one "already gone" per photograph.

use anyhow::Result;
use pc_db::Db;
use std::path::{Path, PathBuf};

use pc_core::anchored::Dir;

/// The roots of the runs this database knows that hold any path of the run,
/// each opened once and held.
pub struct RunRoots {
    held: Vec<(PathBuf, Dir)>,
}

impl RunRoots {
    pub fn hold<'a>(db: &Db, paths: impl IntoIterator<Item = &'a str>) -> Result<RunRoots> {
        let roots: Vec<PathBuf> = db.all_run_roots()?.into_iter().map(PathBuf::from).collect();
        let mut used: Vec<PathBuf> = Vec::new();
        for p in paths {
            for r in &roots {
                if Path::new(p).starts_with(r) && !used.contains(r) {
                    used.push(r.clone());
                }
            }
        }
        // A root that cannot be opened now holds nothing of this run that
        // could move under it; the moves themselves say why they cannot
        // reach their files. On Windows a root is held sharing everything:
        // it is held for the whole run, and the user is not locked out of
        // their own archive for that long (el-61ar8 §2.1) — while any
        // handle inside it is open, Windows refuses to rename it anyway.
        let held = used
            .into_iter()
            .filter_map(|r| Dir::open_following(&r).ok().map(|d| (r, d)))
            .collect();
        Ok(RunRoots { held })
    }

    /// Every root's path still leads to the folder held for it.
    pub fn check(&self) -> std::result::Result<(), crate::FolderMoved> {
        for (path, dir) in &self.held {
            if !dir.is_at(path) {
                let now = dir.current_path();
                return Err(crate::FolderMoved {
                    reason: match now {
                        Some(p) => pc_core::tf!(
                            "папку прогона {0} переместили во время работы; система называет её \
                             теперь {1} (не проверено)",
                            "the run's folder {0} was moved while the run went on; the system now \
                             names it {1} (unverified)",
                            path.display(),
                            p.display()
                        ),
                        None => pc_core::tf!(
                            "папку прогона {0} переместили во время работы",
                            "the run's folder {0} was moved while the run went on",
                            path.display()
                        ),
                    },
                });
            }
        }
        Ok(())
    }
}
