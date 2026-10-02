//! What a refusal leaves behind (el-usdqi, independent review el-23goa).
//!
//! The race tests prove that a move never replaces what appeared at its
//! destination. These prove the rest of the contract around a refusal: the
//! check before a run writes nothing, a refusal that concerns the volume
//! stops the run wherever it happens — a sidecar, a litter sweep, an undo —
//! the caller is told what had already moved and how to walk it back, and
//! a half-finished undo or recovery can be asked again and finishes.

use super::*;
use std::cell::RefCell;
use std::rc::Rc;

/// Everything about a directory entry that a stranger would care about.
#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
struct Signature(u64, u64, u32, u32, u32, i64, i64, Vec<u8>);

#[cfg(unix)]
fn signature(p: &Path) -> Signature {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    let m = fs::symlink_metadata(p).unwrap();
    let payload = if m.file_type().is_symlink() {
        fs::read_link(p).unwrap().as_os_str().as_bytes().to_vec()
    } else {
        fs::read(p).unwrap()
    };
    Signature(
        m.dev(),
        m.ino(),
        m.mode(),
        m.uid(),
        m.gid(),
        m.mtime(),
        m.mtime_nsec(),
        payload,
    )
}

/// Every path under `dir`, with the bytes of every file.
fn tree(dir: &Path) -> std::collections::BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut all = std::collections::BTreeMap::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for entry in fs::read_dir(&d).unwrap() {
            let path = entry.unwrap().path();
            let md = fs::symlink_metadata(&path).unwrap();
            if md.is_dir() {
                todo.push(path.clone());
                all.insert(path, None);
            } else if md.file_type().is_symlink() {
                let to = fs::read_link(&path).unwrap();
                all.insert(path, Some(to.display().to_string().into_bytes()));
            } else {
                all.insert(path.clone(), Some(fs::read(&path).unwrap()));
            }
        }
    }
    all
}

fn entry(
    a: &Archive,
    home: &Path,
    held: &Path,
    manifest: &[pc_db::Moved],
    status: JournalStatus,
) -> i64 {
    let id =
        a.db.journal_begin(&pc_db::NewJournalEntry {
            run_id: a.run,
            op: "quarantine-file",
            target_id: None,
            src: &home.display().to_string(),
            dst: Some(&held.display().to_string()),
            size: 7,
            file_count: 1,
            manifest,
        })
        .unwrap();
    a.db.journal_finish(id, status, None).unwrap();
    id
}

fn pair(src: &Path, dst: &Path) -> pc_db::Moved {
    pc_db::Moved {
        src: src.display().to_string(),
        dst: dst.display().to_string(),
        ident: None,
    }
}

/// Plant `what` at `at` once, just before something moves there, and keep
/// what it looked like right after.
#[cfg(unix)]
fn foreign_at(at: PathBuf, what: Stranger) -> (race::Guard, Rc<RefCell<Option<Signature>>>) {
    let saved = Rc::new(RefCell::new(None));
    let out = saved.clone();
    let guard = race::before_move(move |_, dst| {
        if dst == at && out.borrow().is_none() {
            plant(dst, what);
            *out.borrow_mut() = Some(signature(dst));
        }
        Ok(())
    });
    (guard, saved)
}

/// The volume refuses to move `src` the way exFAT refuses every move.
fn unsupported_for(src: PathBuf) -> race::Guard {
    race::before_move(move |from, _| {
        if from == src {
            Err(io::Error::from(io::ErrorKind::Unsupported))
        } else {
            Ok(())
        }
    })
}

/// Nothing a refusal could have said claims the whole run moved nothing.
fn denies_earlier_moves(shown: &str) -> bool {
    shown.contains("nothing was moved") || shown.contains("ничего не перенесено")
}

// ---------------------------------------------------------------- B1 -----

/// The candidate is refused for its missing keeper; the check of the run
/// before it must not have written anything on the way — least of all
/// through a symlink planted at the layout file's name.
#[cfg(unix)]
#[test]
fn a_refused_candidate_never_writes_through_a_layout_symlink() {
    let a = archive();
    let photo = a.dir.join("candidate.arw");
    fs::write(&photo, b"candidate frame").unwrap();
    let mut c = candidate(&a.db, a.run, &photo);
    c.manual = false;
    c.keeper_path = a.dir.join("missing-keeper.arw").display().to_string();
    let gathered = a.dir.join("gathered");
    let q = gathered.join(pc_core::QUARANTINE_DIR);
    fs::create_dir_all(&q).unwrap();
    let foreign = a.dir.join("unrelated-original.arw");
    fs::write(&foreign, b"irreplaceable foreign synthetic payload").unwrap();
    std::os::unix::fs::symlink(&foreign, q.join(pc_core::QUARANTINE_LAYOUT)).unwrap();
    let before = signature(&foreign);

    let result = crate::apply(&a.db, a.run, &[c], Some(&gathered)).unwrap();

    assert_eq!(result.totals.files, 0);
    assert_eq!(result.refused.len(), 1);
    assert_eq!(journal_rows(&a.db), 0);
    assert_eq!(signature(&foreign), before, "запись сквозь чужую ссылку");
}

/// Nothing in the archive changes when the check before the run refuses a
/// candidate — with the default quarantine beside the file, and with a
/// configured one that does not exist yet.
#[test]
fn the_check_before_a_run_writes_nothing() {
    for gathered in [false, true] {
        let a = archive();
        let photo = a.dir.join("candidate.arw");
        fs::write(&photo, b"candidate frame").unwrap();
        let mut c = candidate(&a.db, a.run, &photo);
        c.manual = false;
        c.keeper_path = a.dir.join("missing-keeper.arw").display().to_string();
        let root = a.dir.join("gathered");
        let before = tree(&a.dir);

        crate::check_candidates(&a.db, std::slice::from_ref(&c), gathered.then_some(&*root))
            .unwrap();
        let result = crate::apply(&a.db, a.run, &[c], gathered.then_some(&*root)).unwrap();

        assert_eq!(result.refused.len(), 1, "gathered={gathered}");
        assert_eq!(tree(&a.dir), before, "gathered={gathered}: архив изменился");
    }
}

/// Something that is not a layout this tool wrote sits at its name: a file
/// with other bytes, or one hard-linked to a file elsewhere. The move stops
/// before the first photograph goes there, and the stranger is untouched.
#[cfg(unix)]
#[test]
fn a_stranger_at_the_layout_name_stops_the_move_and_stays_intact() {
    for case in ["foreign bytes", "hard link", "symlink"] {
        let a = archive();
        let photo = a.dir.join("frame.arw");
        fs::write(&photo, b"raw frame").unwrap();
        let c = candidate(&a.db, a.run, &photo);
        let gathered = a.dir.join("gathered");
        let q = gathered.join(pc_core::QUARANTINE_DIR);
        fs::create_dir_all(&q).unwrap();
        let layout = q.join(pc_core::QUARANTINE_LAYOUT);
        let foreign = a.dir.join("elsewhere.json");
        match case {
            "foreign bytes" => fs::write(&layout, b"someone else's notes").unwrap(),
            "hard link" => {
                // Even a valid, empty layout is someone else's once it has a
                // second name: writing it changes that file too.
                fs::write(&foreign, b"{}").unwrap();
                fs::hard_link(&foreign, &layout).unwrap();
            }
            _ => {
                fs::write(&foreign, b"{}").unwrap();
                std::os::unix::fs::symlink(&foreign, &layout).unwrap();
            }
        }
        let stranger = signature(&layout);
        let elsewhere = fs::metadata(&foreign).is_ok().then(|| signature(&foreign));

        let result = crate::apply(&a.db, a.run, &[c], Some(&gathered));

        assert!(result.is_err(), "{case}: {result:?}");
        let shown = format!("{:#}", result.unwrap_err());
        assert!(
            shown.contains(&layout.display().to_string()),
            "{case}: {shown}"
        );
        assert_eq!(fs::read(&photo).unwrap(), b"raw frame", "{case}");
        assert_eq!(signature(&layout), stranger, "{case}: раскладка затёрта");
        if let Some(sig) = elsewhere {
            assert_eq!(signature(&foreign), sig, "{case}: чужой файл изменён");
        }
        assert!(a.db.journal_quarantined(None).unwrap().is_empty(), "{case}");
    }
}

/// The layout is published once, and a second disk's label joins it; what
/// was recorded first is never lost.
#[test]
fn the_layout_is_written_once_and_grows() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join(pc_core::QUARANTINE_DIR);
    pc_core::quarantine_layout::note(&root, "disk1", Path::new("/mnt/disk1")).unwrap();
    pc_core::quarantine_layout::note(&root, "disk1", Path::new("/mnt/disk1")).unwrap();
    pc_core::quarantine_layout::note(&root, "disk2", Path::new("/mnt/disk2")).unwrap();
    let disks = pc_core::quarantine_layout::read(&root);
    assert_eq!(disks.get("disk1").map(String::as_str), Some("/mnt/disk1"));
    assert_eq!(disks.get("disk2").map(String::as_str), Some("/mnt/disk2"));
    // Only the layout itself: no half-written temporary left beside it.
    let names: Vec<_> = fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(
        names,
        [std::ffi::OsString::from(pc_core::QUARANTINE_LAYOUT)]
    );
}

// ---------------------------------------------------------------- B2 -----

/// An entry from before the journal held a list: the frame comes home, its
/// sidecar is refused (a stranger turned up at home). Once the stranger is
/// moved aside, asking again finishes — it recognises the frame that is
/// already home instead of failing on its absence from quarantine.
#[cfg(unix)]
#[test]
fn a_legacy_undo_that_stopped_at_a_sidecar_can_be_asked_again() {
    let a = archive();
    let home = a.dir.join("legacy.arw");
    let held = a.quarantine.join("legacy.arw");
    fs::create_dir_all(&a.quarantine).unwrap();
    fs::write(&held, b"raw frame").unwrap();
    fs::write(a.quarantine.join("legacy.xmp"), b"original edits").unwrap();
    let id = entry(&a, &home, &held, &[], JournalStatus::Done);
    let side = a.dir.join("legacy.xmp");
    let (hook, foreign) = foreign_at(side.clone(), Stranger::File);

    let err = crate::undo(&a.db, id).unwrap_err();
    drop(hook);

    assert!(err.to_string().contains("legacy.xmp"), "{err:#}");
    assert_eq!(Some(signature(&side)), *foreign.borrow());
    assert_eq!(fs::read(&home).unwrap(), b"raw frame");
    let (status, first_note) = journal_row(&a.db, id);
    assert_eq!(status, "done");
    assert!(first_note.contains("legacy.xmp"), "{first_note}");

    fs::rename(&side, a.dir.join("saved-stranger.xmp")).unwrap();
    let retry = crate::undo(&a.db, id);

    assert!(retry.is_ok(), "{retry:?}; {:?}", journal_row(&a.db, id));
    assert_eq!(fs::read(&side).unwrap(), b"original edits");
    assert_eq!(fs::read(&home).unwrap(), b"raw frame");
    assert_eq!(
        fs::read(a.dir.join("saved-stranger.xmp")).unwrap(),
        STRANGER
    );
    let (status, note) = journal_row(&a.db, id);
    assert_eq!(status, "undone");
    assert!(
        note.contains(&first_note),
        "the earlier refusal is history: {note}"
    );
}

/// The frame that was brought home is not there any more — another file of
/// the same name is. The retry does not take that file for the frame: it
/// stops, the entry stays `done`, and the held sidecar stays held.
#[cfg(unix)]
#[test]
fn a_retried_undo_does_not_take_a_stranger_at_home_for_the_frame() {
    let a = archive();
    let home = a.dir.join("legacy.arw");
    let held = a.quarantine.join("legacy.arw");
    let held_side = a.quarantine.join("legacy.xmp");
    fs::create_dir_all(&a.quarantine).unwrap();
    fs::write(&held, b"raw frame").unwrap();
    fs::write(&held_side, b"original edits").unwrap();
    let id = entry(&a, &home, &held, &[], JournalStatus::Done);
    let side = a.dir.join("legacy.xmp");
    let (hook, _) = foreign_at(side.clone(), Stranger::File);
    crate::undo(&a.db, id).unwrap_err();
    drop(hook);

    // The frame is taken elsewhere and a stranger arrives at its name.
    fs::rename(&home, a.dir.join("moved-by-user.arw")).unwrap();
    fs::write(&home, STRANGER).unwrap();
    fs::remove_file(&side).unwrap();

    let err = crate::undo(&a.db, id).unwrap_err();

    assert!(
        err.to_string().contains(&home.display().to_string()),
        "{err:#}"
    );
    assert_eq!(fs::read(&home).unwrap(), STRANGER);
    assert_eq!(fs::read(&held_side).unwrap(), b"original edits");
    assert!(fs::symlink_metadata(&side).is_err());
    assert_eq!(journal_row(&a.db, id).0, "done");
}

/// An old row whose photograph an older version already brought home,
/// leaving its sidecar behind, and wrote nothing down. There is nothing to
/// tell that file from a stranger with its name: the undo does not guess,
/// the sidecar stays held, the entry stays `done`, and the reason is noted.
#[test]
fn an_old_half_undo_without_a_list_is_not_finished_by_guessing() {
    let a = archive();
    let home = a.dir.join("old.arw");
    let held = a.quarantine.join("old.arw");
    let held_side = a.quarantine.join("old.xmp");
    fs::create_dir_all(&a.quarantine).unwrap();
    fs::write(&home, b"some file named old.arw").unwrap();
    fs::write(&held_side, b"original edits").unwrap();
    let id = entry(&a, &home, &held, &[], JournalStatus::Done);

    let err = crate::undo(&a.db, id).unwrap_err();

    assert!(err.to_string().contains(&home.display().to_string()), "{err:#}");
    assert_eq!(fs::read(&held_side).unwrap(), b"original edits");
    assert!(fs::symlink_metadata(a.dir.join("old.xmp")).is_err());
    let (status, note) = journal_row(&a.db, id);
    assert_eq!(status, "done");
    assert!(note.contains(&home.display().to_string()), "{note}");
    // Asked again, the same answer: the row did not turn into one that a
    // later retry would take on trust.
    assert!(crate::undo(&a.db, id).is_err());
    assert!(a.db.journal_entry(id).unwrap().unwrap().manifest.is_empty());
}

// ---------------------------------------------------------------- B3 -----

/// A sidecar the volume refuses to move the way it would refuse every move:
/// the photograph that already moved stays journaled as moved, the sidecar
/// stays home and is named, and the next photograph is never touched.
#[test]
fn a_volume_refusal_at_a_sidecar_stops_apply_after_journaling_the_photo() {
    let a = archive();
    let (one, two) = (a.dir.join("one.arw"), a.dir.join("two.arw"));
    fs::write(&one, b"raw one").unwrap();
    fs::write(&two, b"raw two").unwrap();
    let side = a.dir.join("one.xmp");
    fs::write(&side, b"edits").unwrap();
    let cs = [candidate(&a.db, a.run, &one), candidate(&a.db, a.run, &two)];

    let _refused = unsupported_for(side.clone());
    let result = crate::apply(&a.db, a.run, &cs, None);

    let err = result.expect_err("a volume refusal must stop the run");
    assert!(crate::is_no_exclusive_rename(&err), "{err:#}");
    assert!(format!("{err:#}").contains("one.xmp"), "{err:#}");
    assert_eq!(fs::read(&side).unwrap(), b"edits");
    assert_eq!(
        fs::read(&two).unwrap(),
        b"raw two",
        "следующий снимок уехал"
    );
    let done = a.db.journal_quarantined(None).unwrap();
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].src, one.display().to_string());
    assert_eq!(done[0].manifest.len(), 1, "спутник записан как уехавший");
    let (_, note) = journal_row(&a.db, done[0].id);
    assert!(note.contains("one.xmp"), "{note}");
    assert_eq!(file_state(&a.db, &one), "quarantined");
}

/// The same at a sidecar in a reorganisation.
#[test]
fn a_volume_refusal_at_a_sidecar_stops_organize() {
    let a = archive();
    let (one, two) = (a.dir.join("one.jpg"), a.dir.join("two.jpg"));
    fs::write(&one, b"one").unwrap();
    fs::write(&two, b"two").unwrap();
    let side = a.dir.join("one.xmp");
    fs::write(&side, b"edits").unwrap();
    let moves = [
        organized(&a, &one, &a.dir.join("2019/one.jpg")),
        organized(&a, &two, &a.dir.join("2019/two.jpg")),
    ];

    let _refused = unsupported_for(side.clone());
    let err = crate::organize(&a.db, a.run, &moves).unwrap_err();

    assert!(crate::is_no_exclusive_rename(&err), "{err:#}");
    assert_eq!(fs::read(a.dir.join("2019/one.jpg")).unwrap(), b"one");
    assert_eq!(fs::read(&side).unwrap(), b"edits");
    assert_eq!(fs::read(&two).unwrap(), b"two", "следующий файл уехал");
    let done = a.db.journal_by_run_op(a.run, "organize").unwrap();
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].manifest.len(), 1);
    let (_, note) = journal_row(&a.db, done[0].id);
    assert!(note.contains("one.xmp"), "{note}");
    assert!(!denies_earlier_moves(&err.to_string()), "{err}");
}

/// The litter sweep after a reorganisation: the first service file refused
/// by the volume stops the sweep, so the second emptied folder is left as
/// it was, and organize reports the stop.
#[test]
fn a_volume_refusal_in_the_litter_sweep_stops_it() {
    let a = archive();
    let (d1, d2) = (a.dir.join("d1"), a.dir.join("d2"));
    fs::create_dir_all(&d1).unwrap();
    fs::create_dir_all(&d2).unwrap();
    let (one, two) = (d1.join("one.jpg"), d2.join("two.jpg"));
    fs::write(&one, b"one").unwrap();
    fs::write(&two, b"two").unwrap();
    fs::write(d1.join(".DS_Store"), b"finder one").unwrap();
    fs::write(d2.join(".DS_Store"), b"finder two").unwrap();
    let moves = [
        organized(&a, &one, &a.dir.join("2019/one.jpg")),
        organized(&a, &two, &a.dir.join("2019/two.jpg")),
    ];

    let _refused = unsupported_for(d1.join(".DS_Store"));
    let err = crate::organize(&a.db, a.run, &moves).unwrap_err();

    assert!(crate::is_no_exclusive_rename(&err), "{err:#}");
    assert_eq!(fs::read(d1.join(".DS_Store")).unwrap(), b"finder one");
    assert_eq!(fs::read(d2.join(".DS_Store")).unwrap(), b"finder two");
    assert!(fs::symlink_metadata(d2.join(pc_core::QUARANTINE_DIR)).is_err());
    // Both photographs moved before the sweep, and stay journaled.
    assert_eq!(a.db.journal_by_run_op(a.run, "organize").unwrap().len(), 2);
    assert!(!denies_earlier_moves(&err.to_string()), "{err}");
}

/// Walking a reorganisation back: a volume refusal stops the walk instead of
/// being collected as one more failure while the next entries are tried.
#[test]
fn a_volume_refusal_stops_the_undo_of_a_run() {
    let a = archive();
    let names = ["one.jpg", "two.jpg", "three.jpg"];
    let moves = names.map(|n| {
        let src = a.dir.join(n);
        fs::write(&src, n.as_bytes()).unwrap();
        organized(&a, &src, &a.dir.join("2019").join(n))
    });
    assert_eq!(crate::organize(&a.db, a.run, &moves).unwrap().moved, 3);

    // Newest first: three.jpg comes back, the volume refuses two.jpg, and
    // one.jpg must not be tried at all.
    let _refused = unsupported_for(a.dir.join("2019/two.jpg"));
    let err = crate::undo_run(&a.db, a.run).unwrap_err();

    assert!(crate::is_no_exclusive_rename(&err), "{err:#}");
    assert_eq!(fs::read(a.dir.join("three.jpg")).unwrap(), b"three.jpg");
    assert_eq!(fs::read(a.dir.join("2019/two.jpg")).unwrap(), b"two.jpg");
    assert_eq!(
        fs::read(a.dir.join("2019/one.jpg")).unwrap(),
        b"one.jpg",
        "после отказа тома откат продолжился"
    );
    assert_eq!(a.db.journal_by_run_op(a.run, "organize").unwrap().len(), 2);
    let shown = err.to_string();
    assert!(
        shown.contains("1 entry") || shown.contains("1 запись"),
        "what came back before the stop: {shown}"
    );
}

// ---------------------------------------------------------------- B4 -----

/// The second photograph's move is refused by the volume, the third is
/// never tried. The first stays moved and undoable, and the caller is told
/// exactly that — not that nothing moved.
#[test]
fn a_run_stopped_halfway_says_what_had_moved_and_how_to_undo_it() {
    let a = archive();
    let paths = ["one.arw", "two.arw", "three.arw"].map(|n| a.dir.join(n));
    for p in &paths {
        fs::write(p, b"raw frame").unwrap();
    }
    let cs = paths.each_ref().map(|p| candidate(&a.db, a.run, p));
    let _refused = unsupported_for(paths[1].clone());

    let err = crate::apply(&a.db, a.run, &cs, None).unwrap_err();

    assert!(crate::is_no_exclusive_rename(&err));
    assert!(!paths[0].exists());
    assert!(paths[1].exists() && paths[2].exists());
    assert_eq!(journal_rows(&a.db), 2);
    let shown = err.to_string();
    assert!(!denies_earlier_moves(&shown), "{shown}");
    assert!(
        shown.contains(&a.quarantine.join("two.arw").display().to_string()),
        "the refusal names its file: {shown}"
    );
    assert!(
        shown.contains("1 file") || shown.contains("1 файл"),
        "partial totals: {shown}"
    );
    assert!(shown.contains("undo"), "the undo route: {shown}");
    let done = a.db.journal_quarantined(None).unwrap();
    assert_eq!(done.len(), 1);
    crate::undo(&a.db, done[0].id).unwrap();
    assert_eq!(fs::read(&paths[0]).unwrap(), b"raw frame");
}

// ---------------------------------------------------------------- B5 -----

/// Recovering an interrupted operation meets a stranger at home. The entry
/// stays pending, and the journal says which file and why.
#[cfg(unix)]
#[test]
fn a_refused_recovery_is_written_down_and_stays_pending() {
    let a = archive();
    let home = a.dir.join("interrupted.arw");
    let held = a.quarantine.join("interrupted.arw");
    fs::create_dir_all(&a.quarantine).unwrap();
    fs::write(&held, b"raw frame").unwrap();
    let id = entry(
        &a,
        &home,
        &held,
        &[pair(&home, &held)],
        JournalStatus::Pending,
    );
    let (hook, foreign) = foreign_at(home.clone(), Stranger::Dangling);

    let err = crate::reconcile_undo(&a.db, id).unwrap_err();
    drop(hook);

    assert!(
        err.to_string().contains(&home.display().to_string()),
        "{err:#}"
    );
    assert_eq!(Some(signature(&home)), *foreign.borrow());
    assert_eq!(fs::read(&held).unwrap(), b"raw frame");
    let (status, note) = journal_row(&a.db, id);
    assert_eq!(status, "pending");
    assert!(note.contains(&home.display().to_string()), "{note}");
}

/// Two files to bring back; the first comes, the volume refuses the second.
/// The journal keeps both facts, the entry stays pending, and the volume
/// refusal keeps its type for the caller.
#[test]
fn a_recovery_stopped_by_the_volume_says_what_came_back() {
    let a = archive();
    let (home, side) = (a.dir.join("f.arw"), a.dir.join("f.xmp"));
    let (held, held_side) = (a.quarantine.join("f.arw"), a.quarantine.join("f.xmp"));
    fs::create_dir_all(&a.quarantine).unwrap();
    fs::write(&held, b"raw").unwrap();
    fs::write(&held_side, b"edits").unwrap();
    let id = entry(
        &a,
        &home,
        &held,
        &[pair(&home, &held), pair(&side, &held_side)],
        JournalStatus::Pending,
    );

    let _refused = unsupported_for(held_side.clone());
    let err = crate::reconcile_undo(&a.db, id).unwrap_err();

    assert!(crate::is_no_exclusive_rename(&err), "{err:#}");
    assert_eq!(fs::read(&home).unwrap(), b"raw");
    assert_eq!(fs::read(&held_side).unwrap(), b"edits");
    let (status, note) = journal_row(&a.db, id);
    assert_eq!(status, "pending");
    assert!(note.contains(&side.display().to_string()), "{note}");
    assert!(note.contains(&home.display().to_string()), "{note}");
}
