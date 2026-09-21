use super::*;
use axum::{
    body::{to_bytes, Body},
    http::Request,
};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
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
