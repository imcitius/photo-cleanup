//! The command line as it is actually used: a process, with arguments and
//! exit codes, not a library call.
//!
//! The library behind it is tested elsewhere. What these check is the part
//! only a process can be wrong about — which arguments carry out the work,
//! what is refused, and whether the archive looks right afterwards.

use std::path::{Path, PathBuf};
use std::process::Output;

#[path = "../../pc-core/src/derived/fixtures.rs"]
mod junk;

struct Cli {
    _tmp: tempfile::TempDir,
    db: PathBuf,
    archive: PathBuf,
}

impl Cli {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let archive = root.join("archive");
        std::fs::create_dir_all(&archive).unwrap();
        Self {
            db: root.join("test.db"),
            archive,
            _tmp: tmp,
        }
    }
    fn run(&self, args: &[&str]) -> Output {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_photo-cleanup"))
            .arg("--db")
            .arg(&self.db)
            .args(args)
            .output()
            .expect("не запустить photo-cleanup");
        assert!(
            out.status.success(),
            "{args:?} завершилась с ошибкой:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }
    fn said(&self, args: &[&str]) -> String {
        String::from_utf8_lossy(&self.run(args).stdout).into_owned()
    }
    fn photo(&self, name: &str, shade: u8) -> PathBuf {
        let img = image::RgbImage::from_fn(240, 180, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, shade])
        });
        let path = self.archive.join(name);
        let mut encoded = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut encoded)
            .encode_image(&img)
            .unwrap();
        std::fs::write(&path, &encoded).unwrap();
        path
    }
}

fn quarantined(dir: &Path) -> Vec<String> {
    let hidden = dir.join(pc_core::QUARANTINE_DIR);
    let Ok(entries) = std::fs::read_dir(hidden) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Put a scanned bundle into quarantine as a version before el-126jk did:
/// the same journal entry, the same rename beside it. This version moves
/// nothing of Lightroom's, but what an earlier one moved is still there,
/// and purge and undo have to answer for it.
fn moved_by_an_earlier_version(cli: &Cli, rel: &str) -> PathBuf {
    let src = cli.archive.join(rel);
    let src_s = src.display().to_string();
    let db = pc_db::Db::open(&cli.db).unwrap();
    let b = db
        .list_bundles(&Default::default())
        .unwrap()
        .into_iter()
        .find(|b| b.path == src_s)
        .unwrap_or_else(|| panic!("{rel} was not scanned"));
    let hidden = cli.archive.join(pc_core::QUARANTINE_DIR);
    std::fs::create_dir_all(&hidden).unwrap();
    let dst = hidden.join(src.file_name().unwrap());
    let manifest = [pc_db::Moved {
        src: src_s.clone(),
        dst: dst.display().to_string(),
        proof: pc_core::proof::Proof::of(&std::fs::symlink_metadata(&src).unwrap()),
    }];
    let run = db.latest_run().unwrap().unwrap();
    let jid = db
        .journal_begin(&pc_db::NewJournalEntry {
            run_id: run,
            op: "quarantine",
            target_id: Some(b.id),
            src: &src_s,
            dst: Some(&manifest[0].dst),
            size: b.size,
            file_count: b.file_count,
            manifest: &manifest,
        })
        .unwrap();
    std::fs::rename(&src, &dst).unwrap();
    db.journal_close(
        jid,
        pc_db::JournalStatus::Done,
        &pc_db::Event {
            moved: &manifest,
            ..pc_db::Event::new("forward", "done")
        },
    )
    .unwrap();
    db.set_bundle_state(b.id, pc_db::BundleState::Quarantined)
        .unwrap();
    dst
}

#[test]
fn a_copy_goes_to_quarantine_comes_back_and_only_then_can_be_deleted() {
    let cli = Cli::new();
    let original = cli.photo("frame.jpg", 90);
    let copy = cli.photo("frame copy.jpg", 90);
    std::fs::write(cli.archive.join("frame.xmp"), b"edits").unwrap();

    cli.run(&[
        "index",
        "--root",
        cli.archive.to_str().unwrap(),
        "--min-size",
        "0",
    ]);
    cli.run(&["families", "build"]);

    // Looking is not doing: `plan` names the file and leaves the disk alone.
    let plan = cli.said(&["plan"]);
    assert!(plan.contains("frame copy.jpg"), "{plan}");
    assert!(copy.exists(), "план тронул диск");

    let applied = cli.said(&["apply", "--yes"]);
    assert!(
        applied.contains("computed just now"),
        "не сказано, что план посчитан сейчас: {applied}"
    );
    assert!(!copy.exists(), "копия не уехала");
    assert!(original.exists(), "уехал оригинал");
    assert_eq!(
        quarantined(&cli.archive),
        vec!["frame copy.jpg".to_string()]
    );

    // The journal brings it back, and the sidecar of the kept frame is
    // untouched by all of this.
    let entry = {
        let db = pc_db::Db::open(&cli.db).unwrap();
        db.journal_quarantined(None).unwrap().pop().unwrap().id
    };
    cli.run(&["derived", "undo", "--journal", &entry.to_string()]);
    assert!(copy.exists(), "файл не вернулся");
    assert!(cli.archive.join("frame.xmp").exists());
}

#[test]
fn a_taken_place_keeps_the_file_without_a_terminal_and_on_conflict_decides_it() {
    // el-14vx0. Another program puts a file where the copy belongs. Without
    // a terminal to ask, the copy stays in quarantine and the command says
    // where it is; `--on-conflict rename-returning` brings it back beside
    // the newcomer, which nothing touches.
    let cli = Cli::new();
    cli.photo("frame.jpg", 90);
    let copy = cli.photo("frame copy.jpg", 90);
    let ours = std::fs::read(&copy).unwrap();
    cli.run(&[
        "index",
        "--root",
        cli.archive.to_str().unwrap(),
        "--min-size",
        "0",
    ]);
    cli.run(&["families", "build"]);
    cli.run(&["apply", "--yes"]);
    assert!(!copy.exists());
    std::fs::write(&copy, b"a newer file under the same name").unwrap();
    let entry = {
        let db = pc_db::Db::open(&cli.db).unwrap();
        db.journal_quarantined(None).unwrap().pop().unwrap().id
    };
    let id = entry.to_string();

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_photo-cleanup"))
        .arg("--db")
        .arg(&cli.db)
        .args(["derived", "undo", "--journal", &id])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success(), "a kept file is not a finished undo");
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(said.contains("kept in quarantine"), "{said}");
    assert!(said.contains(pc_core::QUARANTINE_DIR), "{said}");
    assert_eq!(
        std::fs::read(&copy).unwrap(),
        b"a newer file under the same name"
    );
    assert_eq!(
        quarantined(&cli.archive),
        vec!["frame copy.jpg".to_string()]
    );

    let said = cli.said(&[
        "derived",
        "undo",
        "--journal",
        &id,
        "--on-conflict",
        "rename-returning",
    ]);
    assert!(said.contains("rename-returning"), "{said}");
    assert_eq!(
        std::fs::read(&copy).unwrap(),
        b"a newer file under the same name"
    );
    assert_eq!(
        std::fs::read(cli.archive.join("frame copy_1.jpg")).unwrap(),
        ours
    );
    assert!(quarantined(&cli.archive).is_empty());
}

#[test]
fn deleting_for_good_needs_the_flag_and_the_holding_period() {
    let cli = Cli::new();
    cli.photo("frame.jpg", 90);
    cli.photo("frame copy.jpg", 90);
    cli.run(&[
        "index",
        "--root",
        cli.archive.to_str().unwrap(),
        "--min-size",
        "0",
    ]);
    cli.run(&["families", "build"]);
    cli.run(&["apply", "--yes"]);

    // Still inside the holding period: nothing is offered.
    let held = cli.said(&["derived", "purge", "--older-than", "7d", "--yes"]);
    assert!(held.contains("Nothing to delete"), "{held}");
    assert_eq!(quarantined(&cli.archive).len(), 1);

    // Past it, but without the flag: listed, not carried out.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let listed = cli.said(&["derived", "purge", "--older-than", "0d"]);
    assert!(listed.contains("cannot be undone"), "{listed}");
    assert_eq!(quarantined(&cli.archive).len(), 1, "удалено без --yes");

    let done = cli.said(&["derived", "purge", "--older-than", "0d", "--yes"]);
    assert!(done.contains("Deleted"), "{done}");
    assert!(quarantined(&cli.archive).is_empty(), "байты остались");
}

#[test]
fn previews_stay_whether_their_catalogue_is_open_or_closed() {
    // The command line once went straight past the open-catalogue check the
    // web made. Now there is nothing to check: nothing of Lightroom's moves
    // (el-126jk), open or closed, and the command says why.
    let cli = Cli::new();
    let previews = cli.archive.join("Library Previews.lrdata");
    std::fs::create_dir_all(&previews).unwrap();
    std::fs::write(previews.join("cache"), vec![b'x'; 4096]).unwrap();

    cli.run(&["scan", "--root", cli.archive.to_str().unwrap()]);
    for lock in [false, true] {
        if lock {
            std::fs::write(cli.archive.join("Library.lrcat.lock"), b"open").unwrap();
        }
        let said = cli.said(&["derived", "clean", "--kind", "lr-previews", "--yes"]);
        assert!(previews.join("cache").exists(), "previews moved: {said}");
        assert!(said.contains("Lightroom is never touched"), "{said}");
        assert!(said.contains("Nothing matches"), "{said}");
    }
    assert!(quarantined(&cli.archive).is_empty());
}

/// B3 (el-1y8uo), the command-line half of
/// `pc-api::recovery_tests::a_database_failure_after_a_move_keeps_the_receipt_in_the_job`:
/// the journal refuses the second photograph's row after the first has
/// moved. The command fails, and prints the receipt of the move that did
/// happen — the same summary the web's job carries.
#[test]
fn a_database_failure_after_a_move_prints_the_receipt() {
    let cli = Cli::new();
    let one = cli.photo("one.jpg", 40);
    cli.photo("two.jpg", 200);
    cli.run(&[
        "index",
        "--root",
        cli.archive.to_str().unwrap(),
        "--min-size",
        "0",
    ]);
    let dest = cli.archive.join("new");
    std::fs::create_dir(&dest).unwrap();
    {
        let db = pc_db::Db::open(&cli.db).unwrap();
        db.conn
            .execute_batch(
                "CREATE TRIGGER fail_second BEFORE INSERT ON journal \
                 WHEN (SELECT count(*) FROM journal WHERE op='organize') >= 1 \
                 BEGIN SELECT RAISE(FAIL,'review second journal failure'); END;",
            )
            .unwrap();
    }
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_photo-cleanup"))
        .arg("--db")
        .arg(&cli.db)
        .args(["organize", "apply", "--root"])
        .arg(&dest)
        .args(["--allow-duplicates", "--yes"])
        .output()
        .unwrap();
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "{text}");
    let moved: Vec<PathBuf> = walkdir::WalkDir::new(&dest)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .collect();
    assert_eq!(moved.len(), 1, "{moved:?}\n{text}");
    let size = std::fs::metadata(&moved[0]).unwrap().len();
    let receipt = pc_apply::Tally {
        frames: 1,
        bytes: size,
        ..Default::default()
    }
    .summary();
    assert!(
        text.contains(&format!("Moved before the stop: {receipt}")),
        "the command line lost the receipt the web keeps: {text}"
    );
    assert!(text.contains("review second journal failure"), "{text}");
    let _ = one;
}

#[test]
fn reviewer_photo_in_named_bundle_is_not_regenerable_data() {
    // Before el-126jk this photograph went to quarantine with the bundle
    // and only purge's allowlist kept it. Now it never leaves.
    let cli = Cli::new();
    let bundle = cli.archive.join("Cat Previews.lrdata");
    std::fs::create_dir_all(&bundle).unwrap();
    let original = cli.photo("Cat Previews.lrdata/original.jpg", 37);
    let bytes = std::fs::read(&original).unwrap();
    cli.run(&["scan", "--root", cli.archive.to_str().unwrap()]);
    let cleaned = cli.said(&["derived", "clean", "--kind", "lr-previews", "--yes"]);
    assert_eq!(std::fs::read(&original).unwrap(), bytes, "{cleaned}");
    assert!(quarantined(&cli.archive).is_empty(), "{cleaned}");
}

#[test]
fn reviewer_missing_purge_confirmation_leaves_the_fixture_intact() {
    let cli = Cli::new();
    let bundle = cli.archive.join("Cat Previews.lrdata");
    std::fs::create_dir_all(&bundle).unwrap();
    std::fs::write(bundle.join("cache.lrprev"), b"synthetic cache").unwrap();
    cli.run(&["scan", "--root", cli.archive.to_str().unwrap()]);
    let at = moved_by_an_earlier_version(&cli, "Cat Previews.lrdata").join("cache.lrprev");
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let out = cli.said(&["derived", "purge", "--older-than", "0d"]);
    assert!(out.contains("cannot be undone"), "{out}");
    assert_eq!(std::fs::read(at).unwrap(), b"synthetic cache");
}

/// el-8s63g B2, the command-line half of
/// `pc-api::reviewer_web_finalization_failure_keeps_actual_counts`: the
/// journal refuses the end of a purge after the file is gone. The command
/// prints what was deleted, and then fails. (A photograph's copy now: purge
/// deletes no bundle, el-3s9kp.)
#[test]
fn a_journal_failure_after_purge_prints_what_went_and_fails() {
    let cli = Cli::new();
    cli.photo("frame.jpg", 90);
    let copy = cli.photo("frame copy.jpg", 90);
    let size = std::fs::metadata(&copy).unwrap().len();
    cli.run(&[
        "index",
        "--root",
        cli.archive.to_str().unwrap(),
        "--min-size",
        "0",
    ]);
    cli.run(&["families", "build"]);
    cli.run(&["apply", "--yes"]);
    let at = cli
        .archive
        .join(pc_core::QUARANTINE_DIR)
        .join("frame copy.jpg");
    assert!(at.exists());
    rusqlite::Connection::open(&cli.db)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER fail_purge_close BEFORE UPDATE OF status ON journal \
             WHEN NEW.status='purged' BEGIN SELECT RAISE(FAIL, 'synthetic journal failure'); END;",
        )
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_photo-cleanup"))
        .arg("--db")
        .arg(&cli.db)
        .args(["derived", "purge", "--older-than", "0d", "--yes"])
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&out.stdout);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{said}\n{err}");
    assert!(!at.exists());
    let receipt = format!("Deleted: 1 object, 1 file, {}", pc_core::fmt_bytes(size));
    assert!(said.contains(&receipt), "the receipt is missing: {said}");
    assert!(said.contains("synthetic journal failure"), "{said}");
    assert!(err.contains("stopped after deleting"), "{err}");
}

/// el-wffu8 B1-R2b through the real command line, under the contract
/// narrowed after el-2rpxq: system junk is never moved, so never deleted
/// for good either. `scan`, `derived clean`, `derived purge` as a person
/// runs them; each file is still at home, byte for byte and inode for
/// inode, nothing reaches a quarantine, and the command says why.
#[cfg(unix)]
#[test]
fn junk_without_a_reliable_signature_survives_scan_clean_and_purge() {
    let cli = Cli::new();
    let fixtures = [
        ("desktop.ini", junk::desktop_ini_text()),
        ("Thumbs.db", junk::thumbs_db()),
        (".DS_Store", junk::ds_store()),
    ];
    for (name, bytes) in &fixtures {
        std::fs::write(cli.archive.join(name), bytes).unwrap();
    }
    let prints: Vec<_> = fixtures
        .iter()
        .map(|(n, _)| fingerprint(&cli.archive.join(n)))
        .collect();
    cli.run(&["scan", "--root", cli.archive.to_str().unwrap()]);
    let cleaned = cli.said(&["derived", "clean", "--kind", "system-junk", "--yes"]);
    assert!(cleaned.contains("moves no system files"), "{cleaned}");
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let said = cli.said(&["derived", "purge", "--older-than", "0d", "--yes"]);
    for ((name, _), before) in fixtures.iter().zip(&prints) {
        assert_eq!(
            &fingerprint(&cli.archive.join(name)),
            before,
            "{name}: {cleaned}\n{said}"
        );
    }
    assert!(quarantined(&cli.archive).is_empty(), "{cleaned}\n{said}");
}

/// Everything that says a file is the same file, untouched: where it lives,
/// what it is, who may read it, and every byte.
#[cfg(unix)]
fn fingerprint(p: &Path) -> (u64, u64, u32, u32, u32, u64, u64, i64, i64, Vec<u8>) {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::symlink_metadata(p).unwrap();
    (
        m.dev(),
        m.ino(),
        m.mode(),
        m.uid(),
        m.gid(),
        m.nlink(),
        m.len(),
        m.mtime(),
        m.mtime_nsec(),
        std::fs::read(p).unwrap(),
    )
}

/// el-5gr1y B1-R3 and the user's decision of 2026-10-06 ("Lightroom
/// catalogues are not to be touched at all"), through the real command
/// line: `scan`, `derived clean`, `derived purge --yes` as a person runs
/// them. Whatever a Lightroom bundle held when it moved — a catalogue under
/// a `.db` name, this tool's own database, SQLite named `Thumbs.db`, a
/// 24-byte fragment, or the protected names that were already refused —
/// purge deletes nothing of it, and says it is kept for a person to delete
/// by hand, naming where it is and how big.
#[cfg(unix)]
#[test]
fn lightroom_bundles_survive_purge_whatever_they_hold_and_are_reported_kept() {
    // A synthetic catalogue: tables named as Lightroom names them, rows
    // that stand for edits nobody can rebuild.
    let seed = tempfile::tempdir().unwrap();
    let catalog = seed.path().join("source.lrcat");
    rusqlite::Connection::open(&catalog)
        .unwrap()
        .execute_batch(
            "CREATE TABLE AgLibraryFile (id_local INTEGER PRIMARY KEY, baseName TEXT);
             INSERT INTO AgLibraryFile VALUES (1, 'synthetic-frame');
             CREATE TABLE Adobe_images (id_local INTEGER PRIMARY KEY, developSettings TEXT);
             INSERT INTO Adobe_images VALUES (1, 'synthetic irreplaceable edit');",
        )
        .unwrap();
    let catalog = std::fs::read(&catalog).unwrap();
    // This tool's own database, made by this tool from a synthetic scan.
    let state = {
        let cli = Cli::new();
        std::fs::write(cli.archive.join("desktop.ini"), b"synthetic state").unwrap();
        cli.run(&["scan", "--root", cli.archive.to_str().unwrap()]);
        std::fs::read(&cli.db).unwrap()
    };
    let cells: [(&str, &str, &str, Vec<u8>); 8] = [
        (
            "lr-previews",
            "Library Previews.lrdata",
            "catalog.db",
            catalog.clone(),
        ),
        (
            "lr-helper",
            "Library Helper.lrdata",
            "catalog.db",
            catalog.clone(),
        ),
        (
            "lr-previews",
            "Library Previews.lrdata",
            "photo-cleanup.db",
            state,
        ),
        (
            "lr-previews",
            "Library Previews.lrdata",
            "Thumbs.db",
            catalog.clone(),
        ),
        (
            "lr-previews",
            "Library Previews.lrdata",
            "unknown.db",
            catalog[..24].to_vec(),
        ),
        (
            "lr-previews",
            "Library Previews.lrdata",
            "catalog.lrcat",
            catalog.clone(),
        ),
        (
            "lr-previews",
            "Library Previews.lrdata",
            "Library.lrcat-data/data.db",
            catalog.clone(),
        ),
        (
            "lr-previews",
            "Library Previews.lrdata",
            "Library.photoslibrary/database/Photos.db",
            catalog.clone(),
        ),
    ];
    // Every cell runs, and the failure names each that failed.
    let mut failed = Vec::new();
    for (kind, bundle, name, payload) in cells {
        let cell = format!("{bundle}/{name}");
        let run = std::panic::catch_unwind(|| lightroom_cell(kind, bundle, name, &payload));
        if run.is_err() {
            failed.push(cell);
        }
    }
    assert!(failed.is_empty(), "failed cells: {failed:?}");
}

#[cfg(unix)]
fn lightroom_cell(kind: &str, bundle: &str, name: &str, payload: &[u8]) {
    let cell = format!("{bundle}/{name}");
    {
        let cli = Cli::new();
        let at = cli.archive.join(bundle).join(name);
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::fs::write(&at, payload).unwrap();
        let mut held = vec![name.to_string()];
        if kind == "lr-previews" {
            std::fs::write(
                cli.archive.join(bundle).join("innocent.lrprev"),
                b"AgHg disposable synthetic preview",
            )
            .unwrap();
            held.push("innocent.lrprev".into());
        }
        cli.run(&["scan", "--root", cli.archive.to_str().unwrap()]);
        // This version moves none of it (el-126jk)...
        let cleaned = cli.said(&["derived", "clean", "--kind", kind, "--yes"]);
        assert!(
            cli.archive.join(bundle).join(name).exists(),
            "{cell}: moved: {cleaned}"
        );
        assert!(
            cleaned.contains("Lightroom is never touched"),
            "{cell}: {cleaned}"
        );
        // ...but what an earlier version moved is still kept by purge.
        let q = moved_by_an_earlier_version(&cli, bundle);
        assert!(q.join(name).exists(), "{cell}: not in quarantine");
        let before: Vec<_> = held.iter().map(|h| fingerprint(&q.join(h))).collect();
        let bundle_before = fingerprint_dir(&q);
        std::thread::sleep(std::time::Duration::from_millis(1100));

        let said = cli.said(&["derived", "purge", "--older-than", "0d", "--yes"]);

        for h in &held {
            assert!(q.join(h).exists(), "{cell}: {h} was deleted: {said}");
        }
        let after: Vec<_> = held.iter().map(|h| fingerprint(&q.join(h))).collect();
        assert!(before == after, "{cell}: something was changed: {said}");
        assert_eq!(bundle_before, fingerprint_dir(&q), "{cell}: {said}");
        assert!(said.contains("Deleted: 0 objects"), "{cell}: {said}");
        assert!(
            said.contains("kept — delete it by hand if you are sure"),
            "{cell}: {said}"
        );
        assert!(said.contains(&q.display().to_string()), "{cell}: {said}");
        // Still in quarantine, still undoable, and its history says why.
        let db = pc_db::Db::open(&cli.db).unwrap();
        let e = db.journal_quarantined(None).unwrap().pop().unwrap();
        assert_eq!(e.status, pc_db::JournalStatus::Done, "{cell}");
        let events: Vec<_> = db
            .journal_events(e.id)
            .unwrap()
            .into_iter()
            .map(|ev| (ev.phase, ev.kind))
            .collect();
        assert!(
            events.contains(&("purge".into(), "kept".into())),
            "{cell}: {events:?}"
        );
    }
}

/// Every entry below `dir`, with its kind and identity: nothing added,
/// nothing taken away.
#[cfg(unix)]
fn fingerprint_dir(dir: &Path) -> Vec<(PathBuf, u64, u32)> {
    use std::os::unix::fs::MetadataExt;
    let mut out: Vec<_> = walkdir::WalkDir::new(dir)
        .into_iter()
        .map(|e| {
            let e = e.unwrap();
            let m = e.path().symlink_metadata().unwrap();
            (e.path().to_path_buf(), m.ino(), m.mode())
        })
        .collect();
    out.sort();
    out
}

/// el-126jk, the user's decision of 2026-10-06 ("Lightroom catalogues are
/// not to be touched at all"), through the real command line: `scan`, then
/// `derived clean --yes` for every kind as a person runs it. Nothing of
/// Lightroom moves — in any case, in either Unicode normal form, under any
/// ancestor — and neither does a photograph inside a folder that is only
/// called derived. Each refusal is said, with its reason. Since el-2rpxq
/// no system file moves either, a well-formed `.DS_Store` included.
#[test]
fn derived_clean_moves_nothing_of_lightroom_and_no_photograph_in_a_named_bundle() {
    let cli = Cli::new();
    let a = &cli.archive;
    let put = |rel: &str, bytes: &[u8]| {
        let p = a.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, bytes).unwrap();
        p
    };
    let jpeg = std::fs::read(cli.photo("seed.jpg", 61)).unwrap();
    std::fs::remove_file(a.join("seed.jpg")).unwrap();
    let ds_store = junk::ds_store();
    // "Ёлка" precomposed (NFC) and "Йод" decomposed (NFD: И + U+0306).
    let nfc = "\u{401}\u{43b}\u{43a}\u{430}";
    let nfd = "\u{418}\u{306}\u{43e}\u{434}";
    let kept = [
        put("Cat Previews.lrdata/root-pyramid.lrprev", b"AgHg synthetic"),
        put("Cat Previews.lrdata/previews.db", b"SQLite format 3\0syn"),
        put("Cat Smart Previews.lrdata/A/frame.dng", b"II*\0synthetic"),
        put("Cat Helper.lrdata/helper.db", b"SQLite format 3\0syn"),
        put("Other.lrdata/x.bin", b"synthetic"),
        put("X.lrcat-data/masks.db", b"SQLite format 3\0syn"),
        put("PREVIEWS.LRDATA/upper.lrprev", b"AgHg synthetic"),
        put("Mixed Previews.LrData/m.lrprev", b"AgHg synthetic"),
        put("Shout.LRCAT-DATA/m.db", b"SQLite format 3\0syn"),
        put(&format!("{nfc} Previews.lrdata/c.lrprev"), b"AgHg nfc"),
        put(&format!("{nfd} Previews.lrdata/d.lrprev"), b"AgHg nfd"),
        // Junk inside anything of Lightroom's stays with it.
        put("Lightroom/Backups/2026-01-01 1200/.DS_Store", &ds_store),
        put("Cat Previews.lrdata/.DS_Store", &ds_store),
        put("._Cat.lrcat", b"\0\x05\x16\x07synthetic appledouble"),
        // A photograph in a folder called derived is a photograph.
        put("@eaDir/IMG_0001.JPG/real.jpg", &jpeg),
        put(".thumbnails/mystery.bin", b"unknown synthetic"),
        // A photograph under a junk file name is a photograph.
        put("desktop.ini", &jpeg),
    ];
    let junk = put(".DS_Store", &ds_store);
    let junk_print = fingerprint(&junk);

    cli.run(&["scan", "--root", a.to_str().unwrap()]);
    let mut said = String::new();
    for kind in [
        "lr-previews",
        "lr-smart-previews",
        "lr-helper",
        "lr-lrdata-other",
        "system-junk",
    ] {
        let out = cli.run(&["derived", "clean", "--kind", kind, "--yes"]);
        said.push_str(&String::from_utf8_lossy(&out.stdout));
        said.push_str(&String::from_utf8_lossy(&out.stderr));
    }
    let moved: Vec<_> = kept.iter().filter(|p| !p.exists()).collect();
    assert!(moved.is_empty(), "moved: {moved:?}\n{said}");
    assert_eq!(fingerprint(&junk), junk_print, "{said}");
    assert!(quarantined(a).is_empty(), "{said}");
    assert!(said.contains("Lightroom is never touched"), "{said}");
    assert!(said.contains("moves no system files"), "{said}");
}

/// el-wda81, the independent review of 9b785bf, through the real command
/// line: scan, then `derived clean --yes`. A protected folder anywhere below
/// a junk folder (B1), a satellite whose file is there — including one that
/// arrived after the scan (B2), and a magic prefix or broken text (B3) all
/// stay where they are. Since the contract narrowed after el-2rpxq so do
/// the orphans and complete structures that used to move: no system file
/// and no companion is ever moved, and the command says why.
#[cfg(unix)]
#[test]
fn reviewer_el_wda81_matrix_keeps_protected_satellites_and_prefixes() {
    let cli = Cli::new();
    let a = &cli.archive;
    let put = |rel: &str, bytes: &[u8]| {
        let p = a.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, bytes).unwrap();
        p
    };
    let ds = junk::ds_store();
    let ad = junk::apple_double();
    let jpeg = std::fs::read(cli.photo("seed.jpg", 17)).unwrap();
    std::fs::remove_file(a.join("seed.jpg")).unwrap();
    let mut kept = vec![
        // B1
        put("n1/@eaDir/X.lrcat-data/.DS_Store", &ds),
        put("n2/@eaDir/Cat.lrdata/.DS_Store", &ds),
        put("n3/@eaDir/Backups/.DS_Store", &ds),
        put("n4/@eaDir/Library.photoslibrary/.DS_Store", &ds),
        put("n5/Backups. /.DS_Store", &ds),
        put(
            "n6/.thumbnails/deep/er/Old Lightroom Catalogs/.DS_Store",
            &ds,
        ),
        // B2
        put("s1/photo.png", &jpeg),
        put("s1/._photo.png", &ad),
        put("s2/photo.xmp", b"<x:xmpmeta/>"),
        put("s2/._photo.xmp", &ad),
        put("s3/photo.png", &jpeg),
        put("s3/@eaDir/photo.png/photo.png@SynoEAStream", &ad),
        // B3
        put("m1/._orphan", &ad[..4]),
        put("m2/.DS_Store", &ds[..8]),
        put("m3/Thumbs.db", &junk::thumbs_db()[..8]),
    ];
    let mut bad_utf16 = vec![0xFF, 0xFE];
    for _ in 0..8 {
        bad_utf16.extend_from_slice(&[0x00, 0xD8]);
    }
    kept.push(put("m4/desktop.ini", &bad_utf16));
    let empty_lr = a.join("e1/@eaDir/X.lrcat-data");
    let empty_photos = a.join("e2/.thumbnails/Library.photoslibrary");
    std::fs::create_dir_all(&empty_lr).unwrap();
    std::fs::create_dir_all(&empty_photos).unwrap();
    let late_sat = put("late/._frame.png", &ad);
    let controls = [
        put("ok/._orphan", &ad),
        put("ok/.DS_Store", &ds),
        put("ok/Thumbs.db", &junk::thumbs_db()),
        put("ok/desktop.ini", &junk::desktop_ini_utf16()),
    ];

    cli.run(&["scan", "--root", a.to_str().unwrap()]);
    // The counterpart arrives after the scan: the move must see it.
    let late = put("late/frame.png", &jpeg);
    let prints: Vec<_> = kept
        .iter()
        .chain(&controls)
        .chain([&late_sat, &late])
        .map(|p| (p.clone(), fingerprint(p)))
        .collect();
    let dry = cli.said(&["derived", "clean", "--dry-run"]);
    let out = cli.run(&["derived", "clean", "--yes"]);
    let said = format!(
        "{dry}\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    for (p, before) in &prints {
        assert!(p.exists(), "{} moved:\n{said}", p.display());
        assert_eq!(&fingerprint(p), before, "{} changed", p.display());
    }
    assert!(empty_lr.is_dir() && empty_photos.is_dir(), "{said}");
    assert!(quarantined(a).is_empty(), "{said}");
    assert!(!said.contains("Moved:"), "{said}");
    assert!(said.contains("moves no system files"), "{said}");
}

/// el-2rpxq, the independent review of 9759581, and the contract the
/// director narrowed after it: `derived clean` moves no system file and no
/// companion, through the real command line. The reviewer's malformed
/// `Thumbs.db` and `.DS_Store` (B3-R2), an AppleDouble file whose frame is
/// spelled in the other Unicode normal form (B2-R2), and — under the
/// narrowed contract — every well-formed control too: an orphan `._*`, a
/// Synology stream, `@eaDir`, `.thumbnails`, `Thumbs.db`, `desktop.ini`,
/// `.DS_Store`. Each stays byte for byte and inode for inode, nothing
/// reaches a quarantine, and the command says why.
#[cfg(unix)]
#[test]
fn reviewer_el_2rpxq_no_system_file_or_companion_ever_moves() {
    let cli = Cli::new();
    let a = &cli.archive;
    let put = |rel: &str, bytes: &[u8]| {
        let p = a.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, bytes).unwrap();
        p
    };
    let ad = junk::apple_double();
    let ds = junk::ds_store();
    let jpeg = std::fs::read(cli.photo("seed.jpg", 23)).unwrap();
    std::fs::remove_file(a.join("seed.jpg")).unwrap();
    let kept = [
        // B3-R2
        put("b3cfb/Thumbs.db", &junk::thumbs_db_missing_mini_stream()),
        put("b3ds/.DS_Store", &junk::ds_store_impossible_node()),
        // B2-R2: NFC frame, NFD satellite.
        put("b2/caf\u{e9}.png", &jpeg),
        put("b2/._cafe\u{301}.png", &ad),
        // Well-formed: none of it moves either.
        put("v/.DS_Store", &ds),
        put("v/Thumbs.db", &junk::thumbs_db()),
        put("v/desktop.ini", &junk::desktop_ini_utf16()),
        put("v/._orphan", &ad),
        put("v/._.DS_Store", &ad),
        put("e/@eaDir/gone.png@SynoEAStream", &ad),
        put("e/@eaDir/gone.png@SynoResource", &ad),
        put("e/@eaDir/.DS_Store", &ds),
        put("t/.thumbnails/.DS_Store", &ds),
    ];
    let prints: Vec<_> = kept.iter().map(|p| (p.clone(), fingerprint(p))).collect();

    cli.run(&["scan", "--root", a.to_str().unwrap()]);
    let dry = cli.said(&["derived", "clean", "--dry-run"]);
    let yes = cli.said(&["derived", "clean", "--yes"]);
    let all = cli.said(&["derived", "clean", "--kind", "system-junk", "--yes"]);
    let said = format!("{dry}\n{yes}\n{all}");
    for (p, before) in &prints {
        assert!(p.exists(), "{} moved:\n{said}", p.display());
        assert_eq!(&fingerprint(p), before, "{} changed", p.display());
    }
    for dir in [
        "b3cfb",
        "b3ds",
        "b2",
        "v",
        "e",
        "t",
        "e/@eaDir",
        "t/.thumbnails",
    ] {
        assert!(quarantined(&a.join(dir)).is_empty(), "{dir}: {said}");
    }
    assert!(!said.contains("Moved:"), "{said}");
    assert!(said.contains("moves no system files"), "{said}");
}

/// el-14vx0 B5 (review el-4cmzu). Two sorted photographs whose original
/// names another program has taken since. The preview names both conflicts
/// and their choices before anything is carried out; on a real terminal
/// every question is asked before the first file moves — the first answer
/// moves nothing while the second question is still open.
#[cfg(unix)]
#[test]
fn every_conflict_is_shown_and_answered_before_the_first_file_moves() {
    use std::io::{Read, Write};
    let cli = Cli::new();
    let first = cli.photo("first.jpg", 40);
    let second = cli.photo("second.jpg", 200);
    cli.run(&[
        "index",
        "--root",
        cli.archive.to_str().unwrap(),
        "--min-size",
        "0",
    ]);
    let sorted = cli.archive.parent().unwrap().join("sorted");
    std::fs::create_dir(&sorted).unwrap();
    cli.run(&[
        "organize",
        "apply",
        "--root",
        sorted.to_str().unwrap(),
        "--allow-duplicates",
        "--yes",
    ]);
    assert!(!first.exists() && !second.exists());
    for p in [&first, &second] {
        std::fs::write(p, b"a foreign file under the same name").unwrap();
    }
    let sorted_files = || -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = walkdir::WalkDir::new(&sorted)
            .into_iter()
            .flatten()
            .filter(|e| e.file_type().is_file())
            .map(|e| e.into_path())
            .collect();
        v.sort();
        v
    };
    let held = sorted_files();
    assert_eq!(held.len(), 2, "{held:?}");

    // The preview: both conflicts, with what can be chosen for each.
    let preview = cli.said(&["organize", "undo"]);
    for p in [&first, &second] {
        assert!(preview.contains(p.to_str().unwrap()), "{preview}");
    }
    for word in ["keep", "replace", "rename-existing", "rename-returning"] {
        assert!(preview.contains(word), "{preview}");
    }
    assert_eq!(sorted_files(), held, "a preview moves nothing");

    // A real terminal, through script(1): the questions are answered one by
    // one, and the disk is looked at while the second is still open.
    let bin = env!("CARGO_BIN_EXE_photo-cleanup");
    let db = cli.db.to_str().unwrap();
    let mut cmd = if cfg!(target_os = "linux") {
        let mut c = std::process::Command::new("script");
        c.args([
            "-q",
            "-e",
            "-c",
            &format!("'{bin}' --db '{db}' organize undo --yes"),
            "/dev/null",
        ]);
        c
    } else {
        let mut c = std::process::Command::new("script");
        c.args([
            "-q",
            "/dev/null",
            bin,
            "--db",
            db,
            "organize",
            "undo",
            "--yes",
        ]);
        c
    };
    let mut child = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("script(1) runs the command on a terminal");
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let mut out = child.stdout.take().unwrap();
    let sink = seen.clone();
    let reader = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = out.read(&mut buf) {
            if n == 0 {
                break;
            }
            sink.lock().unwrap().extend_from_slice(&buf[..n]);
        }
    });
    let shown = || String::from_utf8_lossy(&seen.lock().unwrap()).into_owned();
    let wait_for = |n: usize| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while shown().matches("Choose 1-").count() < n {
            assert!(
                std::time::Instant::now() < deadline,
                "question {n} never came:\n{}",
                shown()
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    };
    let mut input = child.stdin.take().unwrap();
    wait_for(1);
    input.write_all(b"4\n").unwrap();
    input.flush().unwrap();
    wait_for(2);
    assert_eq!(
        sorted_files(),
        held,
        "a file moved before every question was answered:\n{}",
        shown()
    );
    for p in [&first, &second] {
        let renamed = p.with_file_name(format!(
            "{}_1.jpg",
            p.file_stem().unwrap().to_str().unwrap()
        ));
        assert!(!renamed.exists(), "{renamed:?} before the second answer");
    }
    input.write_all(b"1\n").unwrap();
    input.flush().unwrap();
    let status = child.wait().unwrap();
    drop(input);
    reader.join().unwrap();
    let said = shown();
    assert_eq!(said.matches("Choose 1-").count(), 2, "{said}");
    // One came back as *_1 beside the newcomer; the other stays held.
    let back: Vec<PathBuf> = [&first, &second]
        .iter()
        .map(|p| {
            p.with_file_name(format!(
                "{}_1.jpg",
                p.file_stem().unwrap().to_str().unwrap()
            ))
        })
        .filter(|p| p.exists())
        .collect();
    assert_eq!(back.len(), 1, "{said}");
    assert_eq!(sorted_files().len(), 1, "{said}");
    for p in [&first, &second] {
        assert_eq!(
            std::fs::read(p).unwrap(),
            b"a foreign file under the same name"
        );
    }
    let _ = status;
}
