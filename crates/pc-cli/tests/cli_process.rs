//! The command line as it is actually used: a process, with arguments and
//! exit codes, not a library call.
//!
//! The library behind it is tested elsewhere. What these check is the part
//! only a process can be wrong about — which arguments carry out the work,
//! what is refused, and whether the archive looks right afterwards.

use std::path::{Path, PathBuf};
use std::process::Output;

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
fn an_open_catalogue_between_scan_and_clean_stops_the_command_line_too() {
    // The web interface checks the catalogue again at the moment of the move.
    // The command line calls the core directly, and for a long time went
    // straight past that check.
    let cli = Cli::new();
    let previews = cli.archive.join("Library Previews.lrdata");
    std::fs::create_dir_all(&previews).unwrap();
    std::fs::write(previews.join("cache"), vec![b'x'; 4096]).unwrap();

    cli.run(&["scan", "--root", cli.archive.to_str().unwrap()]);
    std::fs::write(cli.archive.join("Library.lrcat.lock"), b"open").unwrap();

    let said = cli.said(&["derived", "clean", "--yes"]);
    assert!(
        previews.exists(),
        "превью уехали при открытом каталоге Lightroom"
    );
    assert!(
        said.contains("catalogue is open") || said.contains("каталог"),
        "отказ не объяснён: {said}"
    );
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
    let cli = Cli::new();
    let bundle = cli.archive.join("Cat Previews.lrdata");
    std::fs::create_dir_all(&bundle).unwrap();
    let original = cli.photo("Cat Previews.lrdata/original.jpg", 37);
    let bytes = std::fs::read(&original).unwrap();
    cli.run(&["scan", "--root", cli.archive.to_str().unwrap()]);
    let cleaned = cli.said(&["derived", "clean", "--yes"]);
    let at = cli
        .archive
        .join(pc_core::QUARANTINE_DIR)
        .join("Cat Previews.lrdata/original.jpg");
    assert!(at.exists(), "fixture did not reach quarantine: {cleaned}");
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let said = cli.said(&["derived", "purge", "--older-than", "0d", "--yes"]);
    assert!(
        at.exists(),
        "valid synthetic JPEG deleted as regenerable preview: {said}"
    );
    assert_eq!(std::fs::read(at).unwrap(), bytes);
}

#[test]
fn reviewer_missing_purge_confirmation_leaves_the_fixture_intact() {
    let cli = Cli::new();
    let bundle = cli.archive.join("Cat Previews.lrdata");
    std::fs::create_dir_all(&bundle).unwrap();
    std::fs::write(bundle.join("cache.lrprev"), b"synthetic cache").unwrap();
    cli.run(&["scan", "--root", cli.archive.to_str().unwrap()]);
    cli.run(&["derived", "clean", "--yes"]);
    let at = cli
        .archive
        .join(pc_core::QUARANTINE_DIR)
        .join("Cat Previews.lrdata/cache.lrprev");
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

/// el-wffu8 B1-R2b through the real command line: system junk whose format
/// has no reliable signature is never deleted for good, whatever its bytes
/// — not even when they start the way the real format starts. `scan`,
/// `derived clean`, `derived purge` as a person runs them; each payload is
/// still there, byte for byte, and the command says it is kept: purge
/// deletes no bundle at all (el-3s9kp), a lone junk file included.
#[test]
fn junk_without_a_reliable_signature_survives_scan_clean_and_purge() {
    let cli = Cli::new();
    let arbitrary: Vec<u8> = (0..=255).collect();
    let with_head = |head: &[u8]| {
        let mut v = head.to_vec();
        v.extend_from_slice(&arbitrary);
        v
    };
    let fixtures = [
        ("desktop.ini", arbitrary.clone()),
        (
            "Thumbs.db",
            with_head(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]),
        ),
        (".DS_Store", with_head(b"\0\0\0\x01Bud1")),
    ];
    for (name, bytes) in &fixtures {
        std::fs::write(cli.archive.join(name), bytes).unwrap();
    }
    cli.run(&["scan", "--root", cli.archive.to_str().unwrap()]);
    let cleaned = cli.said(&["derived", "clean", "--kind", "system-junk", "--yes"]);
    let q = cli.archive.join(pc_core::QUARANTINE_DIR);
    for (name, bytes) in &fixtures {
        let at = q.join(name);
        assert_eq!(
            std::fs::read(&at).ok().as_ref(),
            Some(bytes),
            "{name} did not reach quarantine: {cleaned}"
        );
    }
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let said = cli.said(&["derived", "purge", "--older-than", "0d", "--yes"]);
    for (name, bytes) in &fixtures {
        assert_eq!(
            std::fs::read(q.join(name)).ok().as_ref(),
            Some(bytes),
            "{name} was deleted for good: {said}"
        );
    }
    assert!(
        said.contains("kept — delete it by hand if you are sure"),
        "{said}"
    );
    assert!(said.contains("Deleted: 0 objects"), "{said}");
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
        let cleaned = cli.said(&["derived", "clean", "--kind", kind, "--yes"]);
        let q = cli.archive.join(pc_core::QUARANTINE_DIR).join(bundle);
        assert!(
            q.join(name).exists(),
            "{cell}: not in quarantine: {cleaned}"
        );
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
