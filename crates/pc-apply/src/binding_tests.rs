//! The object that was checked is the object that moves (el-3wizg, D8 of
//! diagnosis el-5vue3).
//!
//! Apply re-reads a photograph and its keeper and compares the pixels — and
//! then used to rename whatever bore the photograph's *name* by then. Between
//! the two, another program (a sync client, an import, a second tool) can put
//! a different file under that name, replace the keeper, or replace the
//! folder itself; the move then took an object nobody had checked. These
//! tests stage that moment on the real consumers through [`crate::race`]:
//! after the pixels were compared, before the rename.

use crate::race;
use image::{Rgb, RgbImage};
use pc_db::{Db, JournalStatus};
use pc_family::plan::Candidate;
use std::fs;
use std::path::{Path, PathBuf};

const STRANGER: &[u8] = b"someone else's photograph, never checked";

struct Archive {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    db: Db,
    run: i64,
}

fn archive() -> Archive {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("archive");
    fs::create_dir_all(&dir).unwrap();
    let db = Db::open(&tmp.path().join("test.db")).unwrap();
    let run = db.start_run(&[dir.display().to_string()], "test").unwrap();
    Archive {
        dir,
        _tmp: tmp,
        db,
        run,
    }
}

fn bmp(path: &Path, colour: [u8; 3]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let img = RgbImage::from_pixel(64, 48, Rgb(colour));
    image::DynamicImage::ImageRgb8(img).save(path).unwrap();
}

fn indexed(db: &Db, run: i64, path: &Path) -> i64 {
    db.upsert_file(
        &pc_db::NewFile {
            path: path.display().to_string(),
            name: path.file_name().unwrap().to_string_lossy().into_owned(),
            size: fs::metadata(path).unwrap().len() as i64,
            ..Default::default()
        },
        run,
    )
    .unwrap()
}

/// The tool's own candidate: `copy` is redundant to `keeper`.
fn automatic(db: &Db, run: i64, copy: &Path, keeper: &Path) -> Candidate {
    let keeper_id = indexed(db, run, keeper);
    let file_id = indexed(db, run, copy);
    Candidate {
        file_id,
        family_id: 0,
        path: copy.display().to_string(),
        size: fs::metadata(copy).unwrap().len() as i64,
        role: pc_family::Role::Copy,
        keeper_id,
        keeper_path: keeper.display().to_string(),
        reason: "copy".into(),
        manual: false,
        group_keeper: keeper.display().to_string(),
    }
}

/// The user's own pick: nothing is redundant to anything, the file itself
/// was looked at.
fn manual(db: &Db, run: i64, path: &Path) -> Candidate {
    let file_id = indexed(db, run, path);
    Candidate {
        file_id,
        family_id: 0,
        path: path.display().to_string(),
        size: fs::metadata(path).unwrap().len() as i64,
        role: pc_family::Role::Copy,
        keeper_id: 0,
        keeper_path: String::new(),
        reason: "выбор человека".into(),
        manual: true,
        group_keeper: String::new(),
    }
}

fn last_row(db: &Db) -> (String, String) {
    db.conn
        .query_row(
            "SELECT status, coalesce(note, '') FROM journal ORDER BY id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
}

fn file_state(db: &Db, path: &Path) -> String {
    db.conn
        .query_row(
            "SELECT state FROM files WHERE path = ?1",
            [path.display().to_string()],
            |r| r.get(0),
        )
        .unwrap()
}

/// Everything under `dir`, as relative paths, sorted.
fn tree(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            out.push(p.strip_prefix(dir).unwrap().display().to_string());
            if e.file_type().unwrap().is_dir() {
                stack.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Move the checked file aside and put a stranger under its name.
fn substitute(at: &Path, aside: &Path) {
    fs::rename(at, aside).unwrap();
    fs::write(at, STRANGER).unwrap();
}

#[test]
fn validated_photo_is_not_replaced_before_the_move() {
    for auto in [true, false] {
        let a = archive();
        let keeper = a.dir.join("keeper.bmp");
        let copy = a.dir.join("copy.bmp");
        let aside = a.dir.join("checked-aside.bmp");
        bmp(&keeper, [10, 120, 200]);
        fs::copy(&keeper, &copy).unwrap();
        let checked = fs::read(&copy).unwrap();
        let c = if auto {
            automatic(&a.db, a.run, &copy, &keeper)
        } else {
            manual(&a.db, a.run, &copy)
        };

        let (at, to) = (copy.clone(), aside.clone());
        let _race = race::before_move(move |src, _| {
            if src == at && !to.exists() {
                substitute(&at, &to);
            }
            Ok(())
        });
        let report = crate::apply(&a.db, a.run, &[c], None).unwrap();

        assert_eq!(
            report.done.frames, 0,
            "перенесён непроверенный файл (auto={auto})"
        );
        assert_eq!(fs::read(&copy).unwrap(), STRANGER, "чужой файл тронут");
        assert_eq!(fs::read(&aside).unwrap(), checked, "проверенный тронут");
        let quarantine = a.dir.join(pc_core::QUARANTINE_DIR);
        assert!(
            !quarantine.join("copy.bmp").exists(),
            "в карантине: {:?}",
            tree(&a.dir)
        );
        let (_, why) = &report.refused[0];
        assert!(why.contains(&copy.display().to_string()), "{why}");
        let (status, note) = last_row(&a.db);
        assert_eq!(status, JournalStatus::Failed.as_str(), "{note}");
        assert!(note.contains(&copy.display().to_string()), "{note}");
        assert_eq!(file_state(&a.db, &copy), "present");
    }
}

/// What can happen to the keeper after its pixels were compared.
#[derive(Clone, Copy, Debug)]
enum KeeperFate {
    /// Another file now bears its name.
    Replaced,
    /// The same file, other bytes.
    Rewritten,
    /// Nothing bears its name any more.
    Gone,
}

#[test]
fn keeper_changed_before_the_write_refuses_the_candidate() {
    for fate in [
        KeeperFate::Replaced,
        KeeperFate::Rewritten,
        KeeperFate::Gone,
    ] {
        let a = archive();
        let keeper = a.dir.join("keeper.bmp");
        let copy = a.dir.join("copy.bmp");
        bmp(&keeper, [10, 120, 200]);
        fs::copy(&keeper, &copy).unwrap();
        let checked = fs::read(&copy).unwrap();
        let c = automatic(&a.db, a.run, &copy, &keeper);

        let (k, at) = (keeper.clone(), copy.clone());
        let aside = a.dir.join("keeper-aside.bmp");
        let to = aside.clone();
        let mut done = false;
        let _race = race::before_move(move |src, _| {
            if src == at && !done {
                done = true;
                match fate {
                    KeeperFate::Replaced => {
                        fs::rename(&k, &to).unwrap();
                        bmp(&k, [200, 46, 46]);
                    }
                    KeeperFate::Rewritten => {
                        // Same inode, other picture, a later time.
                        std::thread::sleep(std::time::Duration::from_millis(20));
                        let other = to.with_file_name("other.bmp");
                        bmp(&other, [200, 46, 46]);
                        fs::write(&k, fs::read(&other).unwrap()).unwrap();
                    }
                    KeeperFate::Gone => fs::rename(&k, &to).unwrap(),
                }
            }
            Ok(())
        });
        let report = crate::apply(&a.db, a.run, &[c], None).unwrap();

        assert_eq!(report.done.frames, 0, "кандидат уехал ({fate:?})");
        assert_eq!(fs::read(&copy).unwrap(), checked, "{fate:?}");
        let (_, why) = &report.refused[0];
        assert!(
            why.contains(&keeper.display().to_string()),
            "{fate:?}: {why}"
        );
        let (status, note) = last_row(&a.db);
        assert_eq!(status, JournalStatus::Failed.as_str(), "{fate:?}: {note}");
        assert!(
            note.contains(&keeper.display().to_string()),
            "{fate:?}: {note}"
        );
        assert_eq!(file_state(&a.db, &copy), "present", "{fate:?}");
    }
}

#[test]
fn parent_replacement_cannot_redirect_a_validated_move() {
    let a = archive();
    let keeper = a.dir.join("keeper.bmp");
    let folder = a.dir.join("event");
    let copy = folder.join("copy.bmp");
    bmp(&keeper, [10, 120, 200]);
    fs::create_dir_all(&folder).unwrap();
    fs::copy(&keeper, &copy).unwrap();
    let checked = fs::read(&copy).unwrap();
    let c = automatic(&a.db, a.run, &copy, &keeper);

    // The folder is moved away and another one, laid out the same way, takes
    // its name: a path looked up again now leads somewhere else.
    let (at, dir) = (copy.clone(), folder.clone());
    let aside = a.dir.join("event-aside");
    let to = aside.clone();
    let _race = race::before_move(move |src, _| {
        if src == at && !to.exists() {
            fs::rename(&dir, &to).unwrap();
            fs::create_dir_all(dir.join(pc_core::QUARANTINE_DIR)).unwrap();
            fs::write(&at, STRANGER).unwrap();
        }
        Ok(())
    });
    let report = crate::apply(&a.db, a.run, &[c], None).unwrap();

    assert_eq!(report.done.frames, 0, "перенос ушёл в чужую папку");
    assert_eq!(fs::read(&copy).unwrap(), STRANGER, "чужой файл тронут");
    assert!(
        !folder
            .join(pc_core::QUARANTINE_DIR)
            .join("copy.bmp")
            .exists(),
        "чужой файл в карантине: {:?}",
        tree(&a.dir)
    );
    assert_eq!(
        fs::read(aside.join("copy.bmp")).unwrap(),
        checked,
        "проверенный файл тронут: {:?}",
        tree(&a.dir)
    );
    let (status, note) = last_row(&a.db);
    assert_eq!(status, JournalStatus::Failed.as_str(), "{note}");
    assert!(note.contains(&copy.display().to_string()), "{note}");
    assert_eq!(file_state(&a.db, &copy), "present");
}

/// Swap the source for a stranger at the one moment no comparison covers:
/// after the last one, right before `renameat`.
fn swap_at_the_syscall(at: PathBuf, aside: PathBuf) -> race::SyscallGuard {
    race::at_rename(move |moment, src, _| {
        if moment == race::Syscall::Before && src == at && !aside.exists() {
            substitute(&at, &aside);
        }
        Ok(())
    })
}

#[test]
fn a_stranger_moved_by_the_rename_itself_is_put_back_and_the_move_refused() {
    let a = archive();
    let keeper = a.dir.join("keeper.bmp");
    let copy = a.dir.join("copy.bmp");
    let aside = a.dir.join("checked-aside.bmp");
    bmp(&keeper, [10, 120, 200]);
    fs::copy(&keeper, &copy).unwrap();
    let checked = fs::read(&copy).unwrap();
    let c = automatic(&a.db, a.run, &copy, &keeper);

    let _race = swap_at_the_syscall(copy.clone(), aside.clone());
    let report = crate::apply(&a.db, a.run, &[c], None).unwrap();

    assert_eq!(report.done.frames, 0);
    assert_eq!(fs::read(&copy).unwrap(), STRANGER, "чужой файл не вернулся");
    assert_eq!(fs::read(&aside).unwrap(), checked);
    let quarantine = a.dir.join(pc_core::QUARANTINE_DIR);
    assert!(!quarantine.join("copy.bmp").exists(), "{:?}", tree(&a.dir));
    let (_, why) = &report.refused[0];
    assert!(why.contains(&copy.display().to_string()), "{why}");
    let (status, note) = last_row(&a.db);
    assert_eq!(status, JournalStatus::Failed.as_str(), "{note}");
    assert!(note.contains(&copy.display().to_string()), "{note}");
    assert_eq!(file_state(&a.db, &copy), "present");
}

#[test]
fn a_stranger_that_cannot_be_put_back_is_left_and_named_not_removed() {
    let a = archive();
    let copy = a.dir.join("copy.bmp");
    let aside = a.dir.join("checked-aside.bmp");
    let third = b"a third file, written after the rename";
    bmp(&copy, [10, 120, 200]);
    let checked = fs::read(&copy).unwrap();
    let c = manual(&a.db, a.run, &copy);

    let (at, to) = (copy.clone(), aside.clone());
    let _race = race::at_rename(move |moment, src, _| {
        if src == at {
            match moment {
                race::Syscall::Before => substitute(&at, &to),
                // The name the stranger came from is taken at once.
                race::Syscall::After => fs::write(&at, third).unwrap(),
                _ => {}
            }
        }
        Ok(())
    });
    // A stranger the tool moved and cannot put back is in its keeping: the
    // run stops there (user decision (c), point 2), and says where it is.
    let e = crate::apply(&a.db, a.run, &[c], None).unwrap_err();

    let dst = a.dir.join(pc_core::QUARANTINE_DIR).join("copy.bmp");
    let stop = crate::stopped_run(&e).expect("typed stop");
    assert_eq!(stop.done.frames, 0);
    assert_eq!(fs::read(&aside).unwrap(), checked);
    assert_eq!(fs::read(&copy).unwrap(), third, "третий файл заменён");
    assert_eq!(fs::read(&dst).unwrap(), STRANGER, "чужой файл не сохранён");
    let why = format!("{e:#}");
    assert!(why.contains(&dst.display().to_string()), "{why}");
    assert!(stop
        .placed
        .iter()
        .any(|p| p.role == crate::Role::Stranger && p.held && p.at.is_verified_at(&dst)));
    let (status, note) = last_row(&a.db);
    assert_eq!(status, JournalStatus::Failed.as_str(), "{note}");
    assert!(note.contains(&dst.display().to_string()), "{note}");
}

#[test]
fn a_sidecar_replaced_before_its_move_keeps_its_frame_home_and_is_named() {
    let a = archive();
    let photo = a.dir.join("frame.bmp");
    let side = a.dir.join("frame.xmp");
    let aside = a.dir.join("edits-aside.xmp");
    bmp(&photo, [10, 120, 200]);
    fs::write(&side, b"my edits").unwrap();
    let frame = fs::read(&photo).unwrap();
    let c = manual(&a.db, a.run, &photo);

    let (at, to) = (side.clone(), aside.clone());
    let _race = race::before_move(move |src, _| {
        if src == at && !to.exists() {
            substitute(&at, &to);
        }
        Ok(())
    });
    let report = crate::apply(&a.db, a.run, &[c], None).unwrap();

    // The sidecar is not the one checked: the frame does not go without it
    // (user decision (c)). It was put back home; nothing stays moved.
    let quarantine = a.dir.join(pc_core::QUARANTINE_DIR);
    assert_eq!(report.done.frames, 0);
    assert_eq!(report.done.companions, 0);
    assert_eq!(fs::read(&photo).unwrap(), frame);
    assert!(!quarantine.join("frame.bmp").exists(), "{:?}", tree(&a.dir));
    assert!(!quarantine.join("frame.xmp").exists(), "{:?}", tree(&a.dir));
    assert_eq!(fs::read(&side).unwrap(), STRANGER);
    assert_eq!(fs::read(&aside).unwrap(), b"my edits");
    let (_, why) = &report.refused[0];
    assert!(why.contains(&side.display().to_string()), "{why}");
    assert!(a.db.journal_quarantined(None).unwrap().is_empty());
    assert_eq!(last_row(&a.db).0, JournalStatus::Failed.as_str());
    assert_eq!(file_state(&a.db, &photo), "present");
}

#[test]
fn an_undo_does_not_bring_home_a_stranger_put_in_quarantine() {
    let a = archive();
    let photo = a.dir.join("photo.bmp");
    bmp(&photo, [10, 120, 200]);
    let checked = fs::read(&photo).unwrap();
    let c = manual(&a.db, a.run, &photo);
    assert_eq!(
        crate::apply(&a.db, a.run, &[c], None).unwrap().done.frames,
        1
    );
    let held = a.dir.join(pc_core::QUARANTINE_DIR).join("photo.bmp");
    let aside = a.dir.join("held-aside.bmp");
    let entry = a.db.journal_quarantined(None).unwrap().pop().unwrap();

    let _race = swap_at_the_syscall(held.clone(), aside.clone());
    assert!(crate::undo(&a.db, entry.id).is_err(), "откат принёс чужое");

    assert!(!photo.exists(), "чужой файл дома: {:?}", tree(&a.dir));
    assert_eq!(fs::read(&held).unwrap(), STRANGER, "чужой файл не вернулся");
    assert_eq!(fs::read(&aside).unwrap(), checked);
}

#[test]
fn organize_moves_only_the_file_it_planned() {
    let a = archive();
    let src = a.dir.join("a.jpg");
    let dst = a.dir.join("2019/a.jpg");
    let aside = a.dir.join("a-aside.jpg");
    fs::write(&src, b"picture").unwrap();
    let mtime = pc_core::time::mtime_unix(&fs::metadata(&src).unwrap());
    let m = pc_organize::Move {
        file_id: indexed(&a.db, a.run, &src),
        src: src.display().to_string(),
        dst: dst.display().to_string(),
        size: 7,
        mtime,
        date: pc_organize::Dated {
            ts: 1_562_000_000,
            source: pc_organize::Source::Exif,
            precision: pc_organize::Precision::Day,
        },
        event: "2019".into(),
        renamed_from: None,
    };

    let (at, to) = (src.clone(), aside.clone());
    let _race = race::before_move(move |s, _| {
        if s == at && !to.exists() {
            substitute(&at, &to);
        }
        Ok(())
    });
    let report = crate::organize(&a.db, a.run, &[m]).unwrap();

    assert_eq!(report.done.frames, 0);
    assert!(!dst.exists(), "{:?}", tree(&a.dir));
    assert_eq!(fs::read(&src).unwrap(), STRANGER);
    assert_eq!(fs::read(&aside).unwrap(), b"picture");
    let (status, note) = last_row(&a.db);
    assert_eq!(status, JournalStatus::Failed.as_str(), "{note}");
    assert!(note.contains(&src.display().to_string()), "{note}");
}

/// Reproducers of the independent review el-4z6z9 (B1, B2), copied from its
/// evidence and finished with the contract the review asked for instead of
/// the behaviour it observed. The last two passed at e9a06f5 already and are
/// kept as regressions.
mod independent_d8 {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    pub(super) type Signature = (u64, u64, u32, u32, u32, i64, i64, Vec<u8>);

    pub(super) fn signature(p: &Path) -> Signature {
        let m = fs::symlink_metadata(p).unwrap();
        (
            m.dev(),
            m.ino(),
            m.mode(),
            m.uid(),
            m.gid(),
            m.mtime(),
            m.mtime_nsec(),
            fs::read(p).unwrap(),
        )
    }

    #[test]
    fn destination_ancestor_swap_cannot_strand_a_photo_outside_journal_paths() {
        let a = archive();
        let photo = a.dir.join("photo.bmp");
        bmp(&photo, [31, 71, 151]);
        let expected = signature(&photo);
        let c = manual(&a.db, a.run, &photo);
        let q = a.dir.join(pc_core::QUARANTINE_DIR);
        let parked = a.dir.join("intended-quarantine-parked");
        let elsewhere = a._tmp.path().join("unrelated-sync-folder");
        fs::create_dir(&elsewhere).unwrap();
        let (source, root, old, foreign) =
            (photo.clone(), q.clone(), parked.clone(), elsewhere.clone());
        let mut staged = false;
        let _race = race::before_move(move |src, _| {
            if src == source && !staged {
                staged = true;
                fs::rename(&root, &old).unwrap();
                std::os::unix::fs::symlink(&foreign, &root).unwrap();
            }
            Ok(())
        });
        let report = crate::apply(&a.db, a.run, &[c], None).unwrap();
        drop(_race);
        // A transient substituted namespace is restored after the operation.
        // Preserve the injected link too; do not unlink it.
        fs::rename(&q, a.dir.join("retained-injected-link")).unwrap();
        fs::rename(&parked, &q).unwrap();

        assert_eq!(
            report.done.frames, 0,
            "must not claim success for a move redirected outside the recorded quarantine"
        );
        assert_eq!(report.refused.len(), 1, "{report:?}");
        assert!(
            fs::read_dir(&elsewhere).unwrap().next().is_none(),
            "photo stranded outside the journal's paths: {:?}",
            tree(&elsewhere)
        );
        assert_eq!(signature(&photo), expected, "photo is not home, intact");
        assert!(a.db.journal_quarantined(None).unwrap().is_empty());
        let (status, note) = last_row(&a.db);
        assert_eq!(status, JournalStatus::Failed.as_str(), "{note}");
        assert_eq!(file_state(&a.db, &photo), "present");
    }

    #[test]
    fn postcheck_rejects_changed_bytes_even_when_the_inode_is_the_same() {
        let a = archive();
        let photo = a.dir.join("photo.bmp");
        let keeper = a.dir.join("keeper.bmp");
        bmp(&photo, [31, 71, 151]);
        fs::copy(&photo, &keeper).unwrap();
        let c = automatic(&a.db, a.run, &photo, &keeper);
        let original_inode = fs::metadata(&photo).unwrap().ino();
        let p = photo.clone();
        let _race = race::at_rename(move |moment, src, _| {
            if moment == race::Syscall::Before && src == p {
                // Normal writer, no forged timestamps, no inode reuse.
                fs::write(&p, STRANGER).unwrap();
                assert_eq!(fs::metadata(&p).unwrap().ino(), original_inode);
            }
            Ok(())
        });
        let report = crate::apply(&a.db, a.run, &[c], None).unwrap();
        drop(_race);

        assert_eq!(
            report.done.frames, 0,
            "post-check must compare full proof, not only dev:ino"
        );
        assert!(a.db.journal_quarantined(None).unwrap().is_empty());
        let (status, note) = last_row(&a.db);
        assert_eq!(status, JournalStatus::Failed.as_str(), "{note}");
        // The changed object is the same inode: it goes back to its name, as
        // it is now, and is never taken for the checked one.
        assert_eq!(fs::read(&photo).unwrap(), STRANGER);
        assert_eq!(fs::metadata(&photo).unwrap().ino(), original_inode);
        let q = a.dir.join(pc_core::QUARANTINE_DIR).join("photo.bmp");
        assert!(fs::symlink_metadata(&q).is_err(), "{:?}", tree(&a.dir));
        let (_, why) = &report.refused[0];
        assert!(why.contains(&photo.display().to_string()), "{why}");
        assert_eq!(file_state(&a.db, &photo), "present");
    }

    #[test]
    fn crash_after_rename_leaves_foreign_payload_unowned_and_reconcile_refuses() {
        let a = archive();
        let photo = a.dir.join("photo.bmp");
        let aside = a.dir.join("checked-aside.bmp");
        bmp(&photo, [31, 71, 151]);
        let checked = signature(&photo);
        let c = manual(&a.db, a.run, &photo);
        let (p, saved) = (photo.clone(), aside.clone());
        let _race = race::at_rename(move |moment, src, _| {
            if src == p {
                match moment {
                    race::Syscall::Before => {
                        substitute(&p, &saved);
                        fs::set_permissions(&p, fs::Permissions::from_mode(0o640)).unwrap();
                    }
                    race::Syscall::After => panic!("simulated interruption before postcheck"),
                    _ => {}
                }
            }
            Ok(())
        });
        let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::apply(&a.db, a.run, &[c], None)
        }));
        assert!(interrupted.is_err());
        drop(_race);
        let id: i64 =
            a.db.conn
                .query_row("SELECT max(id) FROM journal", [], |r| r.get(0))
                .unwrap();
        let before = a.db.journal_entry(id).unwrap().unwrap();
        assert_eq!(before.status, JournalStatus::Pending);
        let held = Path::new(before.dst.as_ref().unwrap());
        let foreign = signature(held);
        assert_eq!(foreign.7, STRANGER);
        assert!(crate::reconcile_undo(&a.db, id).is_err());
        assert_eq!(signature(held), foreign);
        assert_eq!(signature(&aside), checked);
        assert!(!photo.exists());
        assert_eq!(
            a.db.journal_entry(id).unwrap().unwrap().status,
            JournalStatus::Pending
        );
    }

    #[test]
    fn retained_stranger_is_never_claimed_by_undo_and_preserves_metadata() {
        let a = archive();
        let photo = a.dir.join("photo.bmp");
        let aside = a.dir.join("checked-aside.bmp");
        let foreign_seed = a.dir.join("foreign-seed");
        let blocker_seed = a.dir.join("blocker-seed");
        bmp(&photo, [31, 71, 151]);
        fs::write(&foreign_seed, STRANGER).unwrap();
        fs::set_permissions(&foreign_seed, fs::Permissions::from_mode(0o640)).unwrap();
        fs::write(&blocker_seed, b"foreign home occupant").unwrap();
        let foreign = signature(&foreign_seed);
        let blocker = signature(&blocker_seed);
        let c = manual(&a.db, a.run, &photo);
        let (p, saved, seed, block) = (
            photo.clone(),
            aside.clone(),
            foreign_seed.clone(),
            blocker_seed.clone(),
        );
        let _race = race::at_rename(move |moment, src, _| {
            if src == p {
                match moment {
                    race::Syscall::Before => {
                        fs::rename(&p, &saved).unwrap();
                        fs::rename(&seed, &p).unwrap();
                    }
                    race::Syscall::After => fs::rename(&block, &p).unwrap(),
                    _ => {}
                }
            }
            Ok(())
        });
        // The stranger stays in the tool's keeping: the run stops there.
        let e = crate::apply(&a.db, a.run, &[c], None).unwrap_err();
        drop(_race);
        let held = a.dir.join(pc_core::QUARANTINE_DIR).join("photo.bmp");
        assert_eq!(crate::stopped_run(&e).unwrap().done.frames, 0);
        assert_eq!(signature(&held), foreign);
        assert_eq!(signature(&photo), blocker);
        let reason = &format!("{e:#}");
        assert!(
            reason.contains(&photo.display().to_string())
                && reason.contains(&held.display().to_string())
        );
        let id: i64 =
            a.db.conn
                .query_row("SELECT max(id) FROM journal", [], |r| r.get(0))
                .unwrap();
        let entry = a.db.journal_entry(id).unwrap().unwrap();
        assert_eq!(entry.status, JournalStatus::Failed);
        assert!(crate::recovery::undo_preview(&entry)
            .unwrap()
            .iter()
            .all(|i| matches!(i.standing, crate::recovery::Standing::Doubt(_))));
        assert!(crate::undo(&a.db, id).is_err());
        assert_eq!(signature(&held), foreign);
        assert_eq!(signature(&photo), blocker);
    }
}

/// B1 beyond the reproducer: the destination is a namespace held from its
/// admission to the move, on every consumer of the shared move — the default
/// and the configured quarantine, undo and organize — and a namespace that
/// stopped being the one recorded is a refusal, with the photograph home or
/// its real place named.
mod destination_namespace {
    use super::independent_d8::signature;
    use super::*;

    /// Rename `dir` aside to `parked` and put a link to `elsewhere` at its
    /// name. Nothing is removed.
    fn swap_for_link(dir: &Path, parked: &Path, elsewhere: &Path) {
        fs::rename(dir, parked).unwrap();
        std::os::unix::fs::symlink(elsewhere, dir).unwrap();
    }

    fn empty(dir: &Path) -> bool {
        fs::read_dir(dir).unwrap().next().is_none()
    }

    #[test]
    fn a_link_in_place_of_the_quarantine_folder_is_refused() {
        let a = archive();
        let photo = a.dir.join("photo.bmp");
        bmp(&photo, [31, 71, 151]);
        let expected = signature(&photo);
        let elsewhere = a._tmp.path().join("unrelated-sync-folder");
        fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, a.dir.join(pc_core::QUARANTINE_DIR)).unwrap();
        let c = manual(&a.db, a.run, &photo);

        let report = crate::apply(&a.db, a.run, &[c], None).unwrap();

        assert_eq!(report.done.frames, 0, "{report:?}");
        assert!(empty(&elsewhere), "{:?}", tree(&elsewhere));
        assert_eq!(signature(&photo), expected);
        assert!(a.db.journal_quarantined(None).unwrap().is_empty());
    }

    #[test]
    fn a_quarantine_swapped_after_the_rename_sends_the_photo_home() {
        let a = archive();
        let photo = a.dir.join("photo.bmp");
        bmp(&photo, [31, 71, 151]);
        let expected = signature(&photo);
        let c = manual(&a.db, a.run, &photo);
        let q = a.dir.join(pc_core::QUARANTINE_DIR);
        let parked = a.dir.join("intended-quarantine-parked");
        let elsewhere = a._tmp.path().join("unrelated-sync-folder");
        fs::create_dir(&elsewhere).unwrap();
        let (p, root, old, foreign) = (photo.clone(), q.clone(), parked.clone(), elsewhere.clone());
        let _race = race::at_rename(move |moment, src, _| {
            if moment == race::Syscall::After && src == p {
                swap_for_link(&root, &old, &foreign);
            }
            Ok(())
        });
        let report = crate::apply(&a.db, a.run, &[c], None).unwrap();
        drop(_race);

        assert_eq!(report.done.frames, 0, "{report:?}");
        assert_eq!(signature(&photo), expected, "photo is not home, intact");
        assert!(
            empty(&elsewhere) && empty(&parked),
            "{:?}",
            tree(a._tmp.path())
        );
        assert!(a.db.journal_quarantined(None).unwrap().is_empty());
        let (status, note) = last_row(&a.db);
        assert_eq!(status, JournalStatus::Failed.as_str(), "{note}");
        // The injected link is left as it was.
        assert!(fs::symlink_metadata(&q).unwrap().file_type().is_symlink());
    }

    #[test]
    fn a_photo_that_cannot_go_home_is_kept_and_its_real_place_named() {
        let a = archive();
        let photo = a.dir.join("photo.bmp");
        bmp(&photo, [31, 71, 151]);
        let expected = signature(&photo);
        let c = manual(&a.db, a.run, &photo);
        let q = a.dir.join(pc_core::QUARANTINE_DIR);
        let parked = a.dir.join("intended-quarantine-parked");
        let elsewhere = a._tmp.path().join("unrelated-sync-folder");
        fs::create_dir(&elsewhere).unwrap();
        let (p, root, old, foreign) = (photo.clone(), q.clone(), parked.clone(), elsewhere.clone());
        let _race = race::at_rename(move |moment, src, _| {
            if moment == race::Syscall::After && src == p {
                swap_for_link(&root, &old, &foreign);
                fs::write(&p, b"a new file at the old name").unwrap();
            }
            Ok(())
        });
        // The photo stays in the tool's keeping: the run stops, the row
        // stays open (user decision (c), point 2).
        let e = crate::apply(&a.db, a.run, &[c], None).unwrap_err();
        drop(_race);

        let stop = crate::stopped_run(&e).expect("typed stop");
        assert_eq!(stop.done.frames, 0, "{e:#}");
        assert_eq!(stop.pending.len(), 1);
        let kept = parked.join("photo.bmp");
        assert_eq!(signature(&kept), expected, "photo not kept intact");
        assert_eq!(fs::read(&photo).unwrap(), b"a new file at the old name");
        assert!(empty(&elsewhere));
        let why = &format!("{e:#}");
        let real = fs::canonicalize(&kept).unwrap();
        assert!(
            why.contains(&real.display().to_string()) || why.contains(&kept.display().to_string()),
            "the real place is not named: {why}"
        );
        let (status, note) = last_row(&a.db);
        assert_eq!(status, JournalStatus::Pending.as_str(), "{note}");
        assert!(
            note.contains(&real.display().to_string())
                || note.contains(&kept.display().to_string()),
            "{note}"
        );
    }

    #[test]
    fn a_configured_quarantine_swapped_before_the_move_is_refused() {
        let a = archive();
        let photo = a.dir.join("photo.bmp");
        bmp(&photo, [31, 71, 151]);
        let expected = signature(&photo);
        let c = manual(&a.db, a.run, &photo);
        let root = a._tmp.path().join("gathered");
        let elsewhere = a._tmp.path().join("unrelated-sync-folder");
        fs::create_dir(&elsewhere).unwrap();
        let (source, foreign) = (photo.clone(), elsewhere.clone());
        let mut parked = None;
        let _race = race::before_move(move |src, dst| {
            if src == source && parked.is_none() {
                let dir = dst.parent().unwrap();
                let old = dir.with_file_name("parked-by-another-program");
                swap_for_link(dir, &old, &foreign);
                parked = Some(old);
            }
            Ok(())
        });
        let report = crate::apply(&a.db, a.run, &[c], Some(&root)).unwrap();
        drop(_race);

        assert_eq!(report.done.frames, 0, "{report:?}");
        assert_eq!(signature(&photo), expected);
        assert!(empty(&elsewhere), "{:?}", tree(&elsewhere));
        assert!(a.db.journal_quarantined(None).unwrap().is_empty());
    }

    #[test]
    fn an_undo_whose_home_folder_is_swapped_brings_nothing_elsewhere() {
        let a = archive();
        let home = a.dir.join("sub");
        let photo = home.join("photo.bmp");
        bmp(&photo, [31, 71, 151]);
        let expected = fs::read(&photo).unwrap();
        let c = manual(&a.db, a.run, &photo);
        let root = a._tmp.path().join("gathered");
        let report = crate::apply(&a.db, a.run, &[c], Some(&root)).unwrap();
        assert_eq!(report.done.frames, 1, "{report:?}");
        let entry = a.db.journal_quarantined(None).unwrap().pop().unwrap();
        let held = PathBuf::from(entry.dst.clone().unwrap());

        let elsewhere = a._tmp.path().join("unrelated-sync-folder");
        fs::create_dir(&elsewhere).unwrap();
        let parked = a.dir.join("sub-parked");
        let (dir, old, foreign) = (home.clone(), parked.clone(), elsewhere.clone());
        let mut staged = false;
        let _race = race::before_move(move |_, dst| {
            if dst.starts_with(&dir) && !staged {
                staged = true;
                swap_for_link(&dir, &old, &foreign);
            }
            Ok(())
        });
        let undone = crate::undo(&a.db, entry.id);
        drop(_race);

        assert!(undone.is_err(), "{undone:?}");
        assert!(empty(&elsewhere), "{:?}", tree(&elsewhere));
        assert_eq!(fs::read(&held).unwrap(), expected, "photo left quarantine");
        // Namespace restored: the same entry still comes back by its record.
        fs::rename(&home, a.dir.join("retained-injected-link")).unwrap();
        fs::rename(&parked, &home).unwrap();
        crate::undo(&a.db, entry.id).unwrap();
        assert_eq!(fs::read(&photo).unwrap(), expected);
    }

    #[test]
    fn organize_into_a_swapped_folder_moves_nothing() {
        let a = archive();
        let src = a.dir.join("a.jpg");
        let dst = a.dir.join("2019/a.jpg");
        fs::write(&src, b"picture").unwrap();
        let mtime = pc_core::time::mtime_unix(&fs::metadata(&src).unwrap());
        let m = pc_organize::Move {
            file_id: indexed(&a.db, a.run, &src),
            src: src.display().to_string(),
            dst: dst.display().to_string(),
            size: 7,
            mtime,
            date: pc_organize::Dated {
                ts: 1_562_000_000,
                source: pc_organize::Source::Exif,
                precision: pc_organize::Precision::Day,
            },
            event: "2019".into(),
            renamed_from: None,
        };
        let elsewhere = a._tmp.path().join("unrelated-sync-folder");
        fs::create_dir(&elsewhere).unwrap();
        let (at, foreign) = (src.clone(), elsewhere.clone());
        let mut staged = false;
        let _race = race::before_move(move |s, d| {
            if s == at && !staged {
                staged = true;
                let dir = d.parent().unwrap();
                swap_for_link(dir, &dir.with_file_name("2019-parked"), &foreign);
            }
            Ok(())
        });
        let report = crate::organize(&a.db, a.run, &[m]).unwrap();
        drop(_race);

        assert_eq!(report.done.frames, 0);
        assert!(empty(&elsewhere), "{:?}", tree(&elsewhere));
        assert_eq!(fs::read(&src).unwrap(), b"picture");
    }
}

/// B2 on the shared move itself: the object that arrives is compared with
/// the whole evidence, not only its device and inode.
mod arrival_proof {
    use super::*;

    fn rewrite_at_the_syscall(at: PathBuf, bytes: Vec<u8>) -> race::SyscallGuard {
        race::at_rename(move |moment, src, _| {
            if moment == race::Syscall::Before && src == at {
                // An ordinary writer, later than the evidence: no forged
                // time. The pause outlasts a coarse file-system clock.
                std::thread::sleep(std::time::Duration::from_millis(20));
                fs::write(&at, &bytes).unwrap();
            }
            Ok(())
        })
    }

    fn staged(bytes: &[u8]) -> (tempfile::TempDir, PathBuf, PathBuf, pc_core::proof::Proof) {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("frame.raw");
        let dst = tmp.path().join("q").join("frame.raw");
        fs::write(&src, bytes).unwrap();
        let proof = pc_core::proof::Proof::of(&fs::symlink_metadata(&src).unwrap()).unwrap();
        (tmp, src, dst, proof)
    }

    fn refused_and_home(src: &Path, dst: &Path, now: &[u8], r: anyhow::Result<()>) {
        let e = r.expect_err("a changed arrival was accepted");
        assert!(e.to_string().contains(&src.display().to_string()), "{e}");
        assert_eq!(fs::read(src).unwrap(), now, "the object is not back");
        assert!(
            fs::symlink_metadata(dst).is_err(),
            "kept at the destination"
        );
    }

    #[test]
    fn a_rewrite_that_changes_the_size_is_refused_after_the_rename() {
        let (_tmp, src, dst, proof) = staged(b"checked frame");
        let _race = rewrite_at_the_syscall(src.clone(), b"longer, unrelated bytes".to_vec());
        let r = crate::rename_with_parents(&src, &dst, Some(&proof)).map(|_| ());
        refused_and_home(&src, &dst, b"longer, unrelated bytes", r);
    }

    #[test]
    fn a_same_size_rewrite_with_its_ordinary_mtime_is_refused_after_the_rename() {
        let (_tmp, src, dst, proof) = staged(b"checked frame");
        let _race = rewrite_at_the_syscall(src.clone(), b"CHECKED FRAME".to_vec());
        let r = crate::rename_with_parents(&src, &dst, Some(&proof)).map(|_| ());
        refused_and_home(&src, &dst, b"CHECKED FRAME", r);
    }

    #[test]
    fn an_unchanged_object_still_moves() {
        let (_tmp, src, dst, proof) = staged(b"checked frame");
        crate::rename_with_parents(&src, &dst, Some(&proof)).unwrap();
        assert_eq!(fs::read(&dst).unwrap(), b"checked frame");
        assert!(fs::symlink_metadata(&src).is_err());
    }
}

/// The reviewer's reproducers of el-57qpk (B1-R2 and its neighbours),
/// kept exactly as handed over in /tmp/el-3wizg-review-4d93aa1/adjacent.rs.
mod reviewer_r2 {
    use super::independent_d8::signature;
    use super::*;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn returned_photo_in_relocated_source_namespace_has_its_actual_path_reported() {
        let a = archive();
        let photo = a.dir.join("photo.bmp");
        bmp(&photo, [31, 71, 151]);
        let expected = signature(&photo);
        let c = manual(&a.db, a.run, &photo);
        let parked = a._tmp.path().join("archive-moved-by-sync");
        let (p, root, to) = (photo.clone(), a.dir.clone(), parked.clone());
        let _race = race::at_rename(move |moment, src, _| {
            if moment == race::Syscall::After && src == p {
                fs::rename(&root, &to).unwrap();
                fs::create_dir(&root).unwrap();
            }
            Ok(())
        });
        let report = crate::apply(&a.db, a.run, &[c], None).unwrap();
        drop(_race);
        let actual = parked.join("photo.bmp");
        assert_eq!(signature(&actual), expected);
        assert!(!photo.exists());
        assert_eq!(report.done.frames, 0);
        assert!(a.db.journal_quarantined(None).unwrap().is_empty());
        let (status, note) = last_row(&a.db);
        assert_eq!(status, JournalStatus::Failed.as_str());
        let id: i64 =
            a.db.conn
                .query_row("SELECT max(id) FROM journal", [], |r| r.get(0))
                .unwrap();
        let entry = a.db.journal_entry(id).unwrap().unwrap();
        let preview = crate::recovery::undo_preview(&entry).unwrap();
        assert!(preview
            .iter()
            .all(|item| matches!(item.standing, crate::recovery::Standing::Gone)));
        assert!(crate::undo(&a.db, id).is_err());
        assert_eq!(signature(&actual), expected);
        let why = &report.refused[0].1;
        eprintln!(
            "actual={}; preview={preview:?}; refusal={why}; journal note={note}",
            actual.display()
        );
        let canonical = fs::canonicalize(&actual).unwrap().display().to_string();
        assert!(why.contains(&actual.display().to_string()) || why.contains(&canonical),
            "returned photo is outside both recorded paths, but its actual location is absent: {why}");
        assert!(note.contains(&actual.display().to_string()) || note.contains(&canonical));
    }

    #[test]
    fn rewritten_arrival_with_occupied_home_retains_payload_and_reports_evidence() {
        let a = archive();
        let photo = a.dir.join("photo.bmp");
        bmp(&photo, [31, 71, 151]);
        let c = manual(&a.db, a.run, &photo);
        let blocker = a.dir.join("blocker");
        fs::write(&blocker, b"foreign home occupant").unwrap();
        let expected_blocker = signature(&blocker);
        let observed = std::sync::Arc::new(std::sync::Mutex::new(None));
        let (p, block, seen) = (photo.clone(), blocker.clone(), observed.clone());
        let _race = race::at_rename(move |moment, src, dst| {
            if moment == race::Syscall::After && src == p {
                fs::write(dst, STRANGER).unwrap();
                *seen.lock().unwrap() = Some(signature(dst));
                fs::rename(&block, &p).unwrap();
            }
            Ok(())
        });
        // A changed photo the tool keeps: the run stops, the row stays open,
        // and no recovery takes it back automatically (user decision (c)).
        let e = crate::apply(&a.db, a.run, &[c], None).unwrap_err();
        drop(_race);
        let actual = a.dir.join(pc_core::QUARANTINE_DIR).join("photo.bmp");
        assert_eq!(
            signature(&actual),
            observed.lock().unwrap().clone().unwrap()
        );
        assert_eq!(signature(&photo), expected_blocker);
        assert_eq!(crate::stopped_run(&e).unwrap().done.frames, 0);
        let why = &format!("{e:#}");
        assert!(why.contains(&actual.display().to_string()), "{why}");
        let (status, note) = last_row(&a.db);
        assert_eq!(status, JournalStatus::Pending.as_str());
        assert!(note.contains(&actual.display().to_string()));
        let id: i64 =
            a.db.conn
                .query_row("SELECT max(id) FROM journal", [], |r| r.get(0))
                .unwrap();
        assert!(crate::undo(&a.db, id).is_err());
        assert!(crate::reconcile_undo(&a.db, id).is_err());
        assert_eq!(
            signature(&actual),
            observed.lock().unwrap().clone().unwrap()
        );
        assert_eq!(signature(&photo), expected_blocker);
    }

    #[test]
    fn journaled_hash_rejects_same_size_backdated_rewrite_after_rename() {
        let a = archive();
        let src = a.dir.join("source");
        let dst = a.dir.join("destination");
        fs::write(&src, b"checked").unwrap();
        let file = fs::File::open(&src).unwrap();
        let md = file.metadata().unwrap();
        let mut proof = pc_core::proof::Proof::of(&md).unwrap();
        proof.blake3 =
            Some("8630b44153aa2d745f2d95e3d23982f39f2ee299fe3f1c081412d400c753acc8".to_string());
        let at = src.clone();
        let _race = race::at_rename(move |moment, s, d| {
            if moment == race::Syscall::After && s == at {
                fs::write(d, b"foreign").unwrap();
                file.set_times(std::fs::FileTimes::new().set_modified(md.modified().unwrap()))
                    .unwrap();
            }
            Ok(())
        });
        let result = crate::rename_with_parents(&src, &dst, Some(&proof));
        assert!(result.is_err(), "{result:?}");
        assert_eq!(fs::read(&src).unwrap(), b"foreign");
        assert_eq!(fs::metadata(&src).unwrap().ino(), proof.ino);
        assert!(!dst.exists());
    }
}

/// The namespace class, cell by cell (el-lvtmk §5).
mod class_d8;

mod unit_rule;
