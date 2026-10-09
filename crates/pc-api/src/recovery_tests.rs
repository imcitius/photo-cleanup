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

/// The same row read by both. A refused first undo leaves the frame held
/// with its sidecar (user decision (c)); where someone else's file takes the
/// frame's name at home, both refuse and nothing moves; once it is gone,
/// both offer the undo again.
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
    assert!(std::fs::symlink_metadata(&home).is_err(), "half undo");
    // Someone else's file takes the frame's name at home.
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
    assert_eq!(std::fs::read(&held).unwrap(), b"our frame");
    assert_eq!(
        std::fs::read(held.with_extension("xmp")).unwrap(),
        b"our edits"
    );

    // The name is free again: both offer the undo again.
    std::fs::rename(&home, f.archive.join("someone-elses-frame.arw")).unwrap();
    let preview = f.preview("journal-undo", json!({"journal_id":id})).await;
    assert_eq!(preview["items"].as_array().unwrap().len(), 1, "{preview}");
}

/// A photograph whose sidecar's place in quarantine is taken: the frame and
/// its companions are one unit (user decision (c)), so on the web, as on the
/// command line, neither moves, and the job says which sidecar held it.
#[tokio::test]
async fn a_frame_whose_sidecar_cannot_follow_stays_and_the_job_says_why() {
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

    assert!(
        !dst.exists(),
        "the photograph moved without its sidecar: {job}"
    );
    assert_eq!(std::fs::read(&photo).unwrap(), encoded);
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

/// el-lvtmk §5 test 23. One photograph, reorganised on two equivalent
/// fixtures — by the command line's engine in one call, by the web action
/// by action. After the rename another program moves the destination folder
/// aside and takes the photograph's old name, so the move cannot be put back
/// and the photograph stays where it landed. Both stop the run, and both say
/// where it is with the same typed value: the web's job carries every line
/// the command line prints from `Placed`.
#[cfg(unix)]
#[tokio::test]
async fn cli_and_api_render_the_same_retained_outcome() {
    fn retain_after_rename(marker: &str) -> pc_apply::race::SyscallSharedGuard {
        let mut fired = false;
        pc_apply::race::at_rename_under(marker, move |at, src, dst| {
            if at == pc_apply::race::Syscall::After
                && !fired
                && src.file_name().is_some_and(|n| n == "frame.jpg")
            {
                fired = true;
                let folder = dst.parent().unwrap();
                let mut parked = folder.as_os_str().to_owned();
                parked.push("-parked");
                std::fs::rename(folder, &parked).unwrap();
                std::fs::write(src, b"another program's file").unwrap();
            }
            Ok(())
        })
    }
    fn normal(text: &str, root: &std::path::Path) -> String {
        text.replace(&root.display().to_string(), "$ROOT")
    }

    // The command line's path.
    let cli = Fixture::new();
    let cli_marker = "retained-parity-cli";
    frame_with_litter(&cli, cli_marker);
    let cli_dest = cli.archive.join("new");
    std::fs::create_dir(&cli_dest).unwrap();
    let cli_report = {
        let _race = retain_after_rename(cli_marker);
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
        pc_apply::organize(&db, run, &plan.moves).unwrap_err()
    };
    // Kept by the operation: the run stops and its row stays open (user
    // decision (c)); the stop carries every place, typed.
    let stop = pc_apply::stopped_run(&cli_report).expect("typed stop");
    assert_eq!(stop.pending.len(), 1, "{cli_report:#}");
    let held: Vec<&pc_apply::Placed> = stop.placed.iter().filter(|p| p.held).collect();
    assert_eq!(held.len(), 1, "{cli_report:#}");
    assert!(held[0].at.at().is_some(), "{:?}", held[0]);
    let cli_root = cli.archive.parent().unwrap().to_path_buf();
    let lines: Vec<String> = stop
        .placed
        .iter()
        .map(|p| {
            let (path, words) = p.line();
            normal(&format!("{path} — {words}"), &cli_root)
        })
        .collect();

    // The web's path: preview, token, confirmation, job.
    let api = Fixture::new();
    let api_marker = "retained-parity-api";
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
        let _race = retain_after_rename(api_marker);
        api.apply(&plan).await
    };
    assert_eq!(
        job["state"], "failed",
        "a moved folder stops the job: {job}"
    );
    let api_root = api.archive.parent().unwrap().to_path_buf();
    let said = normal(&job.to_string().replace("\\\"", "\""), &api_root);
    for line in &lines {
        let line = line.replace(cli_marker, api_marker);
        assert!(said.contains(&line), "web lacks «{line}»: {said}");
    }
}

/// el-14vx0. An undo whose place is taken: the preview lists the conflict
/// with pc-apply's choices and keeps the file by default; with a choice in
/// the parameters, the job carries it out exactly as the command line's
/// `--on-conflict` does — the same pc-apply function, the same result.
#[cfg(unix)]
#[tokio::test]
async fn the_web_offers_the_choice_on_a_taken_place_and_carries_it_out_like_the_cli() {
    let f = Fixture::new();
    let (home, held, id) = legacy_held(&f);
    std::fs::write(&home, b"someone else's frame").unwrap();

    let p = f.preview("journal-undo", json!({"journal_id":id})).await;
    assert!(p["items"].as_array().unwrap().is_empty(), "{p}");
    let c = &p["conflicts"][0];
    assert_eq!(c["journal_id"], id, "{p}");
    assert_eq!(c["choice"], "keep");
    let offered: Vec<&str> = c["choices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["choice"].as_str().unwrap())
        .collect();
    assert_eq!(
        offered,
        ["keep", "replace", "rename-existing", "rename-returning"]
    );
    let kept = p["refusals"][0]["why"].as_str().unwrap();
    assert!(kept.contains(&held.display().to_string()), "{kept}");

    let p = f
        .preview(
            "journal-undo",
            json!({"journal_id":id,"choices":{id.to_string():"rename-returning"}}),
        )
        .await;
    assert_eq!(p["items"][0]["choice"], "rename-returning", "{p}");
    let done = f.apply(&p).await;
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!(std::fs::read(&home).unwrap(), b"someone else's frame");
    assert_eq!(
        std::fs::read(f.archive.join("frame_1.arw")).unwrap(),
        b"our frame"
    );
    assert_eq!(
        std::fs::read(f.archive.join("frame_1.xmp")).unwrap(),
        b"our edits"
    );
}

/// el-14vx0. A replace reviewed against one file, and another file in its
/// place by the time the job runs: the plan changed, nothing moves, the
/// newcomer is neither replaced nor set aside.
#[cfg(unix)]
#[tokio::test]
async fn a_file_swapped_in_after_the_preview_is_never_replaced_by_the_job() {
    let f = Fixture::new();
    let (home, held, id) = legacy_held(&f);
    std::fs::write(&home, b"someone else's frame").unwrap();
    let p = f
        .preview(
            "journal-undo",
            json!({"journal_id":id,"choices":{id.to_string():"replace"}}),
        )
        .await;
    assert_eq!(p["items"].as_array().unwrap().len(), 1, "{p}");
    std::fs::rename(&home, f.archive.join("elsewhere.arw")).unwrap();
    std::fs::write(&home, b"a newcomer, never reviewed").unwrap();

    let (s, v) = f
        .req(
            "POST",
            "/api/jobs",
            json!({"kind":p["kind"],"params":p["params"],"plan_token":p["token"]}),
        )
        .await;
    if s == 202 {
        let j = f.wait(v["job_id"].as_i64().unwrap()).await;
        assert_ne!(j["state"], "done", "{j}");
    }
    assert_eq!(std::fs::read(&home).unwrap(), b"a newcomer, never reviewed");
    assert_eq!(std::fs::read(&held).unwrap(), b"our frame");
    let db = f.state.db.lock().unwrap();
    assert_eq!(
        db.journal_entry(id).unwrap().unwrap().status,
        pc_db::JournalStatus::Done
    );
}

#[tokio::test]
async fn reviewer_reconcile_conflicts_must_offer_the_same_four_choices() {
    let f = Fixture::new();
    let (home, held, id) = legacy_held(&f);
    {
        let db = f.state.db.lock().unwrap();
        db.journal_finish(id, pc_db::JournalStatus::Pending, None)
            .unwrap();
    }
    std::fs::write(&home, b"foreign existing frame").unwrap();
    let before = std::fs::read(&home).unwrap();
    let p = f
        .preview(
            "journal-reconcile",
            json!({"journal_id":id,"choices":{id.to_string():"rename-returning"}}),
        )
        .await;
    eprintln!("RECONCILE_PREVIEW={p}");
    assert_eq!(std::fs::read(&home).unwrap(), before);
    assert_eq!(std::fs::read(&held).unwrap(), b"our frame");
    assert_eq!(
        p["conflicts"].as_array().unwrap().len(),
        1,
        "pending recovery has no conflict choices"
    );
}

#[tokio::test]
async fn reviewer_api_refuses_a_new_companion_before_setting_existing_unit_aside() {
    let f = Fixture::new();
    let (home, held, id) = legacy_held(&f);
    std::fs::write(&home, b"foreign existing frame").unwrap();
    let p = f
        .preview(
            "journal-undo",
            json!({"journal_id":id,"choices":{id.to_string():"rename-existing"}}),
        )
        .await;
    let marker = f.archive.to_string_lossy().into_owned();
    let existing = home.clone();
    let late = home.with_extension("aae");
    let put = late.clone();
    let _g = pc_apply::race::before_move_under(&marker, move |src, _| {
        if src == existing {
            std::fs::write(&put, b"late foreign edits")?;
        }
        Ok(())
    });
    let job = f.apply(&p).await;
    eprintln!("NEW_COMPANION_HTTP_JOB={job}");
    assert_eq!(std::fs::read(&late).unwrap(), b"late foreign edits");
    assert_eq!(
        std::fs::read(&home).unwrap(),
        b"foreign existing frame",
        "API reports successful undo after splitting existing unit"
    );
    assert_eq!(std::fs::read(&held).unwrap(), b"our frame");
}

#[tokio::test]
async fn reviewer_forced_lightroom_choices_keep_every_payload_in_api() {
    for ch in ["replace", "rename-existing"] {
        let f = Fixture::new();
        let (home, held, id) = legacy_held(&f);
        std::fs::write(&home, b"catalogued existing frame").unwrap();
        {
            let db = f.state.db.lock().unwrap();
            let cat = db
                .upsert_catalog(&pc_db::NewCatalog {
                    path: f.archive.join("Catalog.lrcat").display().to_string(),
                    name: "Catalog".into(),
                    disk: String::new(),
                    size: 0,
                    is_backup: false,
                    is_locked: false,
                    image_count: Some(1),
                    read_error: None,
                })
                .unwrap();
            db.replace_catalog_files(cat, &[(home.display().to_string(), Some(5), None)])
                .unwrap();
        }
        let p = f
            .preview(
                "journal-undo",
                json!({"journal_id":id,"choices":{id.to_string():ch}}),
            )
            .await;
        let offered = p["conflicts"][0]["choices"].as_array().unwrap();
        assert!(offered
            .iter()
            .all(|c| c["choice"] != "replace" && c["choice"] != "rename-existing"));
        let job = f.apply(&p).await;
        eprintln!("FORCED_LIGHTROOM_{ch}={job}");
        assert_eq!(std::fs::read(&home).unwrap(), b"catalogued existing frame");
        assert_eq!(std::fs::read(&held).unwrap(), b"our frame");
        assert!(job.to_string().contains("not offered"), "{job}");
    }
}

#[tokio::test]
async fn reviewer_web_mixed_keep_decision_is_structurally_journaled() {
    let f = Fixture::new();
    let (run, ids) = {
        let db = f.state.db.lock().unwrap();
        let run = db
            .start_run(&[f.archive.display().to_string()], "test")
            .unwrap();
        let mut ids = Vec::new();
        for name in ["first.arw", "second.arw"] {
            let home = f.archive.join(name);
            let held = f.archive.join(format!("held-{name}"));
            std::fs::write(&held, b"returning frame").unwrap();
            std::fs::write(&home, b"foreign occupant").unwrap();
            let m = pc_db::Moved {
                src: home.display().to_string(),
                dst: held.display().to_string(),
                proof: pc_core::proof::Proof::of(&std::fs::metadata(&held).unwrap()),
            };
            let id = db
                .journal_begin(&pc_db::NewJournalEntry {
                    run_id: run,
                    op: "organize",
                    target_id: None,
                    src: &m.src,
                    dst: Some(&m.dst),
                    size: 15,
                    file_count: 1,
                    manifest: std::slice::from_ref(&m),
                })
                .unwrap();
            db.journal_finish(id, pc_db::JournalStatus::Done, None)
                .unwrap();
            ids.push(id);
        }
        (run, ids)
    };
    let p=f.preview("organize-undo",json!({"run_id":run,"choices":{ids[0].to_string():"keep",ids[1].to_string():"rename-returning"}})).await;
    let job = f.apply(&p).await;
    eprintln!("MIXED_KEEP_JOB={job}");
    let db = f.state.db.lock().unwrap();
    let ev = db.journal_events(ids[0]).unwrap();
    eprintln!("KEPT_ENTRY_EVENTS={ev:?}");
    assert_eq!(
        std::fs::read(f.archive.join("first.arw")).unwrap(),
        b"foreign occupant"
    );
    assert_eq!(
        std::fs::read(f.archive.join("held-first.arw")).unwrap(),
        b"returning frame"
    );
    assert_eq!(
        std::fs::read(f.archive.join("second_1.arw")).unwrap(),
        b"returning frame"
    );
    assert!(
        ev.iter().any(|e| e.phase == "undo"
            && e.kind == "kept"
            && e.data
                .as_deref()
                .is_some_and(|d| d.contains("\"choice\":\"keep\""))),
        "confirmed web keep decision missing from structured history"
    );
}

#[tokio::test]
async fn reviewer_replace_is_listed_with_origin_and_undoing_it_preserves_both_units() {
    let f = Fixture::new();
    let (home, held, id) = legacy_held(&f);
    std::fs::write(&home, b"foreign existing frame").unwrap();
    let p = f
        .preview(
            "journal-undo",
            json!({"journal_id":id,"choices":{id.to_string():"replace"}}),
        )
        .await;
    let job = f.apply(&p).await;
    assert_eq!(job["state"], "done", "{job}");
    let (status, q) = f.req("GET", "/api/quarantine", Value::Null).await;
    assert_eq!(status, 200);
    let rows = q.as_array().unwrap();
    assert_eq!(rows.len(), 1, "{q}");
    assert_eq!(rows[0]["src"], home.display().to_string());
    let aside = rows[0]["journal_id"].as_i64().unwrap();
    let place = rows[0]["dst"].as_str().unwrap();
    assert_eq!(std::fs::read(place).unwrap(), b"foreign existing frame");
    {
        let db = f.state.db.lock().unwrap();
        let e = db.journal_entry(aside).unwrap().unwrap();
        assert_eq!(e.op, "quarantine-file");
        assert!(e.manifest.iter().all(|m| m.proof.is_some()));
        assert!(db.journal_events(id).unwrap().iter().any(|e| e
            .data
            .as_ref()
            .is_some_and(|d| d.contains(&format!("\"aside_entry\":{aside}")))));
        assert!(db
            .journal_events(aside)
            .unwrap()
            .iter()
            .any(|e| e.kind == "done"));
    }
    let p = f
        .preview(
            "journal-undo",
            json!({"journal_id":aside,"choices":{aside.to_string():"replace"}}),
        )
        .await;
    let job = f.apply(&p).await;
    assert_eq!(job["state"], "done", "{job}");
    assert_eq!(std::fs::read(&home).unwrap(), b"foreign existing frame");
    assert_eq!(std::fs::read(&held).unwrap(), b"our frame");
    assert_eq!(
        std::fs::read(held.with_extension("xmp")).unwrap(),
        b"our edits"
    );
    let (_, q) = f.req("GET", "/api/quarantine", Value::Null).await;
    assert_eq!(q.as_array().unwrap().len(), 1, "{q}");
    eprintln!("REPLACE_CHAIN_AND_QUARANTINE_ORIGIN_PASS={q}");
}
