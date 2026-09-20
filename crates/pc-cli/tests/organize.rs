//! Reorganisation end to end: dates in, a tree out, and the way back.
//!
//! The thing being guarded here is that a move is not a loss. A sidecar
//! keeps its photograph, two frames that share a Sony filename stay two
//! frames, the index keeps pointing at real paths, and one command puts the
//! whole archive back exactly as it was.

use std::fs;
use std::path::{Path, PathBuf};

use image::{DynamicImage, Rgb, RgbImage};
use pc_db::Db;

fn photo(w: u32, h: u32, seed: u32) -> DynamicImage {
    let s = seed as f32;
    let mut img = RgbImage::new(w, h);
    for (x, y, p) in img.enumerate_pixels_mut() {
        let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
        let tau = std::f32::consts::TAU;
        let a = ((fx * (2.0 + s * 0.7) + s * 0.3) * tau).sin();
        let b = ((fy * (1.6 + s * 0.4) + s * 0.8) * tau).cos();
        let v = |k: f32| ((128.0 + (a + b) * 45.0 * k).clamp(0.0, 255.0)) as u8;
        *p = Rgb([v(1.0), v(0.85), v(0.6)]);
    }
    DynamicImage::ImageRgb8(img)
}

fn write_jpeg(path: &Path, seed: u32) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let rgb = photo(1400, 1000, seed).to_rgb8();
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 92)
        .encode(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();
    fs::write(path, bytes).unwrap();
}

/// The only date some files have is when the bytes were last written.
fn set_mtime(path: &Path, ts: i64) {
    let f = fs::File::options().write(true).open(path).unwrap();
    let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(ts as u64);
    f.set_times(fs::FileTimes::new().set_modified(t)).unwrap();
}

/// 2019-07-14, morning and evening.
const MORNING: i64 = 1_563_096_000; // 09:20 UTC
const EVENING: i64 = 1_563_134_400; // 20:00 UTC

struct World {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    dest: PathBuf,
    db: Db,
}

fn build_world() -> World {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("foto");
    let dest = tmp.path().join("Архив");

    // Two different frames that a Sony numbered the same, in two folders.
    // They must both survive the move into one event.
    write_jpeg(&root.join("сброс/DSC01234.JPG"), 3);
    fs::write(root.join("сброс/DSC01234.xmp"), b"<x/>").unwrap();
    write_jpeg(&root.join("камера-2/DSC01234.JPG"), 17);
    set_mtime(&root.join("сброс/DSC01234.JPG"), MORNING);
    set_mtime(&root.join("камера-2/DSC01234.JPG"), MORNING + 600);

    // Same day, after a long gap: a second event.
    write_jpeg(&root.join("сброс/DSC09999.JPG"), 31);
    set_mtime(&root.join("сброс/DSC09999.JPG"), EVENING);

    // A folder that dates its contents to a month and no further.
    write_jpeg(&root.join("старое/2021/06/скан.jpg"), 41);
    set_mtime(&root.join("старое/2021/06/скан.jpg"), MORNING);

    let db = Db::open(&tmp.path().join("pc.db")).unwrap();
    let store = pc_core::ThumbStore::new(tmp.path().join("thumbs"));
    pc_cli::scan::run(&db, std::slice::from_ref(&root), "test").unwrap();
    pc_cli::index::run(
        &db,
        std::slice::from_ref(&root),
        &store,
        &pc_cli::index::Options {
            min_file_size: 1024,
            ..Default::default()
        },
    )
    .unwrap();

    World {
        _tmp: tmp,
        root,
        dest,
        db,
    }
}

fn options(dest: &Path) -> pc_organize::Options {
    fs::create_dir_all(dest).unwrap();
    pc_organize::Options {
        root: dest.to_path_buf(),
        ..Default::default()
    }
}

/// The destination, relative to the new tree and written with one separator.
/// These tests are about which folder a photograph lands in, not about which
/// slash the platform writes it with.
fn rel(dst: &str, dest: &Path) -> String {
    dst.strip_prefix(&*dest.to_string_lossy())
        .unwrap_or(dst)
        .replace('\\', "/")
        .trim_start_matches('/')
        .to_string()
}

#[test]
fn a_day_with_a_long_gap_becomes_two_events() {
    let w = build_world();
    let plan = pc_organize::compute(&w.db, &options(&w.dest)).unwrap();

    let dirs: Vec<String> = plan
        .moves
        .iter()
        .map(|m| rel(&m.dst, &w.dest).rsplit_once('/').unwrap().0.to_string())
        .collect();
    assert!(
        dirs.iter().any(|d| d == "2019/2019-07-14"),
        "утренняя съёмка не там: {dirs:?}"
    );
    assert!(
        dirs.iter().any(|d| d == "2019/2019-07-14_2000"),
        "вечерняя съёмка не выделена в своё событие: {dirs:?}"
    );
}

#[test]
fn a_folder_that_only_knows_the_month_does_not_get_a_day() {
    let w = build_world();
    let plan = pc_organize::compute(&w.db, &options(&w.dest)).unwrap();

    let m = plan
        .moves
        .iter()
        .find(|m| m.src.ends_with("скан.jpg"))
        .expect("файл из папки 2021/06 не попал в план");
    assert_eq!(
        rel(&m.dst, &w.dest),
        "2021/2021-06_без-точной-даты/скан.jpg"
    );
    assert!(
        m.date.uncertain(),
        "дата из пути должна считаться ненадёжной"
    );
}

#[test]
fn two_frames_with_one_sony_filename_both_survive() {
    let w = build_world();
    let plan = pc_organize::compute(&w.db, &options(&w.dest)).unwrap();

    let names: Vec<String> = plan
        .moves
        .iter()
        .filter(|m| m.src.ends_with("DSC01234.JPG"))
        .map(|m| m.name().to_string())
        .collect();
    assert_eq!(names.len(), 2, "оба кадра должны быть в плане");
    assert!(names.contains(&"DSC01234.JPG".to_string()), "{names:?}");
    assert!(names.contains(&"DSC01234_2.JPG".to_string()), "{names:?}");
    assert_eq!(plan.renamed, 1);
}

#[test]
fn applying_moves_the_files_the_sidecars_and_the_index() {
    let w = build_world();
    let plan = pc_organize::compute(&w.db, &options(&w.dest)).unwrap();
    let run_id = w.db.latest_run().unwrap().unwrap();
    let report = pc_apply::organize(&w.db, run_id, &plan.moves).unwrap();

    assert_eq!(report.refused.len(), 0, "отказы: {:?}", report.refused);
    assert_eq!(report.moved as usize, plan.moves.len());
    assert_eq!(report.sidecars, 1, "xmp не поехал со своим кадром");

    for m in &plan.moves {
        assert!(Path::new(&m.dst).is_file(), "нет файла {}", m.dst);
        assert!(!Path::new(&m.src).exists(), "остался на месте {}", m.src);
        let row =
            w.db.file(m.file_id)
                .unwrap()
                .expect("файл исчез из индекса");
        assert_eq!(row.path, m.dst, "индекс указывает не туда");
    }

    // The sidecar follows the stem of the photograph it belongs to, whatever
    // that stem ended up being.
    let owner = plan
        .moves
        .iter()
        .find(|m| m.src.replace('\\', "/").ends_with("сброс/DSC01234.JPG"))
        .unwrap();
    let side = Path::new(&owner.dst).with_extension("xmp");
    assert!(side.is_file(), "сайдкар потерялся: {}", side.display());

    // Directories the move emptied are not left behind as husks.
    assert!(
        !w.root.join("камера-2").exists(),
        "опустевший каталог остался"
    );
}

#[test]
fn undo_puts_every_file_back_where_it_was() {
    let w = build_world();
    let plan = pc_organize::compute(&w.db, &options(&w.dest)).unwrap();
    let before: Vec<(i64, String)> = plan
        .moves
        .iter()
        .map(|m| (m.file_id, m.src.clone()))
        .collect();

    let run_id = w.db.latest_run().unwrap().unwrap();
    pc_apply::organize(&w.db, run_id, &plan.moves).unwrap();
    let (back, failed) = pc_apply::undo_run(&w.db, run_id).unwrap();

    assert!(failed.is_empty(), "откат не удался: {failed:?}");
    assert_eq!(back as usize, plan.moves.len());
    for (id, src) in &before {
        assert!(Path::new(src).is_file(), "файл не вернулся: {src}");
        let row = w.db.file(*id).unwrap().unwrap();
        assert_eq!(&row.path, src, "индекс не вернулся к исходному пути");
    }
    assert!(
        w.root.join("сброс/DSC01234.xmp").is_file(),
        "сайдкар вернулся не под своим именем"
    );
}

#[test]
fn a_second_pass_has_nothing_left_to_do() {
    let w = build_world();
    let plan = pc_organize::compute(&w.db, &options(&w.dest)).unwrap();
    let run_id = w.db.latest_run().unwrap().unwrap();
    pc_apply::organize(&w.db, run_id, &plan.moves).unwrap();

    // The tree is now what the plan asks for, so planning again must be a
    // no-op rather than a second round of renames.
    let again = pc_organize::compute(&w.db, &options(&w.dest)).unwrap();
    assert!(again.moves.is_empty(), "повторный план: {:?}", again.moves);
    assert_eq!(again.already_placed, plan.moves.len());
}
