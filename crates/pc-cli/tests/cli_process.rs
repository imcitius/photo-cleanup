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
