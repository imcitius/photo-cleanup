//! A quarantined frame whose recorded place no longer leads to it is looked
//! for by its evidence, and only a frame alone comes back (el-6byp2,
//! diagnosis el-lvtmk D1/D3/R8).
//!
//! The common case: the default quarantine sits inside the photo's own
//! folder, so a user who renames that folder after `apply` takes the
//! quarantine along, and every recorded path of the entry leads nowhere.
//! Each test stages that on a disposable folder, through the real `apply`,
//! and asks the functions the command line and the web call.

use crate::*;
use pc_db::{Db, JournalStatus, ReportOnly, Sought, SoughtFinding};
use pc_family::plan::Candidate;

struct Fx {
    _tmp: tempfile::TempDir,
    top: PathBuf,
    archive: PathBuf,
    db: Db,
    run: i64,
}

fn fx() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let top = tmp.path().canonicalize().unwrap();
    let archive = top.join("archive");
    fs::create_dir_all(&archive).unwrap();
    let db = Db::open(&top.join("pc.db")).unwrap();
    let run = db
        .start_run(&[archive.display().to_string()], "test")
        .unwrap();
    Fx {
        _tmp: tmp,
        top,
        archive,
        db,
        run,
    }
}

const FRAME: &[u8] = b"BM synthetic frame, never a real photograph";

impl Fx {
    /// `archive/<folder>/<name>`, written and quarantined by `apply` with
    /// its default quarantine beside it. Returns the entry.
    fn quarantined(&self, folder: &str, name: &str, companion: bool) -> pc_db::JournalEntry {
        let photo = self.archive.join(folder).join(name);
        fs::create_dir_all(photo.parent().unwrap()).unwrap();
        fs::write(&photo, FRAME).unwrap();
        if companion {
            fs::write(photo.with_extension("xmp"), b"<x:xmpmeta/>").unwrap();
        }
        let file_id = self
            .db
            .upsert_file(
                &pc_db::NewFile {
                    path: photo.display().to_string(),
                    name: name.into(),
                    size: FRAME.len() as i64,
                    ..Default::default()
                },
                self.run,
            )
            .unwrap();
        let c = Candidate {
            file_id,
            family_id: 0,
            path: photo.display().to_string(),
            size: FRAME.len() as i64,
            role: pc_family::Role::Copy,
            keeper_id: 0,
            keeper_path: String::new(),
            reason: "the user's pick".into(),
            manual: true,
            group_keeper: String::new(),
        };
        let r = apply(&self.db, self.run, &[c], None).unwrap();
        assert_eq!(r.done.frames, 1, "scenario: {:?}", r.refused);
        let e = self.db.journal_quarantined(None).unwrap().pop().unwrap();
        assert_eq!(e.manifest.len(), if companion { 2 } else { 1 });
        e
    }

    fn entry(&self, id: i64) -> pc_db::JournalEntry {
        self.db.journal_entry(id).unwrap().unwrap()
    }

    /// The `(phase, kind)` of every event of the entry, and every `sought`
    /// record they carry.
    fn history(&self, id: i64) -> (Vec<(String, String)>, Vec<Sought>) {
        let mut kinds = Vec::new();
        let mut sought = Vec::new();
        for e in self.db.journal_events(id).unwrap() {
            kinds.push((e.phase.clone(), e.kind.clone()));
            sought.extend(e.sought().unwrap());
        }
        (kinds, sought)
    }

    fn renamed(&self, from: &str, to: &str) -> PathBuf {
        let to = self.archive.join(to);
        fs::rename(self.archive.join(from), &to).unwrap();
        to
    }
}

fn q(folder: &Path) -> PathBuf {
    folder.join(pc_core::QUARANTINE_DIR)
}

fn sought_of(items: &[Item]) -> Vec<&Sought> {
    items.iter().filter_map(|i| i.sought.as_ref()).collect()
}

/// Nothing moved: every path given is where it was, with its bytes.
fn unmoved(paths: &[(&Path, &[u8])]) {
    for (p, bytes) in paths {
        assert_eq!(&fs::read(p).unwrap(), bytes, "{} moved", p.display());
    }
}

#[test]
fn a_frame_whose_folder_was_renamed_is_found_by_evidence_and_comes_back_after_its_preview() {
    let f = fx();
    let e = f.quarantined("2015", "IMG_0001.bmp", false);
    let home = PathBuf::from(&e.src);
    let moved = f.renamed("2015", "2015 Crimea");
    let now = q(&moved).join("IMG_0001.bmp");

    // The preview reads, finds, and offers it from where it is now.
    let items = undo_preview(&f.db, &f.entry(e.id)).unwrap();
    assert_eq!(items.len(), 1);
    let s = items[0].sought.as_ref().expect("searched");
    assert_eq!(
        s.finding,
        SoughtFinding::Found {
            at: now.display().to_string()
        }
    );
    assert_eq!(s.report_only, None);
    assert!(s.scope.contains(&q(&moved).display().to_string()), "{s:?}");
    assert_eq!(items[0].standing, Standing::Moved);
    assert_eq!(items[0].dst, now.display().to_string());
    assert!(items[0].why().is_none(), "{:?}", items[0].why());

    // Not bound to a preview: shown, not moved.
    let err = undo(&f.db, e.id).unwrap_err();
    let u = unrecovered(&err).expect("typed");
    assert_eq!(u.sought.len(), 1);
    assert!(
        format!("{err:#}").contains(&now.display().to_string()),
        "{err:#}"
    );
    unmoved(&[(&now, FRAME)]);
    assert!(!home.exists());
    assert_eq!(f.entry(e.id).status, JournalStatus::Done);

    // Bound to the place the preview showed: it comes home.
    let done = undo_found(&f.db, e.id, &now.display().to_string()).unwrap();
    assert_eq!(done.files_back, 1);
    assert_eq!(fs::read(&home).unwrap(), FRAME);
    assert!(!now.exists());
    assert_eq!(f.entry(e.id).status, JournalStatus::Undone);
    let (kinds, sought) = f.history(e.id);
    assert!(
        kinds.contains(&("undo".into(), "refused".into())),
        "{kinds:?}"
    );
    assert!(
        kinds.contains(&("undo".into(), "found".into())),
        "{kinds:?}"
    );
    assert!(kinds.contains(&("undo".into(), "done".into())), "{kinds:?}");
    assert!(sought
        .iter()
        .all(|s| s.movable() == Some(&now.display().to_string())));
}

#[test]
fn two_names_for_the_frame_are_both_shown_and_neither_is_moved() {
    let f = fx();
    let e = f.quarantined("2015", "IMG_0002.bmp", false);
    let moved = f.renamed("2015", "2015-renamed");
    let now = q(&moved).join("IMG_0002.bmp");
    let other = f.archive.join("later");
    fs::create_dir_all(q(&other)).unwrap();
    let twin = q(&other).join("IMG_0002.bmp");
    fs::hard_link(&now, &twin).unwrap();

    let items = undo_preview(&f.db, &f.entry(e.id)).unwrap();
    let s = sought_of(&items);
    assert_eq!(s.len(), 1);
    match &s[0].finding {
        SoughtFinding::Ambiguous { at } => {
            assert!(at.contains(&now.display().to_string()), "{at:?}");
            assert!(at.contains(&twin.display().to_string()), "{at:?}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(s[0].report_only, Some(ReportOnly::NotProven));
    assert!(items[0].why().is_some());

    for at in [&now, &twin] {
        let err = undo_found(&f.db, e.id, &at.display().to_string()).unwrap_err();
        assert!(unrecovered(&err).is_some(), "{err:#}");
    }
    unmoved(&[(&now, FRAME), (&twin, FRAME)]);
    assert!(!PathBuf::from(&e.src).exists());
    assert_eq!(f.entry(e.id).status, JournalStatus::Done);
    let (kinds, sought) = f.history(e.id);
    assert!(
        kinds.contains(&("undo".into(), "refused".into())),
        "{kinds:?}"
    );
    assert!(sought.iter().all(|s| s.movable().is_none()));
}

#[test]
fn a_frame_with_a_companion_is_found_and_only_shown() {
    let f = fx();
    let e = f.quarantined("2016", "IMG_0003.bmp", true);
    let moved = f.renamed("2016", "2016 old");
    let frame = q(&moved).join("IMG_0003.bmp");
    let side = q(&moved).join("IMG_0003.xmp");

    let items = undo_preview(&f.db, &f.entry(e.id)).unwrap();
    let s = sought_of(&items);
    assert_eq!(s.len(), 2, "{items:?}");
    assert!(s.iter().any(|s| s.finding
        == SoughtFinding::Found {
            at: frame.display().to_string()
        }));
    assert!(s.iter().any(|s| s.finding
        == SoughtFinding::Found {
            at: side.display().to_string()
        }));
    assert!(s
        .iter()
        .all(|s| s.report_only == Some(ReportOnly::Companion)));
    let why = items
        .iter()
        .filter_map(Item::why)
        .collect::<Vec<_>>()
        .join("; ");
    assert!(why.contains(&frame.display().to_string()), "{why}");

    let err = undo_found(&f.db, e.id, &frame.display().to_string()).unwrap_err();
    assert!(unrecovered(&err).is_some(), "{err:#}");
    assert!(undo(&f.db, e.id).is_err());
    unmoved(&[(&frame, FRAME), (&side, b"<x:xmpmeta/>")]);
    assert!(!PathBuf::from(&e.src).exists());
    let (_, sought) = f.history(e.id);
    assert!(!sought.is_empty() && sought.iter().all(|s| s.movable().is_none()));
}

#[test]
fn a_row_without_evidence_is_not_searched_and_moves_nothing() {
    let f = fx();
    // Written the way versions before the evidence wrote rows.
    let folder = f.archive.join("2010");
    let held = q(&folder).join("IMG_0004.bmp");
    fs::create_dir_all(held.parent().unwrap()).unwrap();
    fs::write(&held, FRAME).unwrap();
    let (s, d) = (
        folder.join("IMG_0004.bmp").display().to_string(),
        held.display().to_string(),
    );
    let id =
        f.db.journal_begin(&pc_db::NewJournalEntry {
            run_id: f.run,
            op: "quarantine-file",
            target_id: None,
            src: &s,
            dst: Some(&d),
            size: FRAME.len() as i64,
            file_count: 1,
            manifest: &[pc_db::Moved::new(s.clone(), d.clone())],
        })
        .unwrap();
    f.db.journal_finish(id, JournalStatus::Done, None).unwrap();
    let moved = f.renamed("2010", "2010 renamed");
    let now = q(&moved).join("IMG_0004.bmp");

    let items = undo_preview(&f.db, &f.entry(id)).unwrap();
    let s = sought_of(&items);
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].finding, SoughtFinding::NoEvidence);
    assert_eq!(s[0].report_only, Some(ReportOnly::NoEvidence));
    assert!(
        s[0].scope.is_empty(),
        "nothing is read for a row without evidence"
    );

    assert!(undo(&f.db, id).is_err());
    let err = undo_found(&f.db, id, &now.display().to_string()).unwrap_err();
    assert!(unrecovered(&err).is_some(), "{err:#}");
    unmoved(&[(&now, FRAME)]);
    assert_eq!(f.entry(id).status, JournalStatus::Done);
    let (kinds, _) = f.history(id);
    assert!(
        kinds.contains(&("undo".into(), "refused".into())),
        "{kinds:?}"
    );
}

#[test]
fn a_stranger_with_the_same_name_and_size_is_not_taken_for_the_frame() {
    let f = fx();
    let e = f.quarantined("2017", "IMG_0005.bmp", false);
    let moved = f.renamed("2017", "2017 x");
    let now = q(&moved).join("IMG_0005.bmp");
    // The frame itself left every quarantine folder: it sits in the
    // archive proper, where the search never reads.
    let outside = moved.join("IMG_0005.bmp");
    fs::rename(&now, &outside).unwrap();
    let stranger = b"BM another picture, the very same size!!!!!";
    assert_eq!(stranger.len(), FRAME.len());
    fs::write(&now, stranger).unwrap();

    let items = undo_preview(&f.db, &f.entry(e.id)).unwrap();
    let s = sought_of(&items);
    assert_eq!(s.len(), 1);
    assert_eq!(s[0].finding, SoughtFinding::NotFound);
    assert_eq!(s[0].report_only, Some(ReportOnly::NotProven));

    let err = undo_found(&f.db, e.id, &now.display().to_string()).unwrap_err();
    assert!(unrecovered(&err).is_some(), "{err:#}");
    unmoved(&[(&now, stranger), (&outside, FRAME)]);
    assert!(!PathBuf::from(&e.src).exists());
    assert_eq!(f.entry(e.id).status, JournalStatus::Done);
}

#[test]
fn a_frame_that_changed_after_its_preview_is_not_moved() {
    // Moved again, rewritten, or joined by a companion — each between the
    // preview and the action.
    type Change = fn(&Path) -> PathBuf;
    let changes: [(&str, Change); 3] = [
        ("moved again", |at| {
            let folder = at.parent().unwrap().parent().unwrap();
            let again = folder.with_file_name("again");
            fs::rename(folder, &again).unwrap();
            q(&again).join(at.file_name().unwrap())
        }),
        ("rewritten", |at| {
            std::thread::sleep(std::time::Duration::from_millis(20));
            fs::write(at, b"BM rewritten frame of the very same size!!!").unwrap();
            at.to_path_buf()
        }),
        ("joined by a companion", |at| {
            fs::write(at.with_extension("xmp"), b"<x:xmpmeta/>").unwrap();
            at.to_path_buf()
        }),
    ];
    for (what, change) in changes {
        let f = fx();
        let e = f.quarantined("2018", "IMG_0006.bmp", false);
        let moved = f.renamed("2018", "2018 y");
        let previewed = q(&moved).join("IMG_0006.bmp");
        let items = undo_preview(&f.db, &f.entry(e.id)).unwrap();
        assert_eq!(items[0].dst, previewed.display().to_string(), "{what}");
        let before = fs::read(&previewed).unwrap();

        let now = change(&previewed);
        let err = undo_found(&f.db, e.id, &previewed.display().to_string()).unwrap_err();

        let u = unrecovered(&err).unwrap_or_else(|| panic!("{what}: {err:#}"));
        assert_eq!(u.previewed.as_deref(), Some(previewed.to_str().unwrap()));
        assert!(!PathBuf::from(&e.src).exists(), "{what}: moved home");
        assert!(now.exists(), "{what}");
        if what != "rewritten" {
            assert_eq!(fs::read(&now).unwrap(), before, "{what}");
        }
        assert_eq!(f.entry(e.id).status, JournalStatus::Done, "{what}");
        let (kinds, _) = f.history(e.id);
        assert!(
            kinds.contains(&("undo".into(), "refused".into())),
            "{what}: {kinds:?}"
        );
    }
}

#[test]
fn an_interrupted_entry_whose_quarantine_moved_is_reconciled_only_from_its_preview() {
    // el-lvtmk #15: killed after the rename, and the folder renamed since.
    let f = fx();
    let e = f.quarantined("2019", "IMG_0007.bmp", false);
    f.db.conn
        .execute("UPDATE journal SET status='pending' WHERE id=?1", [e.id])
        .unwrap();
    let moved = f.renamed("2019", "2019 z");
    let now = q(&moved).join("IMG_0007.bmp");

    let items = reconcile(&f.db, e.id).unwrap();
    assert_eq!(items[0].standing, Standing::Moved);
    assert_eq!(items[0].dst, now.display().to_string());
    assert!(reconcile_undo(&f.db, e.id).is_err());
    unmoved(&[(&now, FRAME)]);

    let r = reconcile_found(&f.db, e.id, &now.display().to_string()).unwrap();
    assert_eq!(r.done.files_back, 1);
    assert_eq!(fs::read(&e.src).unwrap(), FRAME);
    assert_eq!(f.entry(e.id).status, JournalStatus::Undone);
    let (kinds, _) = f.history(e.id);
    assert!(
        kinds.contains(&("reconcile".into(), "found".into())),
        "{kinds:?}"
    );
}

#[test]
fn nothing_outside_the_run_and_its_quarantine_folders_is_read() {
    // The frame's quarantine was carried outside every root of the run:
    // the search does not follow it there.
    let f = fx();
    let e = f.quarantined("2020", "IMG_0008.bmp", false);
    let away = f.top.join("elsewhere");
    fs::rename(f.archive.join("2020"), &away).unwrap();

    let items = undo_preview(&f.db, &f.entry(e.id)).unwrap();
    let s = sought_of(&items);
    assert_eq!(s[0].finding, SoughtFinding::NotFound);
    assert!(
        s[0].scope
            .iter()
            .all(|p| Path::new(p).starts_with(&f.archive)),
        "{:?}",
        s[0].scope
    );
    assert!(undo(&f.db, e.id).is_err());
    unmoved(&[(&q(&away).join("IMG_0008.bmp"), FRAME)]);
}

#[test]
fn review_unrelated_run_must_not_expand_search() {
    let f = fx();
    let e = f.quarantined("2015", "frame.bmp", false);
    f.db.start_run(&[f.top.display().to_string()], "unrelated historical scan").unwrap();
    let away = f.top.join("outside-the-entry-run");
    fs::rename(f.archive.join("2015"), &away).unwrap();
    let items = undo_preview(&f.db, &e).unwrap();
    eprintln!("REVIEW historical roots: {items:?}");
    assert!(items[0].sought.as_ref().unwrap().movable().is_none(), "outside originating run offered for recovery");
}

#[test]
fn review_partial_proof_must_remain_report_only() {
    let f = fx();
    let mut e = f.quarantined("2015", "frame.bmp", false);
    let p = e.manifest[0].proof.as_mut().unwrap();
    p.size = None;
    p.mtime_ns = None;
    p.birth_ns = None;
    p.blake3 = None;
    f.db.journal_record_manifest(e.id, &e.manifest).unwrap();
    let moved = f.renamed("2015", "renamed");
    let now = q(&moved).join("frame.bmp");
    let items = undo_preview(&f.db, &f.entry(e.id)).unwrap();
    let result = undo_found(&f.db, e.id, now.to_str().unwrap());
    eprintln!("REVIEW partial proof: {items:?}; action={result:?}; home={}", Path::new(&e.src).exists());
    assert!(result.is_err(), "dev:ino-only proof moved a file");
}

#[test]
fn review_companion_at_last_check_must_refuse() {
    let f = fx();
    let e = f.quarantined("2015", "frame.bmp", false);
    let moved = f.renamed("2015", "renamed");
    let now = q(&moved).join("frame.bmp");
    assert!(undo_preview(&f.db, &e).unwrap()[0].sought.as_ref().unwrap().movable().is_some());
    let side = now.with_extension("xmp");
    let side2 = side.clone();
    let _guard = crate::race::before_move(move |_, _| {
        fs::write(&side2, b"foreign edit payload")?;
        Ok(())
    });
    let result = undo_found(&f.db, e.id, now.to_str().unwrap());
    eprintln!("REVIEW companion race: {result:?}; frame_home={}; companion_stays={}", Path::new(&e.src).exists(), side.exists());
    assert_eq!(fs::read(&side).unwrap(), b"foreign edit payload");
    assert!(result.is_err(), "frame moved away from companion that appeared before final checks");
}

#[test]
fn review_unreadable_candidate_tree_must_not_establish_uniqueness() {
    use std::os::unix::fs::PermissionsExt;
    let f = fx();
    let e = f.quarantined("2015", "frame.bmp", false);
    let moved = f.renamed("2015", "renamed");
    let now = q(&moved).join("frame.bmp");
    let hidden = q(&f.archive.join("second-candidate"));
    fs::create_dir_all(&hidden).unwrap();
    let twin = hidden.join("frame.bmp");
    fs::hard_link(&now, &twin).unwrap();
    fs::set_permissions(&hidden, fs::Permissions::from_mode(0o000)).unwrap();
    let items = undo_preview(&f.db, &e).unwrap();
    let result = undo_found(&f.db, e.id, now.to_str().unwrap());
    fs::set_permissions(&hidden, fs::Permissions::from_mode(0o700)).unwrap();
    eprintln!("REVIEW unreadable search: {items:?}; action={result:?}");
    assert_eq!(fs::read(&twin).unwrap(), FRAME);
    assert!(result.is_err(), "incomplete search treated as unique candidate");
}

#[test]
fn review_replaced_anchor_ancestor_must_not_escape() {
    use std::os::unix::fs::symlink;
    let f = fx();
    let e = f.quarantined("2015", "frame.bmp", false);
    let old = f.archive.join("2015");
    let saved = f.archive.join("saved");
    fs::rename(&old, &saved).unwrap();
    let outside = f.top.join("foreign-namespace");
    fs::create_dir_all(q(&outside)).unwrap();
    let found = q(&outside).join("different.bmp");
    fs::rename(q(&saved).join("frame.bmp"), &found).unwrap();
    symlink(&outside, &old).unwrap();
    let items = undo_preview(&f.db, &e).unwrap();
    eprintln!("REVIEW replaced anchor: {items:?}");
    assert!(items[0].sought.as_ref().unwrap().movable().is_none(), "unverified symlink ancestor escaped the run");
}

#[test]
fn review_photoslibrary_inside_quarantine_must_not_be_entered() {
    let f = fx();
    let e = f.quarantined("2015", "frame.bmp", false);
    let moved = f.renamed("2015", "renamed");
    let lib = q(&moved).join("Foreign.photoslibrary");
    fs::create_dir(&lib).unwrap();
    let now = lib.join("frame.bmp");
    fs::rename(q(&moved).join("frame.bmp"), &now).unwrap();
    let items = undo_preview(&f.db, &e).unwrap();
    eprintln!("REVIEW photoslibrary: {items:?}");
    assert!(items[0].sought.as_ref().unwrap().movable().is_none(), "Photos library content offered for automatic move");
}

#[test]
fn review_foreign_replacement_after_preview_preserves_metadata_and_refuses() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let f = fx();
    let e = f.quarantined("2015", "frame.bmp", false);
    let moved = f.renamed("2015", "renamed");
    let now = q(&moved).join("frame.bmp");
    let _ = undo_preview(&f.db, &e).unwrap();
    let saved = f.top.join("saved-frame.bmp");
    fs::rename(&now, &saved).unwrap();
    fs::write(&now, vec![b'X'; FRAME.len()]).unwrap();
    fs::set_permissions(&now, fs::Permissions::from_mode(0o640)).unwrap();
    let original_times = fs::FileTimes::new().set_modified(fs::metadata(&saved).unwrap().modified().unwrap());
    fs::File::options().write(true).open(&now).unwrap().set_times(original_times).unwrap();
    #[cfg(target_os = "macos")]
    let acl_before = {
        assert!(std::process::Command::new("/usr/bin/xattr").args(["-w", "review.fixture", "foreign metadata"]).arg(&now).status().unwrap().success());
        assert!(std::process::Command::new("/bin/chmod").args(["+a", "everyone deny delete"]).arg(&now).status().unwrap().success());
        std::process::Command::new("/bin/ls").arg("-lde").arg(&now).output().unwrap().stdout
    };
    let before = fs::symlink_metadata(&now).unwrap();
    let result = undo_found(&f.db, e.id, now.to_str().unwrap()).unwrap_err();
    let after = fs::symlink_metadata(&now).unwrap();
    assert!(unrecovered(&result).is_some());
    #[cfg(target_os = "macos")]
    {
        assert_eq!(std::process::Command::new("/usr/bin/xattr").args(["-p", "review.fixture"]).arg(&now).output().unwrap().stdout, b"foreign metadata\n");
        assert_eq!(std::process::Command::new("/bin/ls").arg("-lde").arg(&now).output().unwrap().stdout, acl_before);
        assert!(std::process::Command::new("/bin/chmod").arg("-N").arg(&now).status().unwrap().success());
    }
    assert_eq!(fs::read(&now).unwrap(), vec![b'X'; FRAME.len()]);
    assert_eq!((before.ino(), before.mode(), before.uid(), before.gid(), before.mtime_nsec()), (after.ino(), after.mode(), after.uid(), after.gid(), after.mtime_nsec()));
    assert_eq!(fs::read(&saved).unwrap(), FRAME);
    assert!(!Path::new(&e.src).exists());
    assert!(f.db.journal_events(e.id).unwrap().iter().any(|e| e.kind == "refused"));
}

#[test]
fn review_recorded_companion_vanishing_never_enables_frame_only() {
    let f = fx();
    let e = f.quarantined("2015", "frame.bmp", true);
    let moved = f.renamed("2015", "renamed");
    let now = q(&moved).join("frame.bmp");
    let saved_side = f.top.join("saved.xmp");
    fs::rename(now.with_extension("xmp"), &saved_side).unwrap();
    let result = undo_found(&f.db, e.id, now.to_str().unwrap()).unwrap_err();
    assert!(unrecovered(&result).is_some());
    assert_eq!(fs::read(&now).unwrap(), FRAME);
    assert_eq!(fs::read(saved_side).unwrap(), b"<x:xmpmeta/>");
    assert!(!Path::new(&e.src).exists());
}

#[test]
fn review_leaf_quarantine_symlink_does_not_expand_scope() {
    use std::os::unix::fs::symlink;
    let f = fx();
    let e = f.quarantined("2015", "frame.bmp", false);
    let away = f.top.join("outside");
    fs::rename(f.archive.join("2015"), &away).unwrap();
    let fake = f.archive.join("fake");
    fs::create_dir(&fake).unwrap();
    symlink(q(&away), q(&fake)).unwrap();
    let result = undo(&f.db, e.id).unwrap_err();
    assert!(unrecovered(&result).is_some());
    assert_eq!(fs::read(q(&away).join("frame.bmp")).unwrap(), FRAME);
}

#[test]
fn review_change_at_move_boundary_must_have_typed_search_refusal() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let f = fx();
    let e = f.quarantined("2015", "frame.bmp", false);
    let moved = f.renamed("2015", "renamed");
    let now = q(&moved).join("frame.bmp");
    let saved = f.top.join("retained-frame.bmp");
    let saved2 = saved.clone();
    let _ = undo_preview(&f.db, &e).unwrap();
    let _guard = crate::race::before_move(move |src, _| {
        fs::rename(src, &saved2)?;
        fs::write(src, b"foreign payload")?;
        fs::set_permissions(src, fs::Permissions::from_mode(0o640))?;
        Ok(())
    });
    let err = undo_found(&f.db, e.id, now.to_str().unwrap()).unwrap_err();
    assert_eq!(fs::read(&now).unwrap(), b"foreign payload");
    assert_eq!(fs::metadata(&now).unwrap().mode() & 0o777, 0o640);
    assert_eq!(fs::read(saved).unwrap(), FRAME);
    assert!(!Path::new(&e.src).exists());
    let events = f.db.journal_events(e.id).unwrap();
    eprintln!("REVIEW late refusal: {err:#}; Unrecovered={}; events={events:?}", unrecovered(&err).is_some());
    assert!(unrecovered(&err).is_some(), "post-search refusal loses the typed sought/preview binding");
}

#[test]
fn review_dangling_companion_must_fail_closed() {
    use std::os::unix::fs::symlink;
    let f = fx();
    let e = f.quarantined("2015", "frame.bmp", false);
    let moved = f.renamed("2015", "renamed");
    let now = q(&moved).join("frame.bmp");
    let side = now.with_extension("xmp");
    symlink("unavailable-edits.xmp", &side).unwrap();
    let result = undo_found(&f.db, e.id, now.to_str().unwrap());
    assert_eq!(fs::read_link(side).unwrap(), PathBuf::from("unavailable-edits.xmp"));
    eprintln!("REVIEW dangling companion: {result:?}");
    assert!(result.is_err(), "unknown companion treated as absence");
}
