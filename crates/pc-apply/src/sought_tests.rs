//! A quarantined frame whose recorded place no longer leads to it is looked
//! for by its evidence and only reported: where it is and how that was
//! proven, never moved by the tool (el-6byp2, diagnosis el-lvtmk D1/D3/R8;
//! director decision after review el-1oudi).
//!
//! The common case: the default quarantine sits inside the photo's own
//! folder, so a user who renames that folder after `apply` takes the
//! quarantine along, and every recorded path of the entry leads nowhere.
//! Each test stages that on a disposable folder, through the real `apply`,
//! and asks the functions the command line and the web call.

use crate::*;
use pc_db::{Db, JournalStatus, Sought, SoughtFinding};
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

fn one(items: &[Item]) -> Sought {
    let s = sought_of(items);
    assert_eq!(s.len(), 1, "{items:?}");
    s[0].clone()
}

fn at(p: &Path) -> String {
    p.display().to_string()
}

fn names(s: &Sought) -> Vec<String> {
    s.hits.iter().map(|h| h.at.clone()).collect()
}

/// Nothing moved: every path given is where it was, with its bytes.
fn unmoved(paths: &[(&Path, &[u8])]) {
    for (p, bytes) in paths {
        assert_eq!(&fs::read(p).unwrap(), bytes, "{} moved", p.display());
    }
}

/// An undo of `id` refuses, moves nothing, leaves the entry `Done`, and its
/// typed refusal carries the very searches the entry's history now ends
/// with. Returns them and the words.
fn undo_reports(f: &Fx, id: i64) -> (Vec<Sought>, String) {
    let before = f.db.journal_events(id).unwrap().len();
    let err = undo(&f.db, id).unwrap_err();
    let u = unrecovered(&err).unwrap_or_else(|| panic!("untyped: {err:#}"));
    assert_eq!(f.entry(id).status, JournalStatus::Done);
    let events = f.db.journal_events(id).unwrap();
    assert_eq!(events.len(), before + 1, "{events:?}");
    let last = events.last().unwrap();
    assert_eq!(
        (last.phase.as_str(), last.kind.as_str()),
        ("undo", "refused")
    );
    assert_eq!(last.sought().unwrap(), u.sought, "event and result differ");
    (u.sought.clone(), format!("{err:#}"))
}

#[test]
fn a_frame_whose_folder_was_renamed_is_found_by_evidence_and_only_reported() {
    let f = fx();
    let e = f.quarantined("2015", "IMG_0001.bmp", false);
    let home = PathBuf::from(&e.src);
    let moved = f.renamed("2015", "2015 Crimea");
    let now = q(&moved).join("IMG_0001.bmp");

    // The preview reads, finds, proves, and says so — and offers nothing.
    let items = undo_preview(&f.db, &f.entry(e.id)).unwrap();
    let s = one(&items);
    assert_eq!(s.finding, SoughtFinding::Found, "{s:?}");
    let hit = s.found().expect("found");
    assert_eq!(hit.at, at(&now));
    assert!(hit.proven && hit.how.contains("size"), "{hit:?}");
    assert!(s.scope.contains(&at(&q(&moved))), "{s:?}");
    assert!(s.unread.is_empty() && s.skipped.is_empty(), "{s:?}");
    assert_eq!(items[0].standing, Standing::Gone);
    let why = items[0].why().unwrap();
    assert!(why.contains(&at(&now)), "{why}");
    assert!(why.contains("return it by hand"), "{why}");

    // The undo says the same, records it, and moves nothing — every time.
    for _ in 0..2 {
        let (sought, said) = undo_reports(&f, e.id);
        assert_eq!(sought, vec![s.clone()]);
        assert!(said.contains(&at(&now)), "{said}");
        unmoved(&[(&now, FRAME)]);
        assert!(!home.exists());
    }
}

#[test]
fn two_names_for_the_frame_are_both_shown_and_neither_is_claimed() {
    let f = fx();
    let e = f.quarantined("2015", "IMG_0002.bmp", false);
    let moved = f.renamed("2015", "2015-renamed");
    let now = q(&moved).join("IMG_0002.bmp");
    let other = f.archive.join("later");
    fs::create_dir_all(q(&other)).unwrap();
    let twin = q(&other).join("IMG_0002.bmp");
    fs::hard_link(&now, &twin).unwrap();

    let s = one(&undo_preview(&f.db, &f.entry(e.id)).unwrap());
    assert_eq!(s.finding, SoughtFinding::Ambiguous);
    assert!(s.found().is_none());
    let n = names(&s);
    assert!(n.contains(&at(&now)) && n.contains(&at(&twin)), "{n:?}");

    let (sought, said) = undo_reports(&f, e.id);
    assert_eq!(sought[0].finding, SoughtFinding::Ambiguous);
    assert!(
        said.contains(&at(&now)) && said.contains(&at(&twin)),
        "{said}"
    );
    unmoved(&[(&now, FRAME), (&twin, FRAME)]);
    assert!(!PathBuf::from(&e.src).exists());
}

#[test]
fn a_frame_and_its_recorded_companion_are_each_found_and_only_shown() {
    let f = fx();
    let e = f.quarantined("2016", "IMG_0003.bmp", true);
    let moved = f.renamed("2016", "2016 old");
    let frame = q(&moved).join("IMG_0003.bmp");
    let side = q(&moved).join("IMG_0003.xmp");

    let items = undo_preview(&f.db, &f.entry(e.id)).unwrap();
    let s = sought_of(&items);
    assert_eq!(s.len(), 2, "{items:?}");
    for want in [&frame, &side] {
        assert!(
            s.iter()
                .any(|s| s.found().is_some_and(|h| h.at == at(want))),
            "{s:?}"
        );
    }
    let (_, said) = undo_reports(&f, e.id);
    assert!(
        said.contains(&at(&frame)) && said.contains(&at(&side)),
        "{said}"
    );
    unmoved(&[(&frame, FRAME), (&side, b"<x:xmpmeta/>")]);
    assert!(!PathBuf::from(&e.src).exists());
}

#[test]
fn a_row_without_evidence_is_not_searched_and_moves_nothing() {
    let f = fx();
    // Written the way versions before the evidence wrote rows.
    let folder = f.archive.join("2010");
    let held = q(&folder).join("IMG_0004.bmp");
    fs::create_dir_all(held.parent().unwrap()).unwrap();
    fs::write(&held, FRAME).unwrap();
    let (s, d) = (at(&folder.join("IMG_0004.bmp")), at(&held));
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

    let s = one(&undo_preview(&f.db, &f.entry(id)).unwrap());
    assert_eq!(s.finding, SoughtFinding::NoEvidence);
    assert!(s.scope.is_empty() && s.hits.is_empty(), "nothing is read");
    let (sought, _) = undo_reports(&f, id);
    assert_eq!(sought[0].finding, SoughtFinding::NoEvidence);
    unmoved(&[(&now, FRAME)]);
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

    let s = one(&undo_preview(&f.db, &f.entry(e.id)).unwrap());
    assert_eq!(s.finding, SoughtFinding::NotFound, "{s:?}");
    assert!(s.hits.is_empty());
    undo_reports(&f, e.id);
    unmoved(&[(&now, stranger), (&outside, FRAME)]);
    assert!(!PathBuf::from(&e.src).exists());
}

#[test]
fn an_interrupted_entry_whose_quarantine_moved_is_reported_and_stays_pending() {
    // el-lvtmk #15: killed after the rename, and the folder renamed since.
    let f = fx();
    let e = f.quarantined("2019", "IMG_0007.bmp", false);
    f.db.conn
        .execute("UPDATE journal SET status='pending' WHERE id=?1", [e.id])
        .unwrap();
    let moved = f.renamed("2019", "2019 z");
    let now = q(&moved).join("IMG_0007.bmp");

    let items = reconcile(&f.db, e.id).unwrap();
    assert_eq!(items[0].standing, Standing::Gone);
    assert_eq!(one(&items).found().map(|h| h.at.clone()), Some(at(&now)));
    let err = reconcile_undo(&f.db, e.id).unwrap_err();
    let u = unrecovered(&err).expect("typed");
    assert_eq!(u.sought[0].finding, SoughtFinding::Found);
    unmoved(&[(&now, FRAME)]);
    assert!(!PathBuf::from(&e.src).exists());
    assert_eq!(f.entry(e.id).status, JournalStatus::Pending);
    let last = f.db.journal_events(e.id).unwrap().pop().unwrap();
    assert_eq!(
        (last.phase.as_str(), last.kind.as_str()),
        ("reconcile", "refused")
    );
    assert_eq!(last.sought().unwrap(), u.sought);
}

#[test]
fn a_preview_records_its_search_once_and_moves_nothing() {
    let f = fx();
    let e = f.quarantined("2021", "IMG_0009.bmp", false);
    let moved = f.renamed("2021", "2021 a");
    let now = q(&moved).join("IMG_0009.bmp");
    let items = undo_preview(&f.db, &f.entry(e.id)).unwrap();
    let before = f.db.journal_events(e.id).unwrap().len();
    record_sought(&f.db, e.id, "undo", &items).unwrap();
    record_sought(&f.db, e.id, "undo", &items).unwrap();
    let (kinds, sought) = f.history(e.id);
    assert_eq!(kinds.len(), before + 1, "{kinds:?}");
    assert_eq!(
        kinds.last().unwrap(),
        &("undo".to_string(), "sought".to_string())
    );
    assert_eq!(sought, vec![one(&items)]);
    unmoved(&[(&now, FRAME)]);
}

#[test]
fn nothing_outside_the_run_and_its_quarantine_folders_is_read() {
    // The frame's quarantine was carried outside every root of the run:
    // the search does not follow it there.
    let f = fx();
    let e = f.quarantined("2020", "IMG_0008.bmp", false);
    let away = f.top.join("elsewhere");
    fs::rename(f.archive.join("2020"), &away).unwrap();

    let s = one(&undo_preview(&f.db, &f.entry(e.id)).unwrap());
    assert_eq!(s.finding, SoughtFinding::NotFound);
    assert!(
        s.scope.iter().all(|p| Path::new(p).starts_with(&f.archive)),
        "{:?}",
        s.scope
    );
    undo_reports(&f, e.id);
    unmoved(&[(&q(&away).join("IMG_0008.bmp"), FRAME)]);
}

// Review el-1oudi (el-67ku), restated for the report-only contract: each
// was a place the search claimed, or would have moved, what it had not
// proven.

/// B1: another run's roots never widen the search.
#[test]
fn review_unrelated_run_must_not_expand_search() {
    let f = fx();
    let e = f.quarantined("2015", "frame.bmp", false);
    f.db.start_run(&[at(&f.top)], "unrelated historical scan")
        .unwrap();
    let away = f.top.join("outside-the-entry-run");
    fs::rename(f.archive.join("2015"), &away).unwrap();
    let s = one(&undo_preview(&f.db, &e).unwrap());
    assert_eq!(s.finding, SoughtFinding::NotFound, "{s:?}");
    assert!(s.hits.is_empty(), "{s:?}");
    assert!(s.scope.iter().all(|p| Path::new(p).starts_with(&f.archive)));
    undo_reports(&f, e.id);
    unmoved(&[(&q(&away).join("frame.bmp"), FRAME)]);
}

/// B5: device and inode alone prove nothing; neither does evidence without
/// the birth time the system reports.
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
    let s = one(&undo_preview(&f.db, &f.entry(e.id)).unwrap());
    assert_eq!(s.finding, SoughtFinding::Candidate, "{s:?}");
    assert!(s.found().is_none());
    assert_eq!(names(&s), vec![at(&now)]);
    assert!(!s.hits[0].proven && s.hits[0].how.contains("device and inode"));
    let (_, said) = undo_reports(&f, e.id);
    assert!(said.contains("not proven"), "{said}");
    unmoved(&[(&now, FRAME)]);
    assert!(!Path::new(&e.src).exists());
}

#[test]
fn evidence_without_the_birth_time_the_system_reports_is_only_a_candidate() {
    let f = fx();
    let mut e = f.quarantined("2015", "frame.bmp", false);
    let moved_probe = Path::new(&e.manifest[0].dst);
    if fs::metadata(moved_probe).unwrap().created().is_err() {
        eprintln!("this file system reports no birth time: nothing to withhold");
        return;
    }
    e.manifest[0].proof.as_mut().unwrap().birth_ns = None;
    f.db.journal_record_manifest(e.id, &e.manifest).unwrap();
    f.renamed("2015", "renamed");
    let s = one(&undo_preview(&f.db, &f.entry(e.id)).unwrap());
    assert_eq!(s.finding, SoughtFinding::Candidate, "{s:?}");
    assert!(s.hits[0].how.contains("birth time"), "{s:?}");
}

/// B4 restated: a companion beside the place found — even one that appeared
/// after the frame left, even a dangling link — is named, never judged, and
/// never moved.
#[test]
fn review_companions_beside_the_place_found_are_named_and_kept() {
    use std::os::unix::fs::symlink;
    let f = fx();
    let e = f.quarantined("2015", "frame.bmp", false);
    let moved = f.renamed("2015", "renamed");
    let now = q(&moved).join("frame.bmp");
    let side = now.with_extension("xmp");
    fs::write(&side, b"foreign edit payload").unwrap();
    let apple = q(&moved).join("._frame.bmp");
    symlink("unavailable-edits", &apple).unwrap();

    let s = one(&undo_preview(&f.db, &e).unwrap());
    let hit = s.found().expect("found");
    assert!(hit.beside.contains(&at(&side)), "{hit:?}");
    assert!(hit.beside.contains(&at(&apple)), "{hit:?}");
    let (_, said) = undo_reports(&f, e.id);
    assert!(
        said.contains("companion") && said.contains(&at(&side)),
        "{said}"
    );
    unmoved(&[(&now, FRAME), (&side, b"foreign edit payload")]);
    assert_eq!(
        fs::read_link(&apple).unwrap(),
        PathBuf::from("unavailable-edits")
    );
    assert!(!Path::new(&e.src).exists());
}

/// B3: what cannot be read makes the search uncertain, never unique.
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
    if fs::read_dir(&hidden).is_ok() {
        // Running with privileges that read anything: nothing to stage.
        fs::set_permissions(&hidden, fs::Permissions::from_mode(0o700)).unwrap();
        eprintln!("permissions do not bind this user; skipped");
        return;
    }
    let s = one(&undo_preview(&f.db, &e).unwrap());
    let err = undo(&f.db, e.id).unwrap_err();
    fs::set_permissions(&hidden, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(s.finding, SoughtFinding::Uncertain, "{s:?}");
    assert!(s.found().is_none());
    assert!(s.unread.iter().any(|g| g.at == at(&hidden)), "{s:?}");
    assert_eq!(names(&s), vec![at(&now)], "the name it saw is still named");
    assert!(unrecovered(&err).is_some_and(|u| u.sought[0].finding == SoughtFinding::Uncertain));
    unmoved(&[(&now, FRAME), (&twin, FRAME)]);
    assert!(!Path::new(&e.src).exists());
}

/// B2: a recorded folder replaced by a link is not followed, and says so.
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
    let s = one(&undo_preview(&f.db, &e).unwrap());
    assert_eq!(s.finding, SoughtFinding::Uncertain, "{s:?}");
    assert!(s.unread.iter().any(|g| g.at == at(&old)), "{s:?}");
    assert!(s.hits.is_empty(), "{s:?}");
    assert!(s.scope.iter().all(|p| !p.contains("foreign-namespace")));
    undo_reports(&f, e.id);
    unmoved(&[(&found, FRAME)]);
}

/// A quarantine folder of the run replaced by a link leads nowhere.
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
    let s = one(&undo_preview(&f.db, &e).unwrap());
    assert_eq!(s.finding, SoughtFinding::NotFound, "{s:?}");
    undo_reports(&f, e.id);
    unmoved(&[(&q(&away).join("frame.bmp"), FRAME)]);
}

/// B6: a Photos library, or anything of Lightroom's, inside a quarantine
/// folder is never entered — and the search says it did not look there.
#[test]
fn review_photoslibrary_inside_quarantine_must_not_be_entered() {
    for bundle in [
        "Foreign.photoslibrary",
        "Cat Previews.lrdata",
        "Cat.lrcat-data",
    ] {
        let f = fx();
        let e = f.quarantined("2015", "frame.bmp", false);
        let moved = f.renamed("2015", "renamed");
        let lib = q(&moved).join(bundle);
        fs::create_dir(&lib).unwrap();
        let now = lib.join("frame.bmp");
        fs::rename(q(&moved).join("frame.bmp"), &now).unwrap();
        let s = one(&undo_preview(&f.db, &e).unwrap());
        assert_eq!(
            s.finding,
            SoughtFinding::ProtectedSkipped,
            "{bundle}: {s:?}"
        );
        assert_eq!(s.skipped, vec![at(&lib)], "{bundle}");
        assert!(s.hits.is_empty(), "{bundle}: entered");
        let (_, said) = undo_reports(&f, e.id);
        assert!(said.contains(&at(&lib)), "{said}");
        unmoved(&[(&now, FRAME)]);
    }
}

/// B8: a foreign file where the frame was shown changes the answer of the
/// next search, typed and recorded; nothing of it is touched.
#[test]
fn review_foreign_replacement_after_preview_preserves_metadata_and_refuses() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let f = fx();
    let e = f.quarantined("2015", "frame.bmp", false);
    let moved = f.renamed("2015", "renamed");
    let now = q(&moved).join("frame.bmp");
    assert_eq!(
        one(&undo_preview(&f.db, &e).unwrap()).finding,
        SoughtFinding::Found
    );
    let saved = f.top.join("saved-frame.bmp");
    fs::rename(&now, &saved).unwrap();
    fs::write(&now, vec![b'X'; FRAME.len()]).unwrap();
    fs::set_permissions(&now, fs::Permissions::from_mode(0o640)).unwrap();
    let before = fs::symlink_metadata(&now).unwrap();
    let (sought, _) = undo_reports(&f, e.id);
    assert_eq!(sought[0].finding, SoughtFinding::NotFound);
    let after = fs::symlink_metadata(&now).unwrap();
    assert_eq!(fs::read(&now).unwrap(), vec![b'X'; FRAME.len()]);
    assert_eq!(
        (
            before.ino(),
            before.mode(),
            before.uid(),
            before.gid(),
            before.mtime_nsec()
        ),
        (
            after.ino(),
            after.mode(),
            after.uid(),
            after.gid(),
            after.mtime_nsec()
        )
    );
    assert_eq!(fs::read(&saved).unwrap(), FRAME);
    assert!(!Path::new(&e.src).exists());
}

/// B8: every answer is typed and recorded the moment the undo gives it.
#[test]
fn every_answer_of_the_search_is_typed_and_recorded() {
    use std::os::unix::fs::PermissionsExt;
    type Stage = fn(&Fx, &Path);
    let cases: [(SoughtFinding, Stage); 6] = [
        (SoughtFinding::Found, |_, _| {}),
        (SoughtFinding::NotFound, |_, now| {
            fs::remove_file(now).unwrap()
        }),
        (SoughtFinding::Ambiguous, |_, now| {
            fs::hard_link(now, now.with_file_name("twin.bmp")).unwrap()
        }),
        (SoughtFinding::Candidate, |_, now| {
            std::thread::sleep(std::time::Duration::from_millis(20));
            fs::OpenOptions::new()
                .append(true)
                .open(now)
                .unwrap()
                .write_all(b"!")
                .unwrap();
        }),
        (SoughtFinding::ProtectedSkipped, |_, now| {
            fs::create_dir(now.with_file_name("X.photoslibrary")).unwrap()
        }),
        (SoughtFinding::Uncertain, |f, _| {
            let d = q(&f.archive.join("closed"));
            fs::create_dir_all(&d).unwrap();
            fs::set_permissions(&d, fs::Permissions::from_mode(0o000)).unwrap();
        }),
    ];
    use std::io::Write;
    for (want, stage) in cases {
        let f = fx();
        let e = f.quarantined("2015", "frame.bmp", false);
        let moved = f.renamed("2015", "renamed");
        let now = q(&moved).join("frame.bmp");
        stage(&f, &now);
        let closed = q(&f.archive.join("closed"));
        if want == SoughtFinding::Uncertain && fs::read_dir(&closed).is_ok() {
            continue; // privileges that read anything
        }
        let (sought, _) = undo_reports(&f, e.id);
        if closed.exists() {
            fs::set_permissions(&closed, fs::Permissions::from_mode(0o700)).unwrap();
        }
        assert_eq!(sought[0].finding, want, "{sought:?}");
        assert!(!Path::new(&e.src).exists(), "{want:?}: moved home");
    }
}
