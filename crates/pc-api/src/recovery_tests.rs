//! The web and the command line reach the same answer about recovery and
//! partial work, because both ask pc-apply (el-usdqi, diagnosis el-5vue3).
//!
//! The preview of an undo used to refuse any entry whose original path was
//! taken — including the frame a first undo had itself brought back, so the
//! retry the command line offered was closed in the browser. And a
//! photograph whose sidecar could not follow was refused whole in the
//! preview, while the command line moved it and reported the sidecar.

use super::*;

/// A photograph and its sidecar in the configured quarantine, journaled the
/// way an older version wrote it: no list, no evidence.
fn legacy_held(f: &Fixture) -> (PathBuf, PathBuf, i64) {
    let home = f.archive.join("frame.arw");
    let q = f.archive.join(pc_core::QUARANTINE_DIR);
    std::fs::create_dir(&q).unwrap();
    let held = q.join("frame.arw");
    std::fs::write(&held, b"our frame").unwrap();
    std::fs::write(held.with_extension("xmp"), b"our edits").unwrap();
    let db = f.state.db.lock().unwrap();
    let run = db
        .start_run(&[f.archive.display().to_string()], "test")
        .unwrap();
    let id = db
        .journal_begin(&pc_db::NewJournalEntry {
            run_id: run,
            op: "quarantine-file",
            target_id: None,
            src: &home.display().to_string(),
            dst: Some(&held.display().to_string()),
            size: 9,
            file_count: 1,
            manifest: &[],
        })
        .unwrap();
    db.journal_finish(id, pc_db::JournalStatus::Done, None)
        .unwrap();
    (home, held, id)
}

/// D9. The first undo brought the frame home and refused the sidecar: a
/// stranger had its name. The stranger is moved aside; the web offers the
/// retry the command line would carry out.
#[cfg(unix)]
#[tokio::test]
async fn diagnosis_api_offers_identity_proven_partial_undo() {
    let f = Fixture::new();
    let (home, _held, id) = legacy_held(&f);
    let side = home.with_extension("xmp");
    std::fs::write(&side, b"foreign edits").unwrap();
    {
        let db = f.state.db.lock().unwrap();
        assert!(pc_apply::undo(&db, id).is_err());
    }
    std::fs::rename(&side, f.archive.join("foreign-edits-retained.xmp")).unwrap();

    let preview = f.preview("journal-undo", json!({"journal_id":id})).await;
    assert_eq!(preview["items"].as_array().unwrap().len(), 1, "{preview}");
    let done = f.apply(&preview).await;
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!(std::fs::read(&side).unwrap(), b"our edits");
    assert_eq!(std::fs::read(&home).unwrap(), b"our frame");
}

/// The same row read by both: where the frame at home is the one the undo
/// brought back, both offer the retry; where it is someone else's file,
/// both refuse, and nothing moves.
#[cfg(unix)]
#[tokio::test]
async fn cli_and_api_share_recovery_classification() {
    let f = Fixture::new();
    let (home, held, id) = legacy_held(&f);
    let side = home.with_extension("xmp");
    std::fs::write(&side, b"foreign edits").unwrap();
    {
        let db = f.state.db.lock().unwrap();
        assert!(pc_apply::undo(&db, id).is_err());
    }
    std::fs::remove_file(&side).unwrap();
    // Someone else's file takes the frame's name at home.
    std::fs::rename(&home, f.archive.join("frame-moved-by-user.arw")).unwrap();
    std::fs::write(&home, b"someone else's frame").unwrap();

    let (status, web) = f
        .req(
            "POST",
            "/api/preview",
            json!({"kind":"journal-undo","params":{"journal_id":id}}),
        )
        .await;
    assert!(
        status != 200 || web["items"].as_array().unwrap().is_empty(),
        "{web}"
    );
    {
        let db = f.state.db.lock().unwrap();
        assert!(pc_apply::undo(&db, id).is_err());
    }
    assert_eq!(std::fs::read(&home).unwrap(), b"someone else's frame");
    assert_eq!(
        std::fs::read(held.with_extension("xmp")).unwrap(),
        b"our edits"
    );

    // The frame returns to its place: both offer the retry again.
    std::fs::remove_file(&home).unwrap();
    std::fs::rename(f.archive.join("frame-moved-by-user.arw"), &home).unwrap();
    let preview = f.preview("journal-undo", json!({"journal_id":id})).await;
    assert_eq!(preview["items"].as_array().unwrap().len(), 1, "{preview}");
}

/// A photograph whose sidecar's place in quarantine is taken: the command
/// line moves the photograph and reports the sidecar left at home; the web
/// does the same, and says so in the job, instead of refusing the frame.
#[tokio::test]
async fn a_successful_frame_with_a_refused_sidecar_keeps_its_receipt() {
    let f = Fixture::new();
    let img = image::RgbImage::from_fn(200, 150, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, 90])
    });
    let mut encoded = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut encoded)
        .encode_image(&img)
        .unwrap();
    let photo = f.archive.join("DSC9101.JPG");
    std::fs::write(&photo, &encoded).unwrap();
    let id = f
        .start("index", json!({"roots":[f.archive],"min_size":0}))
        .await;
    assert_eq!(f.wait(id).await["state"], "done");
    let (_, recent) = f.req("GET", "/api/recent?limit=1", Value::Null).await;
    let file_id = recent[0]["id"].as_i64().unwrap();
    f.req(
        "POST",
        &format!("/api/files/{file_id}/reject"),
        json!({"rejected":true}),
    )
    .await;
    let first = f.preview("plan-apply", json!({"roles":["copy"]})).await;
    let dst = PathBuf::from(first["items"][0]["dst"].as_str().unwrap());
    let side = photo.with_extension("xmp");
    std::fs::write(&side, b"my edits").unwrap();
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    let taken = dst.with_extension("xmp");
    std::fs::write(&taken, b"someone else's edits").unwrap();

    let preview = f.preview("plan-apply", json!({"roles":["copy"]})).await;
    assert_eq!(preview["items"].as_array().unwrap().len(), 1, "{preview}");
    let job = f.apply(&preview).await;

    assert_eq!(job["state"], "done", "{job}");
    assert!(dst.is_file(), "the photograph did not move");
    assert_eq!(std::fs::read(&side).unwrap(), b"my edits");
    assert_eq!(std::fs::read(&taken).unwrap(), b"someone else's edits");
    let said = job["progress"].to_string();
    assert!(said.contains("DSC9101.xmp"), "{job}");
}

/// One synthetic JPEG and a `.DS_Store` beside it in a folder only this
/// test uses, indexed the way a scan would, so organize plans to move the
/// frame and sweep the litter after it.
fn frame_with_litter(f: &Fixture, folder: &str) -> (PathBuf, u64) {
    let old = f.archive.join(folder);
    std::fs::create_dir(&old).unwrap();
    let src = old.join("frame.jpg");
    image::RgbImage::from_pixel(51, 43, image::Rgb([35, 80, 150]))
        .save(&src)
        .unwrap();
    std::fs::write(old.join(".DS_Store"), b"finder metadata").unwrap();
    let md = std::fs::metadata(&src).unwrap();
    let db = f.state.db.lock().unwrap();
    let run = db
        .start_run(&[f.archive.display().to_string()], "test")
        .unwrap();
    db.upsert_file(
        &pc_db::NewFile {
            path: src.display().to_string(),
            name: "frame.jpg".into(),
            dev: pc_core::volume::device_of(&md, &src) as i64,
            size: md.len() as i64,
            mtime: pc_core::time::mtime_unix(&md),
            ..Default::default()
        },
        run,
    )
    .unwrap();
    (src, md.len())
}

/// The volume refuses the move of the litter only, as a volume without
/// no-replace rename refuses a call it lacks; the frame has moved already.
fn litter_unsupported(marker: &str) -> pc_apply::race::SharedGuard {
    pc_apply::race::before_move_under(marker, |src, _| {
        if src.file_name().is_some_and(|n| n == ".DS_Store") {
            Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
        } else {
            Ok(())
        }
    })
}

/// R4 (reviewer el-2hkbk). The organize job stops on the litter after the
/// frame moved: the job says the frame moved — one file and its bytes —
/// instead of overwriting the actual total with zero.
#[tokio::test]
async fn independent_litter_stop_job_counts_moved_frame() {
    let f = Fixture::new();
    let marker = "stop-litter-review-api";
    let (src, size) = frame_with_litter(&f, marker);
    let dest = f.archive.join("new");
    std::fs::create_dir(&dest).unwrap();
    let plan = f
        .preview(
            "organize-apply",
            json!({"root":dest,"allow_duplicates":true}),
        )
        .await;
    assert_eq!(plan["total_files"], 1, "{plan}");
    let target = PathBuf::from(plan["items"][0]["dst"].as_str().unwrap());

    let _unsupported = litter_unsupported(marker);
    let job = f.apply(&plan).await;

    assert_eq!(job["state"], "failed", "{job}");
    assert!(target.is_file() && !src.exists(), "the frame did not move");
    assert!(
        src.with_file_name(".DS_Store").is_file(),
        "the refused litter must stay where it was"
    );
    let err = job["error"].as_str().unwrap();
    let moved = pc_apply::Tally {
        frames: 1,
        bytes: size,
        ..Default::default()
    };
    assert!(
        err.contains(&moved.summary()),
        "the moved frame is missing from the job's total: {err}"
    );
}

/// The same stop, on equivalent independent fixtures: the command line runs
/// the whole plan through `pc_apply::organize` and prints the typed stop's
/// total; the web runs it action by action. Both report the same moved
/// work, nothing counted twice and nothing reset.
#[tokio::test]
async fn cli_and_api_report_equal_nested_partial_results() {
    // The command line's path: one call over the whole plan.
    let cli = Fixture::new();
    let cli_marker = "nested-parity-cli";
    let (cli_src, _) = frame_with_litter(&cli, cli_marker);
    let cli_dest = cli.archive.join("new");
    std::fs::create_dir(&cli_dest).unwrap();
    let cli_stop = {
        let _unsupported = litter_unsupported(cli_marker);
        let db = cli.state.db.lock().unwrap();
        let plan = pc_organize::compute(
            &db,
            &pc_organize::Options {
                root: cli_dest.clone(),
                gap_secs: 21600,
                respect_lightroom: true,
                skip_uncertain: false,
            },
        )
        .unwrap();
        let run = db.latest_run().unwrap().unwrap();
        let e = pc_apply::organize(&db, run, &plan.moves).unwrap_err();
        pc_apply::stopped_run(&e)
            .expect("a typed stop for the command line to print")
            .clone()
    };
    assert!(!cli_src.exists());

    // The web's path: preview, token, confirmation, job.
    let api = Fixture::new();
    let api_marker = "nested-parity-api";
    frame_with_litter(&api, api_marker);
    let api_dest = api.archive.join("new");
    std::fs::create_dir(&api_dest).unwrap();
    let plan = api
        .preview(
            "organize-apply",
            json!({"root":api_dest,"allow_duplicates":true}),
        )
        .await;
    let job = {
        let _unsupported = litter_unsupported(api_marker);
        api.apply(&plan).await
    };

    assert_eq!(job["state"], "failed", "{job}");
    assert_eq!(cli_stop.done.frames, 1, "{:?}", cli_stop.done);
    assert_eq!(cli_stop.done.litter, 0, "{:?}", cli_stop.done);
    let err = job["error"].as_str().unwrap();
    assert!(
        err.contains(&cli_stop.done.summary()),
        "web: {err}\ncommand line: {}",
        cli_stop.done.summary()
    );
    // Said once, not the frame twice.
    let twice = pc_apply::Tally {
        frames: 2,
        ..cli_stop.done.clone()
    };
    assert!(!err.contains(&twice.summary()), "{err}");
}

/// B3 (el-1y8uo), the web half. Two photographs are reorganised; the
/// journal refuses the second one's row after the first has moved. The job
/// fails and its error carries the receipt of the first move — the same
/// words the command line prints for the same stop (its half is
/// `pc-cli/tests/cli_process.rs`,
/// `a_database_failure_after_a_move_prints_the_receipt`).
#[cfg(unix)]
#[tokio::test]
async fn a_database_failure_after_a_move_keeps_the_receipt_in_the_job() {
    use std::os::unix::fs::MetadataExt;
    let f = Fixture::new();
    let old = f.archive.join("old");
    std::fs::create_dir(&old).unwrap();
    {
        let db = f.state.db.lock().unwrap();
        let run = db
            .start_run(&[f.archive.display().to_string()], "review")
            .unwrap();
        for name in ["one.jpg", "two.jpg"] {
            let src = old.join(name);
            image::RgbImage::from_pixel(51, 43, image::Rgb([35, 80, 150]))
                .save(&src)
                .unwrap();
            let md = std::fs::metadata(&src).unwrap();
            db.upsert_file(
                &pc_db::NewFile {
                    path: src.display().to_string(),
                    name: name.into(),
                    dev: md.dev() as i64,
                    size: md.len() as i64,
                    mtime: pc_core::time::mtime_unix(&md),
                    ..Default::default()
                },
                run,
            )
            .unwrap();
        }
    }
    let dest = f.archive.join("new");
    std::fs::create_dir(&dest).unwrap();
    let plan = f
        .preview(
            "organize-apply",
            json!({"root":dest,"allow_duplicates":true}),
        )
        .await;
    assert_eq!(plan["total_files"], 2, "{plan}");
    {
        let db = f.state.db.lock().unwrap();
        db.conn
            .execute_batch(
                "CREATE TRIGGER fail_second BEFORE INSERT ON journal \
                 WHEN (SELECT count(*) FROM journal WHERE op='organize') >= 1 \
                 BEGIN SELECT RAISE(FAIL,'review second journal failure'); END;",
            )
            .unwrap();
    }
    let job = f.apply(&plan).await;
    assert_eq!(job["state"], "failed", "{job}");
    let moved = PathBuf::from(plan["items"][0]["dst"].as_str().unwrap());
    let size = std::fs::metadata(&moved).unwrap().len();
    let receipt = pc_apply::Tally {
        frames: 1,
        bytes: size,
        ..Default::default()
    }
    .summary();
    let err = job["error"].as_str().unwrap();
    assert!(err.contains(&receipt), "{receipt} missing: {err}");
    assert!(err.contains("review second journal failure"), "{err}");
}
