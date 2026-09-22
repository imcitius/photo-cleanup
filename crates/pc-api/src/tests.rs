use super::*;
use axum::{
    body::{to_bytes, Body},
    http::Request,
};
use serde_json::{json, Value};
use std::{
    path::{Path as FsPath, Path, PathBuf},
    time::Duration,
};
use tower::ServiceExt;

struct Fixture {
    _tmp: tempfile::TempDir,
    state: Arc<AppState>,
    archive: PathBuf,
    quarantine: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let archive = root.join("archive");
        let quarantine = root.join("quarantine");
        std::fs::create_dir(&archive).unwrap();
        std::fs::create_dir(&quarantine).unwrap();
        Self {
            state: Arc::new(
                AppState::new(
                    &root.join("test.db"),
                    &root.join("thumbs"),
                    Some(quarantine.clone()),
                )
                .unwrap(),
            ),
            archive,
            quarantine,
            _tmp: tmp,
        }
    }
    async fn req(&self, method: &str, path: &str, body: Value) -> (u16, Value) {
        let response = router(self.state.clone())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(if method == "GET" {
                        Body::empty()
                    } else {
                        Body::from(body.to_string())
                    })
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status().as_u16();
        let bytes = to_bytes(response.into_body(), 32 * 1024 * 1024)
            .await
            .unwrap();
        let value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| json!(String::from_utf8_lossy(&bytes)));
        (status, value)
    }
    async fn start(&self, kind: &str, params: Value) -> i64 {
        let (status, v) = self
            .req("POST", "/api/jobs", json!({"kind":kind,"params":params}))
            .await;
        assert_eq!(status, 202, "{v}");
        v["job_id"].as_i64().unwrap()
    }
    async fn wait(&self, id: i64) -> Value {
        for _ in 0..1000 {
            let (_, j) = self
                .req("GET", &format!("/api/jobs/{id}"), Value::Null)
                .await;
            if !matches!(j["state"].as_str(), Some("queued" | "running")) {
                return j;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("job timed out")
    }
    async fn scan(&self) {
        let id = self.start("scan", json!({"roots":[self.archive]})).await;
        let j = self.wait(id).await;
        assert_eq!(j["state"], "done", "{j}");
    }
    async fn preview(&self, kind: &str, params: Value) -> Value {
        let (s, p) = self
            .req("POST", "/api/preview", json!({"kind":kind,"params":params}))
            .await;
        assert_eq!(s, 200, "{p}");
        p
    }
    async fn apply(&self, p: &Value) -> Value {
        let(s,v)=self.req("POST","/api/jobs",json!({"kind":p["kind"],"params":p["params"],"plan_token":p["token"],"confirmation":"DELETE"})).await;
        assert_eq!(s, 202, "{v}");
        self.wait(v["job_id"].as_i64().unwrap()).await
    }
    fn bundle(&self, name: &str) {
        let dir = self.archive.join(format!("{name} Previews.lrdata"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("preview.jpg"), b"rebuildable preview").unwrap();
    }
}
#[tokio::test]
async fn api_contract_and_embedded_production_assets() {
    let f = Fixture::new();
    let (s, v) = f.req("GET", "/api/status", Value::Null).await;
    assert_eq!(s, 200);
    for key in [
        "files",
        "images",
        "families",
        "roles",
        "derived_removable_bytes",
        "quarantined_bytes",
    ] {
        assert!(v.get(key).is_some(), "{key}");
    }
    let (s, v) = f.req("GET", "/", Value::Null).await;
    assert_eq!(s, 200);
    assert!(v.as_str().unwrap().contains("/static/app.js"));
    for path in ["/api/jobs", "/api/catalogs", "/api/journal", "/api/runs"] {
        let (s, v) = f.req("GET", path, Value::Null).await;
        assert_eq!(s, 200);
        assert!(v.is_array());
    }
}
#[tokio::test]
async fn one_writer_and_legacy_urls_cannot_bypass_review() {
    let f = Fixture::new();
    *f.state.jobs.active.lock().unwrap() = Some((42, pc_core::work::Control::default()));
    let (s, v) = f
        .req(
            "POST",
            "/api/jobs",
            json!({"kind":"categories","params":{}}),
        )
        .await;
    assert_eq!(s, 409);
    assert!(v["error"].as_str().unwrap().contains("42"));
    for path in [
        "/api/plan/apply",
        "/api/quarantine/1/undo",
        "/api/families/1/keeper",
    ] {
        let (s, _) = f.req("POST", path, json!({"file_id":1})).await;
        assert_eq!(s, 409);
    }
    *f.state.jobs.active.lock().unwrap() = None;
    let (s, _) = f
        .req(
            "POST",
            "/api/jobs",
            json!({"kind":"derived-clean","params":{}}),
        )
        .await;
    assert_eq!(s, 400);
}
#[tokio::test]
async fn reviewed_derived_cycle_undo_and_purge_and_stale_plan() {
    let f = Fixture::new();
    f.bundle("First");
    f.scan().await;
    let params = json!({"kinds":["lr-previews"],"min_size":0});
    let old = f.preview("derived-clean", params.clone()).await;
    assert_eq!(old["total_files"], 1);
    f.bundle("Second");
    f.scan().await;
    let j = f.apply(&old).await;
    assert_eq!(j["state"], "failed");
    assert!(j["error"].as_str().unwrap().contains("plan has changed"));
    assert!(f.archive.join("First Previews.lrdata").exists());
    let plan = f.preview("derived-clean", params.clone()).await;
    assert_eq!(plan["items"].as_array().unwrap().len(), 2);
    assert!(plan["items"][0]["dst"]
        .as_str()
        .unwrap()
        .starts_with(f.quarantine.to_str().unwrap()));
    let j = f.apply(&plan).await;
    assert_eq!(j["state"], "done", "{j}");
    assert!(!f.archive.join("First Previews.lrdata").exists());
    let (_, entries) = f.req("GET", "/api/journal", Value::Null).await;
    let id = entries[0]["id"].as_i64().unwrap();
    let undo = f.preview("journal-undo", json!({"journal_id":id})).await;
    assert_eq!(f.apply(&undo).await["state"], "done");
    let p = f.preview("derived-clean", params).await;
    assert_eq!(f.apply(&p).await["state"], "done");
    f.state
        .db
        .lock()
        .unwrap()
        .conn
        .execute("UPDATE journal SET applied_at=1 WHERE status='done'", [])
        .unwrap();
    let purge = f
        .preview("derived-purge", json!({"older_than_secs":604800}))
        .await;
    let (s, _) = f
        .req(
            "POST",
            "/api/jobs",
            json!({"kind":"derived-purge","params":purge["params"],"plan_token":purge["token"]}),
        )
        .await;
    assert_eq!(s, 400);
    assert_eq!(f.apply(&purge).await["state"], "done");
    let (_, status) = f.req("GET", "/api/status", Value::Null).await;
    assert_eq!(status["quarantined_bytes"], 0);
}
#[tokio::test]
async fn open_lightroom_at_apply_time_invalidates_a_preview() {
    let f = Fixture::new();
    f.bundle("Library");
    f.scan().await;
    let p = f
        .preview("derived-clean", json!({"kinds":["lr-previews"]}))
        .await;
    std::fs::write(f.archive.join("Library.lrcat.lock"), b"open").unwrap();
    assert_eq!(f.apply(&p).await["state"], "failed");
    assert!(f.archive.join("Library Previews.lrdata").exists());
}
#[tokio::test]
async fn recovery_and_directory_boundaries() {
    let f = Fixture::new();
    f.state
        .db
        .lock()
        .unwrap()
        .conn
        .execute(
            "INSERT INTO jobs(kind,params,state,started_at) VALUES('index','{}','running',1)",
            [],
        )
        .unwrap();
    let reopened = AppState::new(&f.state.db_path, f.state.thumbs.root(), None).unwrap();
    let jobs = jobs::rows(&reopened.db.lock().unwrap(), "SELECT * FROM jobs", &[]).unwrap();
    assert_eq!(jobs[0]["state"], "interrupted");
    let secret = f.archive.join("not-a-directory");
    std::fs::write(&secret, b"not exposed").unwrap();
    // Symlinks need an elevated process on Windows, so the escape attempt
    // through one is checked where it can be made.
    let link = f.archive.join("escape");
    #[cfg(unix)]
    std::os::unix::fs::symlink("/", &link).unwrap();
    let (s, v) = f
        .req(
            "GET",
            &format!("/api/fs?path={}", f.archive.display()),
            Value::Null,
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(v["directories"].as_array().unwrap().len(), 0);
    // Spelled as text: joining `..` onto a Windows verbatim path makes Rust
    // fold it away, which would leave nothing for the endpoint to refuse.
    let up = PathBuf::from(format!(
        "{}{}..",
        f.archive.display(),
        std::path::MAIN_SEPARATOR
    ));
    let mut escapes = vec![secret, up];
    #[cfg(unix)]
    escapes.push(link);
    #[cfg(windows)]
    let _ = link;
    for path in escapes {
        let (s, v) = f
            .req(
                "GET",
                &format!("/api/fs?path={}", path.display()),
                Value::Null,
            )
            .await;
        assert_eq!(s, 400, "{} вернул {s}: {v}", path.display());
    }
}
#[tokio::test]
async fn real_index_is_readable_during_work_and_stops_between_files() {
    let f = Fixture::new();
    let img = image::RgbImage::from_fn(320, 240, |x, y| {
        image::Rgb([
            ((x * 7 + y * 3) % 256) as u8,
            ((y * 5) % 256) as u8,
            ((x + y) % 256) as u8,
        ])
    });
    let mut encoded = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut encoded)
        .encode_image(&img)
        .unwrap();
    for n in 0..180 {
        std::fs::write(f.archive.join(format!("DSC{n:04}.JPG")), &encoded).unwrap();
    }
    let id = f
        .start(
            "index",
            json!({"roots":[f.archive],"min_size":0,"readers_per_disk":1}),
        )
        .await;
    let (s, _) = f
        .req(
            "POST",
            "/api/jobs",
            json!({"kind":"categories","params":{}}),
        )
        .await;
    assert_eq!(s, 409);
    for _ in 0..100 {
        let (_, j) = f.req("GET", &format!("/api/jobs/{id}"), Value::Null).await;
        if j["progress"]["done"].as_i64().unwrap_or(0) > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (s, _) = f.req("GET", "/api/status", Value::Null).await;
    assert_eq!(s, 200);
    let (s, _) = f
        .req("POST", &format!("/api/jobs/{id}/cancel"), json!({}))
        .await;
    assert_eq!(s, 200);
    let j = f.wait(id).await;
    assert_eq!(j["state"], "cancelled", "{j}");
    let done = j["progress"]["done"].as_i64().unwrap();
    assert!(done < 180);
    let (_, after) = f.req("GET", &format!("/api/jobs/{id}"), Value::Null).await;
    assert_eq!(after["progress"]["done"], done);
}
#[tokio::test]
async fn manual_dates_and_categories_survive_reindexing() {
    let f = Fixture::new();
    let file_id = {
        let db = f.state.db.lock().unwrap();
        let run = db.start_run(&[], "test").unwrap();
        db.upsert_file(
            &pc_db::NewFile {
                path: f.archive.join("image.jpg").display().to_string(),
                name: "image.jpg".into(),
                ..Default::default()
            },
            run,
        )
        .unwrap()
    };
    let (s, _) = f
        .req(
            "POST",
            &format!("/api/files/{file_id}/date"),
            json!({"taken_at":1563129000}),
        )
        .await;
    assert_eq!(s, 200);
    let (s, _) = f
        .req(
            "POST",
            &format!("/api/files/{file_id}/category"),
            json!({"category":"document"}),
        )
        .await;
    assert_eq!(s, 200);
    let db = f.state.db.lock().unwrap();
    db.upsert_meta(
        file_id,
        &pc_db::NewMeta {
            taken_at: Some(1700000000),
            date_source: Some("exif".into()),
            ..Default::default()
        },
    )
    .unwrap();
    db.set_category(file_id, "photo", 0.9, "automatic").unwrap();
    let v=jobs::rows(&db,"SELECT m.taken_at,m.date_source,c.category,c.manual FROM meta m JOIN file_categories c USING(file_id)",&[]).unwrap();
    assert_eq!(v[0]["date_source"], "manual");
    assert_eq!(v[0]["taken_at"], 1563129000i64);
    assert_eq!(v[0]["category"], "document");
    assert_eq!(v[0]["manual"], 1);
}

#[tokio::test]
async fn a_reset_needs_the_word_and_leaves_the_archive_and_the_journal_alone() {
    let f = Fixture::new();
    f.bundle("Family");
    let photo = f.archive.join("keep.jpg");
    std::fs::write(&photo, vec![7u8; 200_000]).unwrap();
    f.scan().await;

    let (s, v) = f.req("POST", "/api/reset", json!({})).await;
    assert_eq!(s, 400, "{v}");
    assert!(v["error"].as_str().unwrap().contains("RESET"));
    let (s, v) = f
        .req("POST", "/api/reset", json!({"confirmation":"reset"}))
        .await;
    assert_eq!(s, 400, "{v}");

    let (_, before) = f.req("GET", "/api/derived", Value::Null).await;
    assert!(!before.as_array().unwrap().is_empty());

    let (s, v) = f
        .req("POST", "/api/reset", json!({"confirmation":"RESET"}))
        .await;
    assert_eq!(s, 200, "{v}");

    let (_, after) = f.req("GET", "/api/derived", Value::Null).await;
    assert!(after.as_array().unwrap().is_empty(), "{after}");
    let (_, status) = f.req("GET", "/api/status", Value::Null).await;
    assert_eq!(status["files"], 0);
    // The photographs themselves were never the target.
    assert!(photo.exists());
    // And a second scan finds everything again.
    f.scan().await;
    let (_, again) = f.req("GET", "/api/derived", Value::Null).await;
    assert_eq!(
        again.as_array().unwrap().len(),
        before.as_array().unwrap().len()
    );
}

#[tokio::test]
async fn a_reset_is_refused_while_a_job_holds_the_writer() {
    let f = Fixture::new();
    *f.state.jobs.active.lock().unwrap() = Some((7, pc_core::work::Control::default()));
    let (s, v) = f
        .req("POST", "/api/reset", json!({"confirmation":"RESET"}))
        .await;
    assert_eq!(s, 409, "{v}");
    assert!(v["error"].as_str().unwrap().contains("7"));
}

#[tokio::test]
async fn the_whole_chain_runs_as_one_job_and_reports_its_stage() {
    let f = Fixture::new();
    f.bundle("Family");
    let img = image::RgbImage::from_fn(320, 240, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8])
    });
    let mut encoded = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut encoded)
        .encode_image(&img)
        .unwrap();
    for n in 0..4 {
        std::fs::write(f.archive.join(format!("DSC{n:04}.JPG")), &encoded).unwrap();
    }

    let id = f
        .start("all", json!({"roots":[f.archive],"min_size":0}))
        .await;
    let j = f.wait(id).await;
    assert_eq!(j["state"], "done", "{j}");
    // Every stage ran, and the last one said so.
    assert_eq!(j["progress"]["step"], 5);
    assert_eq!(j["progress"]["steps"], 5);

    // The scan half found the bundle, the index half found the photographs.
    let (_, derived) = f.req("GET", "/api/derived", Value::Null).await;
    assert!(!derived.as_array().unwrap().is_empty());
    let (_, status) = f.req("GET", "/api/status", Value::Null).await;
    assert_eq!(status["images"], 4);
    assert!(status["families"].as_i64().unwrap() > 0);
}

#[tokio::test]
async fn one_file_carries_its_evidence_and_an_unknown_one_is_a_404() {
    let f = Fixture::new();
    let img = image::RgbImage::from_fn(64, 48, |x, y| {
        image::Rgb([(x * 4) as u8, (y * 5) as u8, 30])
    });
    let mut encoded = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut encoded)
        .encode_image(&img)
        .unwrap();
    std::fs::write(f.archive.join("one.jpg"), &encoded).unwrap();
    let id = f
        .start("index", json!({"roots":[f.archive],"min_size":0}))
        .await;
    assert_eq!(f.wait(id).await["state"], "done");

    let (_, recent) = f.req("GET", "/api/recent?limit=1", Value::Null).await;
    // Printed rather than unwrapped: this line has failed twice in a
    // thousand runs, and an empty list and an error body look the same to
    // `unwrap`. Whichever it is, the next failure says so.
    let file_id = recent[0]["id"]
        .as_i64()
        .unwrap_or_else(|| panic!("/api/recent answered: {recent}"));
    let (s, d) = f
        .req("GET", &format!("/api/file/{file_id}/details"), Value::Null)
        .await;
    assert_eq!(s, 200, "{d}");
    assert_eq!(d["width"], 64);
    assert_eq!(d["container"], "jpeg");
    assert!(d.get("meta").is_some());
    assert!(d["sharpness"].is_number(), "{d}");

    let (s, _) = f.req("GET", "/api/file/999999/details", Value::Null).await;
    assert_eq!(s, 400);
}

/// A burst of frames a second apart, indexed and grouped into a series.
async fn a_burst(f: &Fixture, count: u32) -> i64 {
    for n in 0..count {
        // Nearly the same scene each time, the way a burst is: alike enough
        // to be one series, different enough not to be exact copies.
        let img = image::RgbImage::from_fn(240, 180, |x, y| {
            let subject = if y > 80 && y < 100 && x > 40 + n * 2 && x < 70 + n * 2 {
                220
            } else {
                0
            };
            image::Rgb([
                ((x * 255 / 240) as u8).saturating_add(subject),
                (y * 255 / 180) as u8,
                60,
            ])
        });
        let mut encoded = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut encoded)
            .encode_image(&img)
            .unwrap();
        std::fs::write(f.archive.join(format!("DSC{n:04}.JPG")), &encoded).unwrap();
    }
    let id = f
        .start("index", json!({"roots":[f.archive],"min_size":0}))
        .await;
    assert_eq!(f.wait(id).await["state"], "done");
    {
        // The encoder writes no EXIF, so the shooting times come from here:
        // one second apart, deliberately out of name order for the first two.
        let db = f.state.db.lock().unwrap();
        let ids: Vec<i64> = db
            .conn
            .prepare("SELECT id FROM files ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        for (n, file) in ids.iter().enumerate() {
            // A series needs a camera behind it: an export is not another
            // press of the shutter.
            db.conn
                .execute(
                    "INSERT INTO meta(file_id,taken_at,date_source,camera_make,camera_model)
                     VALUES(?1,?2,'exif','Sony','ILCE-7M3')
                     ON CONFLICT(file_id) DO UPDATE SET
                       taken_at=excluded.taken_at,
                       camera_make=excluded.camera_make,
                       camera_model=excluded.camera_model",
                    rusqlite::params![file, 1_700_000_000i64 + n as i64],
                )
                .unwrap();
        }
    }
    let id = f.start("series", json!({"gap_secs":10})).await;
    assert_eq!(f.wait(id).await["state"], "done");
    let (_, list) = f.req("GET", "/api/series", Value::Null).await;
    list["series"][0]["id"]
        .as_i64()
        .expect("серия не построена")
}

#[tokio::test]
async fn a_series_is_listed_in_shooting_order_not_in_quality_order() {
    let f = Fixture::new();
    let series = a_burst(&f, 6).await;
    let (_, list) = f.req("GET", "/api/series", Value::Null).await;
    let members = list["series"][0]["members"].as_array().unwrap();
    assert_eq!(members.len(), 6, "{series}");

    let times: Vec<i64> = members
        .iter()
        .map(|m| m["taken_at"].as_i64().unwrap())
        .collect();
    let mut sorted = times.clone();
    sorted.sort();
    assert_eq!(times, sorted, "кадры идут не по времени съёмки");

    // The quality ranks are still there — they are just not the order.
    assert!(members.iter().all(|m| m["rank"].is_number()));
}

#[tokio::test]
async fn rejected_frames_reach_the_plan_and_can_be_taken_back() {
    let f = Fixture::new();
    let series = a_burst(&f, 6).await;
    let (_, list) = f.req("GET", "/api/series", Value::Null).await;
    let first = list["series"][0]["members"][0]["file_id"].as_i64().unwrap();

    let (s, v) = f
        .req(
            "POST",
            &format!("/api/files/{first}/reject"),
            json!({"rejected":true}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let (_, plan) = f.req("GET", "/api/plan", Value::Null).await;
    let rejected: Vec<&Value> = plan["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["file_id"] == first)
        .collect();
    assert_eq!(rejected.len(), 1, "{plan}");
    // The tests run in the default language, which is English.
    assert!(rejected[0]["reason"].as_str().unwrap().contains("by hand"));

    // Taking the mark back takes the file out of the plan again.
    let (s, _) = f
        .req(
            "POST",
            &format!("/api/files/{first}/reject"),
            json!({"rejected":false}),
        )
        .await;
    assert_eq!(s, 200);
    let (_, plan) = f.req("GET", "/api/plan", Value::Null).await;
    assert!(
        !plan["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["file_id"] == first),
        "{plan}"
    );

    // And the bulk action leaves exactly the best frame alone.
    let (s, v) = f
        .req(
            "POST",
            &format!("/api/series/{series}/reject-rest"),
            json!({}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["rejected"], 5);
    let (_, list) = f.req("GET", "/api/series", Value::Null).await;
    let members = list["series"][0]["members"].as_array().unwrap();
    assert_eq!(
        members.iter().filter(|m| m["is_rejected"] == true).count(),
        5
    );
    assert!(members
        .iter()
        .all(|m| m["is_best"] != true || m["is_rejected"] == false));

    let (s, v) = f
        .req("POST", &format!("/api/series/{series}/keep-all"), json!({}))
        .await;
    assert_eq!(s, 200, "{v}");
    let (_, plan) = f.req("GET", "/api/plan", Value::Null).await;
    assert!(plan["items"].as_array().unwrap().is_empty(), "{plan}");
}

#[tokio::test]
async fn a_rejected_frame_moves_beside_itself_and_comes_back() {
    let f = Fixture::new();
    // Not the fixture's own quarantine directory: the point of this test is
    // the default, which is what an ordinary machine uses.
    let state = Arc::new(
        AppState::new(
            &f.archive.parent().unwrap().join("default.db"),
            &f.archive.parent().unwrap().join("default-thumbs"),
            None,
        )
        .unwrap(),
    );
    let f = Fixture { state, ..f };

    let nested = f.archive.join("2024").join("лето");
    std::fs::create_dir_all(&nested).unwrap();
    let img = image::RgbImage::from_fn(200, 150, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, 90])
    });
    let mut encoded = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut encoded)
        .encode_image(&img)
        .unwrap();
    let photo = nested.join("DSC9001.JPG");
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

    let preview = f.preview("plan-apply", json!({"roles":["copy"]})).await;
    let dst = preview["items"][0]["dst"].as_str().unwrap().to_string();
    // Beside the photograph, in its own folder — not at the root of the
    // filesystem, which on an ordinary machine is "/" and unwritable.
    assert_eq!(
        dst,
        nested
            .join(pc_core::QUARANTINE_DIR)
            .join("DSC9001.JPG")
            .display()
            .to_string(),
        "{preview}"
    );

    let done = f.apply(&preview).await;
    assert_eq!(done["state"], "done", "{done}");
    assert!(!photo.exists(), "исходник остался на месте");
    assert!(Path::new(&dst).exists(), "файл не доехал до карантина");

    // And the journal can put it back where it was.
    let (_, q) = f.req("GET", "/api/quarantine", Value::Null).await;
    let journal_id = q[0]["journal_id"].as_i64().unwrap();
    let back = f
        .preview("journal-undo", json!({"journal_id":journal_id}))
        .await;
    let restored = f.apply(&back).await;
    assert_eq!(restored["state"], "done", "{restored}");
    assert!(photo.exists(), "файл не вернулся");
}

#[tokio::test]
async fn one_group_can_be_acted_on_without_touching_the_rest() {
    // Ten thousand groups is a frightening button. Acting on the group in
    // front of you has to move that group's copies and nothing else.
    let f = Fixture::new();
    for (n, shade) in [("a", 40u8), ("b", 200u8)] {
        let img = image::RgbImage::from_fn(240, 180, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, shade])
        });
        let mut encoded = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut encoded)
            .encode_image(&img)
            .unwrap();
        std::fs::write(f.archive.join(format!("{n}.jpg")), &encoded).unwrap();
        std::fs::write(f.archive.join(format!("{n} copy.jpg")), &encoded).unwrap();
    }
    let id = f
        .start("index", json!({"roots":[f.archive],"min_size":0}))
        .await;
    assert_eq!(f.wait(id).await["state"], "done");
    let id = f.start("families", json!({})).await;
    assert_eq!(f.wait(id).await["state"], "done");

    let (_, groups) = f.req("GET", "/api/families?limit=10", Value::Null).await;
    let groups = groups["families"].as_array().unwrap();
    assert_eq!(groups.len(), 2, "{groups:?}");
    let one = groups[0]["id"].as_i64().unwrap();

    let whole = f.preview("plan-apply", json!({"roles":["copy"]})).await;
    assert_eq!(whole["items"].as_array().unwrap().len(), 2, "{whole}");

    let narrowed = f
        .preview("plan-apply", json!({"roles":["copy"],"family_id":one}))
        .await;
    let items = narrowed["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{narrowed}");
    let moved = items[0]["path"].as_str().unwrap().to_string();

    let done = f.apply(&narrowed).await;
    assert_eq!(done["state"], "done", "{done}");
    let left: Vec<String> = std::fs::read_dir(&f.archive)
        .unwrap()
        .filter_map(|e| Some(e.ok()?.file_name().to_str()?.to_string()))
        .filter(|n| n.ends_with(".jpg"))
        .collect();
    assert_eq!(left.len(), 3, "тронули лишнее: {left:?}");
    assert!(!Path::new(&moved).exists(), "{moved}");
}

#[tokio::test]
async fn a_parent_component_is_refused_whatever_the_path_looks_like() {
    // A Windows verbatim path is handed to the OS untouched, and Rust stops
    // reading `..` inside it as a parent component — so the check cannot rely
    // on `Path` and is made on the text. These are the spellings that reach
    // the endpoint on the platforms this runs on.
    let f = Fixture::new();
    let archive = f.archive.display().to_string();
    for path in [
        format!("{archive}/.."),
        format!("{archive}/../"),
        format!("{archive}/../quarantine"),
        r"\\?\C:\Users\someone\Pictures\..\Documents".to_string(),
        r"C:\Users\someone\Pictures\..\Documents".to_string(),
    ] {
        let (s, v) = f
            .req("GET", &format!("/api/fs?path={path}"), Value::Null)
            .await;
        assert_eq!(s, 400, "{path} вернул {s}: {v}");
    }
}

#[tokio::test]
async fn a_group_keeps_the_last_decision_and_never_empties_itself() {
    // Deciding twice used to leave both marks standing: the group ended up
    // with two kept files, the plan took every member, and the group was
    // left with nothing on disk. The last word wins, and whatever happens,
    // one file stays.
    let f = Fixture::new();
    let img = image::RgbImage::from_fn(240, 180, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, 90])
    });
    let mut encoded = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut encoded)
        .encode_image(&img)
        .unwrap();
    for n in ["a.jpg", "a copy.jpg", "a copy 2.jpg"] {
        std::fs::write(f.archive.join(n), &encoded).unwrap();
    }
    let id = f
        .start("index", json!({"roots":[f.archive],"min_size":0}))
        .await;
    assert_eq!(f.wait(id).await["state"], "done");
    let id = f.start("families", json!({})).await;
    assert_eq!(f.wait(id).await["state"], "done");

    let (_, groups) = f.req("GET", "/api/families?limit=10", Value::Null).await;
    let group = groups["families"].as_array().unwrap()[0]["id"]
        .as_i64()
        .unwrap();
    let (_, before) = f
        .req("GET", &format!("/api/families/{group}"), Value::Null)
        .await;
    let ids: Vec<i64> = before["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["file_id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids.len(), 3, "{before}");

    for file in [ids[0], ids[1]] {
        let (s, v) = f
            .req(
                "POST",
                &format!("/api/families/{group}/keep-only"),
                json!({ "file_id": file }),
            )
            .await;
        assert_eq!(s, 200, "{v}");
    }
    // Rebuilding the groups must not resurrect the first decision.
    let id = f.start("families", json!({})).await;
    assert_eq!(f.wait(id).await["state"], "done");

    {
        let db = f.state.db.lock().unwrap();
        let marks = jobs::rows(
            &db,
            "SELECT file_id FROM manual_keepers ORDER BY file_id",
            &[],
        )
        .unwrap();
        assert_eq!(marks.len(), 1, "решений в базе больше одного: {marks:?}");
        assert_eq!(marks[0]["file_id"], ids[1]);
        let rejects = jobs::rows(
            &db,
            "SELECT file_id FROM manual_rejects WHERE file_id = ?1",
            &[&ids[1]],
        )
        .unwrap();
        assert!(rejects.is_empty(), "оставленный файл помечен лишним");
    }

    let (_, after) = f
        .req("GET", &format!("/api/families/{group}"), Value::Null)
        .await;
    let keepers: Vec<i64> = after["members"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["is_keeper"].as_bool() == Some(true))
        .map(|m| m["file_id"].as_i64().unwrap())
        .collect();
    assert_eq!(keepers, vec![ids[1]], "{after}");

    let plan = f
        .preview("plan-apply", json!({"roles":["copy"],"family_id":group}))
        .await;
    let planned: Vec<i64> = plan["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["file_id"].as_i64().unwrap())
        .collect();
    assert!(
        planned.len() < ids.len(),
        "план забирает всю группу: {plan}"
    );
    assert!(
        !planned.contains(&ids[1]),
        "план забирает оставленный: {plan}"
    );
}

#[tokio::test]
async fn an_orphan_comes_out_of_quarantine_the_way_it_went_in() {
    // Quarantine keeps the shape of what went into it, so a file deep inside
    // a directory that was moved whole belongs deep inside that directory
    // again — not one level up, inside the quarantine folder it never left.
    // And it is a disk operation like any other: reviewed, tokened, and
    // written into the journal, so it can be walked back.
    let f = Fixture::new();
    let hidden = f.archive.join(pc_core::QUARANTINE_DIR).join("Old.lrdata");
    std::fs::create_dir_all(hidden.join("sub")).unwrap();
    std::fs::write(hidden.join("sub/cache"), b"previews from another database").unwrap();
    f.scan().await;

    let (s, orphans) = f.req("GET", "/api/quarantine/orphans", Value::Null).await;
    assert_eq!(s, 200, "{orphans}");
    assert_eq!(orphans["files"], 1, "{orphans}");

    let plan = f.preview("quarantine-adopt", json!({})).await;
    let items = plan["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{plan}");
    let home = f.archive.join("Old.lrdata/sub/cache");
    assert_eq!(
        items[0]["dst"].as_str().unwrap(),
        home.display().to_string(),
        "{plan}"
    );

    // Nothing moves without the reviewed plan.
    let (s, v) = f
        .req("POST", "/api/jobs", json!({"kind":"quarantine-adopt"}))
        .await;
    assert_eq!(s, 400, "{v}");

    let done = f.apply(&plan).await;
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!(
        std::fs::read(&home).unwrap(),
        b"previews from another database"
    );
    assert!(
        !hidden.join("sub/cache").exists(),
        "файл остался в карантине"
    );

    // The journal holds the move, so it is undoable like everything else.
    let db = f.state.db.lock().unwrap();
    let rows = jobs::rows(
        &db,
        "SELECT src, dst, status FROM journal WHERE op = 'adopt'",
        &[],
    )
    .unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["status"], "done");
    assert_eq!(rows[0]["dst"], home.display().to_string());
}

#[tokio::test]
async fn a_purge_takes_the_sidecar_with_the_frame_and_leaves_nothing_behind() {
    let f = Fixture::new();
    let img = image::RgbImage::from_fn(240, 180, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, 90])
    });
    let mut encoded = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut encoded)
        .encode_image(&img)
        .unwrap();
    for n in ["a.jpg", "a copy.jpg"] {
        std::fs::write(f.archive.join(n), &encoded).unwrap();
        std::fs::write(f.archive.join(n.replace(".jpg", ".xmp")), b"edits").unwrap();
    }
    let id = f
        .start("index", json!({"roots":[f.archive],"min_size":0}))
        .await;
    assert_eq!(f.wait(id).await["state"], "done");
    let id = f.start("families", json!({})).await;
    assert_eq!(f.wait(id).await["state"], "done");

    let plan = f.preview("plan-apply", json!({"roles":["copy"]})).await;
    assert_eq!(plan["items"].as_array().unwrap().len(), 1, "{plan}");
    assert_eq!(f.apply(&plan).await["state"], "done");

    // The holding period is counted in whole seconds, and nothing is purged
    // on the same one it arrived.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let purge = f
        .preview("derived-purge", json!({"older_than_secs":0}))
        .await;
    assert_eq!(purge["items"].as_array().unwrap().len(), 1, "{purge}");
    assert_eq!(purge["total_files"], 2, "снимок и его спутник: {purge}");
    let done = f.apply(&purge).await;
    assert_eq!(done["state"], "done", "{done}");

    for gone in [
        purge["items"][0]["path"].as_str().unwrap().to_string(),
        purge["items"][0]["companions"][0]["path"]
            .as_str()
            .unwrap()
            .to_string(),
    ] {
        assert!(!Path::new(&gone).exists(), "осталось в карантине: {gone}");
    }
    // And what was kept is untouched.
    assert!(f.archive.join("a.jpg").exists());
    assert!(f.archive.join("a.xmp").exists());
}

#[tokio::test]
async fn a_finished_job_has_already_let_go_of_the_writer() {
    // "Done" is what the interface waits for before pressing the next button.
    // The terminal state used to be written while the job still held the
    // single-writer gate, so a press in that instant was refused with 409 for
    // work that was over — rare enough to look like a ghost, common enough to
    // fail a test run about once in thirty.
    let f = Fixture::new();
    // Stand in the gap on purpose: the two steps are microseconds apart in
    // real life, which is why this went unexplained for so long.
    jobs::FINISH_PAUSE_MS.store(150, std::sync::atomic::Ordering::Relaxed);
    for round in 0..3 {
        let id = f.start("families", json!({})).await;
        // No sleeping between polls: the point is to arrive exactly at the
        // moment the job reports itself finished.
        loop {
            let (_, j) = f.req("GET", &format!("/api/jobs/{id}"), Value::Null).await;
            if !matches!(j["state"].as_str(), Some("queued" | "running")) {
                assert_eq!(j["state"], "done", "{j}");
                break;
            }
            tokio::task::yield_now().await;
        }
        let (status, v) = f
            .req("POST", "/api/jobs", json!({"kind":"families","params":{}}))
            .await;
        assert_eq!(status, 202, "круг {round}: {v}");
        f.wait(v["job_id"].as_i64().unwrap()).await;
    }
    jobs::FINISH_PAUSE_MS.store(0, std::sync::atomic::Ordering::Relaxed);
}

#[tokio::test]
async fn a_folder_decision_reaches_the_plan_that_folder_asks_for() {
    // "Keep this folder's versions in every group" marks the other encodings
    // by hand. The screen then asks for the plan of that folder — and got an
    // empty one: the narrowing went by the file that *proves* a copy, which a
    // decision made by hand does not have. The versions the user had just set
    // aside were exactly the ones dropped.
    let f = Fixture::new();
    let scans = f.archive.join("scans");
    let exports = f.archive.join("exports");
    std::fs::create_dir_all(&scans).unwrap();
    std::fs::create_dir_all(&exports).unwrap();
    let img = image::RgbImage::from_fn(320, 240, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, 120])
    });
    img.save(scans.join("frame.bmp")).unwrap();
    img.save(exports.join("frame.jpg")).unwrap();

    let id = f
        .start("index", json!({"roots":[f.archive],"min_size":0}))
        .await;
    assert_eq!(f.wait(id).await["state"], "done");
    // Two encodings of one frame are not copies of each other, and nothing
    // groups them automatically. A shared provenance link does — the same
    // thing Lightroom writes when it exports.
    f.state
        .db
        .lock()
        .unwrap()
        .conn
        .execute("UPDATE meta SET xmp_original_id = 'one-source'", [])
        .unwrap();
    let id = f.start("families", json!({})).await;
    assert_eq!(f.wait(id).await["state"], "done");

    // Nothing is a copy here: without a person, this plan stays empty.
    let untouched = f.preview("plan-apply", json!({})).await;
    assert_eq!(untouched["total_files"], 0, "{untouched}");

    let dir = exports.display().to_string();
    let (s, v) = f
        .req(
            "POST",
            "/api/keepers/keep-folder-only",
            json!({ "dir": dir }),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["marked"], 1, "{v}");

    let scoped = f
        .preview("plan-apply", json!({ "keeper_folder": dir }))
        .await;
    assert_eq!(
        scoped["total_files"], 1,
        "план папки потерял отложенное вручную: {scoped}"
    );
    assert_eq!(scoped["items"][0]["manual"], true, "{scoped}");
    assert!(
        scoped["items"][0]["path"]
            .as_str()
            .unwrap()
            .ends_with(".bmp"),
        "{scoped}"
    );
}

#[tokio::test]
async fn a_narrowed_plan_still_says_why_a_file_is_not_moving() {
    // Acting on one group shows that group's plan. Refusals used to be
    // dropped from it wholesale, so a copy that no longer matches the file
    // being kept disappeared from the screen entirely: nothing moved, and
    // nothing said why.
    let f = Fixture::new();
    let a = image::RgbImage::from_fn(240, 180, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, 60])
    });
    let mut encoded = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut encoded)
        .encode_image(&a)
        .unwrap();
    for n in ["a.jpg", "a copy.jpg"] {
        std::fs::write(f.archive.join(n), &encoded).unwrap();
    }
    // A third file joins the group by provenance without sharing a pixel.
    let other = image::RgbImage::from_fn(240, 180, |x, y| {
        image::Rgb([(y % 256) as u8, 30, (x % 256) as u8])
    });
    other.save(f.archive.join("scan.bmp")).unwrap();

    let id = f
        .start("index", json!({"roots":[f.archive],"min_size":0}))
        .await;
    assert_eq!(f.wait(id).await["state"], "done");
    f.state
        .db
        .lock()
        .unwrap()
        .conn
        .execute("UPDATE meta SET xmp_original_id = 'one-source'", [])
        .unwrap();
    let id = f.start("families", json!({})).await;
    assert_eq!(f.wait(id).await["state"], "done");

    let (_, groups) = f.req("GET", "/api/families?limit=10", Value::Null).await;
    let group = groups["families"][0]["id"].as_i64().unwrap();
    let (_, listed) = f
        .req("GET", &format!("/api/families/{group}"), Value::Null)
        .await;
    let bmp = listed["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == "scan.bmp")
        .unwrap_or_else(|| panic!("{listed}"))["file_id"]
        .as_i64()
        .unwrap();

    // Keep the one that shares no pixels with the rest: the others stop being
    // copies of what is kept, and the plan has to say so. Only the kept file
    // changes — nobody has set the others aside, so a refusal is the only
    // thing the plan can say about them.
    let (s, v) = f
        .req(
            "POST",
            &format!("/api/families/{group}/keeper"),
            json!({ "file_id": bmp }),
        )
        .await;
    assert_eq!(s, 200, "{v}");

    let whole = f.preview("plan-apply", json!({"roles":["copy"]})).await;
    let why = |p: &Value| {
        p["refusals"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["path"].as_str().unwrap_or("").to_string())
            .collect::<Vec<_>>()
    };
    assert!(!why(&whole).is_empty(), "{whole}");

    let scoped = f
        .preview("plan-apply", json!({"roles":["copy"],"family_id":group}))
        .await;
    assert_eq!(
        why(&scoped),
        why(&whole),
        "сужение до группы съело объяснения: {scoped}"
    );
}

#[tokio::test]
async fn a_job_will_not_start_while_something_else_is_writing() {
    // The queue is this server's own memory. A command line — or a second
    // server on the same database — writes to the same index and moves the
    // same files, and neither can see that memory.
    let f = Fixture::new();
    let held = pc_core::lock::take_writer(&f.state.db_path, "index").unwrap();

    let (status, v) = f
        .req("POST", "/api/jobs", json!({"kind":"families","params":{}}))
        .await;
    assert_eq!(status, 409, "{v}");
    let said = v["error"].as_str().unwrap_or_default();
    assert!(said.contains("index"), "не сказано, кто держит: {said}");
    // Refused before it was written down: a job nobody ran is not history.
    let (_, jobs) = f.req("GET", "/api/jobs", Value::Null).await;
    assert!(jobs.as_array().unwrap().is_empty(), "{jobs}");

    drop(held);
    let id = f.start("families", json!({})).await;
    assert_eq!(f.wait(id).await["state"], "done");
}

#[tokio::test]
async fn one_mark_settles_the_same_folder_on_every_disk_of_an_array() {
    // The shape this page exists for. On an array the photographs live on
    // three filesystems — a move has to stay on one spindle to be a rename —
    // and their owner sees one folder structure laid across the disks. Told
    // "the originals are in D/разобрано/даня/театр", they mean it about all
    // of them, and the tree has to take that as one sentence.
    let f = Fixture::new();
    let roots: Vec<String> = ["disk1", "disk2"]
        .iter()
        .map(|d| f.archive.join(d).display().to_string())
        .collect();
    let good = "D/разобрано/даня/театр";
    for (n, root) in roots.iter().enumerate() {
        let kept = FsPath::new(root).join(good);
        let loose = FsPath::new(root).join("D/свалка");
        std::fs::create_dir_all(&kept).unwrap();
        std::fs::create_dir_all(&loose).unwrap();
        // A different photograph per disk, so each disk is its own group and
        // the numbers below say how many disks were reached.
        let img = image::RgbImage::from_fn(320, 240, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, (n * 90) as u8])
        });
        let mut encoded = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut encoded)
            .encode_image(&img)
            .unwrap();
        std::fs::write(kept.join("IMG.JPG"), &encoded).unwrap();
        std::fs::write(loose.join("IMG.JPG"), &encoded).unwrap();
    }

    let (s, v) = f
        .req("PUT", "/api/settings", json!({ "roots": roots }))
        .await;
    assert_eq!(s, 200, "{v}");
    let id = f
        .start("index", json!({"roots": roots, "min_size": 0}))
        .await;
    assert_eq!(f.wait(id).await["state"], "done");
    let id = f.start("families", json!({})).await;
    assert_eq!(f.wait(id).await["state"], "done");

    // One tree over two disks: `D` is one node, and it says both hold it.
    let (s, top) = f.req("GET", "/api/tree", Value::Null).await;
    assert_eq!(s, 200, "{top}");
    assert_eq!(top["merged"], true, "{top}");
    assert_eq!(top["files"], 4, "{top}");
    let children = top["node"]["children"].as_array().unwrap();
    assert_eq!(children.len(), 1, "{top}");
    assert_eq!(children[0]["path"], "D", "{top}");
    let labels: Vec<&str> = children[0]["roots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["disk1", "disk2"], "{top}");

    // Marked once, relative to the roots.
    let (s, v) = f
        .req(
            "POST",
            "/api/originals",
            json!({"path": good, "scope": "every-root"}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["groups"], 2, "оба диска не охвачены: {v}");
    assert_eq!(v["moved"], 2, "{v}");
    assert_eq!(
        v["marks"],
        json!([{"path": good, "scope": "every-root"}]),
        "отметка должна быть одна: {v}"
    );

    let kept: Vec<String> = {
        let db = f.state.db.lock().unwrap();
        let mut st = db
            .conn
            .prepare("SELECT f.path FROM families fa JOIN files f ON f.id = fa.keeper_file")
            .unwrap();
        let rows = st
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        rows
    };
    assert_eq!(kept.len(), 2, "{kept:?}");
    for path in &kept {
        assert!(
            path.contains("театр"),
            "хранимым остался не оригинал: {path}"
        );
    }

    // The merged folder knows it is marked, and which disks it lies on.
    let (_, node) = f
        .req(
            "GET",
            &format!("/api/tree?path={}", urlencoding(good)),
            Value::Null,
        )
        .await;
    assert_eq!(node["node"]["marked"], true, "{node}");
    assert_eq!(node["node"]["roots"].as_array().unwrap().len(), 2, "{node}");

    let (_, listing) = f
        .req(
            "GET",
            &format!("/api/tree/files?path={}", urlencoding(good)),
            Value::Null,
        )
        .await;
    assert_eq!(listing["here"], 2, "{listing}");
    for entry in listing["entries"].as_array().unwrap() {
        assert_eq!(entry["original"], true, "{entry}");
    }

    // What follows: the copies on both disks, and only those.
    let plan = f
        .preview("plan-apply", json!({"roles":["copy"],"originals":true}))
        .await;
    assert_eq!(plan["total_files"], 2, "{plan}");
    for item in plan["items"].as_array().unwrap() {
        let path = item["path"].as_str().unwrap();
        assert!(path.contains("свалка"), "уезжает оригинал: {path}");
    }
    let done = f.apply(&plan).await;
    assert_eq!(done["state"], "done", "{done}");
    for root in &roots {
        assert!(!FsPath::new(root).join("D/свалка/IMG.JPG").exists());
        assert!(FsPath::new(root).join(good).join("IMG.JPG").exists());
    }

    // And taking it back leaves nothing marked behind.
    let (s, v) = f
        .req(
            "POST",
            "/api/originals",
            json!({"path": good, "scope": "every-root", "marked": false}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["marks"], json!([]), "{v}");
}

#[tokio::test]
async fn a_single_disk_can_still_be_named_on_its_own() {
    // The other half: one copy of a structure is the good one and the others
    // are not. That is an absolute mark, and it must not reach the rest.
    let f = Fixture::new();
    let roots: Vec<String> = ["disk1", "disk2"]
        .iter()
        .map(|d| f.archive.join(d).display().to_string())
        .collect();
    let img = image::RgbImage::from_fn(240, 180, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, 40])
    });
    let mut encoded = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut encoded)
        .encode_image(&img)
        .unwrap();
    for root in &roots {
        let dir = FsPath::new(root).join("D/фото");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("IMG.JPG"), &encoded).unwrap();
    }

    f.req("PUT", "/api/settings", json!({ "roots": roots }))
        .await;
    let id = f
        .start("index", json!({"roots": roots, "min_size": 0}))
        .await;
    assert_eq!(f.wait(id).await["state"], "done");
    let id = f.start("families", json!({})).await;
    assert_eq!(f.wait(id).await["state"], "done");

    let only = format!("{}/D/фото", roots[0]);
    let (s, v) = f
        .req(
            "POST",
            "/api/originals",
            json!({"path": only, "scope": "absolute"}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["groups"], 1, "{v}");

    let keeper: String = f
        .state
        .db
        .lock()
        .unwrap()
        .conn
        .query_row(
            "SELECT f.path FROM families fa JOIN files f ON f.id = fa.keeper_file",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(keeper.starts_with(&roots[0]), "{keeper}");

    // The merged node is not marked: this sentence was about one disk.
    let (_, node) = f
        .req(
            "GET",
            &format!("/api/tree?path={}", urlencoding("D/фото")),
            Value::Null,
        )
        .await;
    assert_eq!(node["node"]["marked"], false, "{node}");
    let per_root = node["node"]["roots"].as_array().unwrap();
    assert_eq!(per_root[0]["marked"], true, "{node}");
    assert_eq!(per_root[1]["marked"], false, "{node}");

    let plan = f
        .preview("plan-apply", json!({"roles":["copy"],"originals":true}))
        .await;
    assert_eq!(plan["total_files"], 1, "{plan}");
    assert!(plan["items"][0]["path"]
        .as_str()
        .unwrap()
        .starts_with(&roots[1]));
}

#[tokio::test]
async fn a_plan_of_the_originals_is_empty_when_no_folder_is_marked() {
    // The accident this must never have: a mark taken back, and a plan that
    // quietly widens from "the copies of these folders" to "the archive".
    let f = Fixture::new();
    let img = image::RgbImage::from_fn(240, 180, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, 30])
    });
    let mut encoded = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut encoded)
        .encode_image(&img)
        .unwrap();
    for n in ["a.jpg", "a copy.jpg"] {
        std::fs::write(f.archive.join(n), &encoded).unwrap();
    }
    let id = f
        .start("index", json!({"roots":[f.archive],"min_size":0}))
        .await;
    assert_eq!(f.wait(id).await["state"], "done");
    let id = f.start("families", json!({})).await;
    assert_eq!(f.wait(id).await["state"], "done");

    let whole = f.preview("plan-apply", json!({"roles":["copy"]})).await;
    assert_eq!(whole["total_files"], 1, "{whole}");
    let scoped = f
        .preview("plan-apply", json!({"roles":["copy"],"originals":true}))
        .await;
    assert_eq!(scoped["total_files"], 0, "{scoped}");
}

/// Percent-encoding for the one character a temporary path can contain that a
/// query string reads as something else. The fixture's paths are otherwise
/// plain, so a full encoder would be more machinery than the question needs.
fn urlencoding(path: &str) -> String {
    path.replace('%', "%25").replace(' ', "%20")
}

#[tokio::test]
async fn a_hand_made_change_waits_for_whoever_else_is_writing() {
    // The gate in this server's memory says nothing about the command line or
    // a second server on the same database. Jobs have always taken the
    // operating system's lock; the hand-made changes went straight past it,
    // which left "one writer at a time" true only of the slow half.
    let f = Fixture::new();
    let held = pc_core::lock::take_writer(&f.state.db_path, "another process").unwrap();

    for (method, path, body) in [
        ("POST", "/api/reset", json!({"confirmation": "RESET"})),
        ("PUT", "/api/settings", json!({"phash_max": 8})),
        (
            "POST",
            "/api/keepers/prefer-folder",
            json!({"dir": "/foto"}),
        ),
    ] {
        let (status, v) = f.req(method, path, body).await;
        assert_eq!(status, 409, "{path} wrote past another writer: {v}");
        assert!(
            v["error"].as_str().unwrap_or_default().contains("another"),
            "{path}: {v}"
        );
    }

    // And once it lets go, the same calls go through.
    drop(held);
    let (status, v) = f.req("PUT", "/api/settings", json!({"phash_max": 8})).await;
    assert_eq!(status, 200, "{v}");
}

#[tokio::test]
async fn a_second_server_leaves_a_live_job_of_the_first_alone() {
    // Starting up, a server marks whatever is still "running" as interrupted:
    // it must be its own work from a previous life. Not if another process is
    // holding the writer — that job is running right now, and calling it
    // interrupted is a lie told about a job that is moving files.
    let f = Fixture::new();
    {
        let db = f.state.db.lock().unwrap();
        db.conn
            .execute(
                "INSERT INTO jobs(kind,params,state,started_at) VALUES('index','{}','running',1)",
                [],
            )
            .unwrap();
    }
    let held = pc_core::lock::take_writer(&f.state.db_path, "another process").unwrap();

    let second = AppState::new(&f.state.db_path, &f._tmp.path().join("t2"), None).unwrap();
    let state: String = second
        .db
        .lock()
        .unwrap()
        .conn
        .query_row("SELECT state FROM jobs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(state, "running", "чужая живая задача объявлена прерванной");

    drop(held);
    let third = AppState::new(&f.state.db_path, &f._tmp.path().join("t3"), None).unwrap();
    let state: String = third
        .db
        .lock()
        .unwrap()
        .conn
        .query_row("SELECT state FROM jobs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(state, "interrupted", "своя брошенная задача не подобрана");
}

#[tokio::test]
async fn a_tree_with_no_roots_configured_hands_back_paths_that_can_be_marked() {
    // A database indexed from the command line has no roots in its settings —
    // that is where the roots of a *run* live — and the tree is then read
    // against nothing: a path relative to nothing is itself. It has to stay
    // itself, leading separator and all. Trimming it produced a folder whose
    // "mark this disk" button carried a path no file was ever called, so the
    // node looked covered and the rule covered nothing.
    let f = Fixture::new();
    let dir = f.archive.join("shots");
    std::fs::create_dir_all(&dir).unwrap();
    let img = image::RgbImage::from_fn(64, 64, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, 10])
    });
    img.save(dir.join("a.png")).unwrap();
    let id = f
        .start("index", json!({"roots":[f.archive],"min_size":0}))
        .await;
    assert_eq!(f.wait(id).await["state"], "done");

    // Asked for by the path the tree itself hands back, the way the browser
    // asks once a folder is opened.
    let (_, node) = f
        .req(
            "GET",
            &format!(
                "/api/tree?depth=0&path={}",
                urlencoding(&dir.display().to_string())
            ),
            Value::Null,
        )
        .await;
    let node = node["node"].clone();
    let full = node["roots"][0]["full"].as_str().unwrap().to_string();
    assert_eq!(full, dir.display().to_string(), "{node}");

    // And a mark made with that path covers the file that is actually there.
    let (s, v) = f
        .req(
            "POST",
            "/api/originals",
            json!({"path": full, "scope": "absolute"}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let marks = f.state.db.lock().unwrap().original_marks().unwrap();
    assert!(
        marks
            .covering(&dir.join("a.png").display().to_string())
            .is_some(),
        "отметка из дерева не накрывает свой же файл"
    );
}

#[tokio::test]
async fn an_interrupted_move_can_be_read_against_the_disk_and_undone() {
    // A killed process leaves a `pending` row: the list of what it meant to
    // move, and no word on how far it got. It is rightly not offered as a
    // whole reversible operation — it is not one — and that used to be the
    // end of it. "Check the journal" is advice, not an operation.
    let f = Fixture::new();
    let src = f.archive.join("a.jpg");
    let dst = f.quarantine.join("a.jpg");
    std::fs::write(&dst, b"a photograph").unwrap();
    let (s, d) = (src.display().to_string(), dst.display().to_string());
    let id = {
        let db = f.state.db.lock().unwrap();
        let run = db.start_run(&[], "test").unwrap();
        db.journal_begin(&pc_db::NewJournalEntry {
            run_id: run,
            op: "quarantine-file",
            target_id: None,
            src: &s,
            dst: Some(&d),
            size: 12,
            file_count: 1,
            manifest: &[pc_db::Moved {
                src: s.clone(),
                dst: d.clone(),
            }],
        })
        .unwrap()
    };

    let plan = f
        .preview("journal-reconcile", json!({ "journal_id": id }))
        .await;
    assert_eq!(plan["total_files"], 1, "{plan}");
    assert_eq!(plan["items"][0]["dst"], json!(s), "{plan}");
    let done = f.apply(&plan).await;
    assert_eq!(done["state"], "done", "{done}");
    assert!(src.exists(), "снимок не вернулся");
    assert!(!dst.exists());
    let entry = f
        .state
        .db
        .lock()
        .unwrap()
        .journal_entry(id)
        .unwrap()
        .unwrap();
    assert_eq!(entry.status, pc_db::JournalStatus::Undone);
}

#[tokio::test]
async fn an_interrupted_move_whose_file_is_in_two_places_is_left_to_a_person() {
    // Two files, and choosing between them is not the tool's decision. It
    // says so and changes nothing — including the entry, which stays the
    // `pending` it truthfully is.
    let f = Fixture::new();
    let src = f.archive.join("a.jpg");
    let dst = f.quarantine.join("a.jpg");
    std::fs::write(&src, b"one of them").unwrap();
    std::fs::write(&dst, b"the other").unwrap();
    let (s, d) = (src.display().to_string(), dst.display().to_string());
    let id = {
        let db = f.state.db.lock().unwrap();
        let run = db.start_run(&[], "test").unwrap();
        db.journal_begin(&pc_db::NewJournalEntry {
            run_id: run,
            op: "quarantine-file",
            target_id: None,
            src: &s,
            dst: Some(&d),
            size: 11,
            file_count: 1,
            manifest: &[pc_db::Moved {
                src: s.clone(),
                dst: d.clone(),
            }],
        })
        .unwrap()
    };

    let plan = f
        .preview("journal-reconcile", json!({ "journal_id": id }))
        .await;
    assert_eq!(plan["total_files"], 0, "{plan}");
    assert_eq!(plan["refusals"].as_array().unwrap().len(), 1, "{plan}");
    assert_eq!(std::fs::read(&src).unwrap(), b"one of them");
    assert_eq!(std::fs::read(&dst).unwrap(), b"the other");
    assert_eq!(
        f.state
            .db
            .lock()
            .unwrap()
            .journal_entry(id)
            .unwrap()
            .unwrap()
            .status,
        pc_db::JournalStatus::Pending
    );
}

#[tokio::test]
async fn a_gathered_quarantine_says_where_its_files_came_from() {
    // Quarantine gathered in one folder holds `<disk label>/<path from that
    // disk>`, and a label is a short name that means nothing on its own. The
    // way home used to be guessed from the path, which brought a file back to
    // `collected/root/<its own old absolute path>` — bytes intact, address
    // invented. Now the layout is written down beside the data, and a
    // database is not needed to read it back.
    let f = Fixture::new();
    let collected = f._tmp.path().join("collected");
    let state = Arc::new(
        AppState::new(
            &f._tmp.path().join("gathered.db"),
            &f._tmp.path().join("t"),
            Some(collected.clone()),
        )
        .unwrap(),
    );
    let img = image::RgbImage::from_fn(64, 64, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, 7])
    });
    std::fs::create_dir_all(f.archive.join("b")).unwrap();
    let home = f.archive.join("b/frame.png");
    img.save(&home).unwrap();
    std::fs::copy(&home, f.archive.join("b/frame copy.png")).unwrap();

    let other = Fixture { state, ..f };
    let id = other
        .start("index", json!({"roots":[other.archive],"min_size":0}))
        .await;
    assert_eq!(other.wait(id).await["state"], "done");
    let id = other.start("families", json!({})).await;
    assert_eq!(other.wait(id).await["state"], "done");
    let plan = other.preview("plan-apply", json!({"roles":["copy"]})).await;
    assert_eq!(plan["total_files"], 1, "{plan}");
    assert_eq!(other.apply(&plan).await["state"], "done");

    // Hidden under the name the walk steps over, so a later scan cannot index
    // quarantined files as photographs of the archive.
    let inside = collected.join(pc_core::QUARANTINE_DIR);
    assert!(
        inside.is_dir(),
        "собранный карантин не помечен как карантин"
    );
    assert!(inside.join(pc_core::QUARANTINE_LAYOUT).is_file());

    // And whatever landed there knows its way home, without the database.
    let moved = walk(&inside)
        .into_iter()
        .find(|p| p.extension().is_some_and(|e| e == "png"))
        .expect("файл в карантине");
    let origin = pc_core::quarantine_origin_of(&moved.display().to_string());
    assert_eq!(
        origin.as_deref(),
        Some(
            other
                .archive
                .join("b/frame copy.png")
                .display()
                .to_string()
                .as_str()
        ),
        "{moved:?}"
    );
}

/// Every file under a directory, for looking at what an operation left.
fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}
