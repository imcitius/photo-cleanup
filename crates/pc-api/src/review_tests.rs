use super::*;
use pc_family::{plan, review};

fn group(f: &Fixture, name: &str, exact: bool) -> (i64, i64, i64) {
    let db = f.state.db.lock().unwrap();
    let run = db.start_run(&[], "review-test").unwrap();
    let mut ids = Vec::new();
    for (n, hash) in [(0, 17), (1, if exact { 17 } else { 39 })] {
        let path = f.archive.join(format!("{name}/{n}.jpg"));
        ids.push(
            db.upsert_file(
                &pc_db::NewFile {
                    path: path.display().to_string(),
                    name: format!("{n}.jpg"),
                    disk: "disk1".into(),
                    size: 7832914,
                    mtime: 1723117843,
                    container: Some("jpeg".into()),
                    content_hash: Some(vec![hash; 32]),
                    ..Default::default()
                },
                run,
            )
            .unwrap(),
        );
    }
    let id = db
        .insert_family("linked", None, None, Some(ids[0]), run)
        .unwrap();
    for (file, role) in [(ids[0], "original"), (ids[1], "copy")] {
        db.insert_family_member(id, file, role, None, 1.0, "test")
            .unwrap();
    }
    (id, ids[0], ids[1])
}
async fn choose(f: &Fixture, id: i64, state: &str) -> Value {
    let (_, queue) = f.req("GET", "/api/review?queue=all", Value::Null).await;
    let g = queue["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["id"] == id)
        .unwrap();
    let (s, response) = f
        .req(
            "POST",
            &format!("/api/review/{id}"),
            json!({"state":state,"token":g["review_token"]}),
        )
        .await;
    assert_eq!(s, 200, "{response}");
    response
}

#[tokio::test]
async fn review_choices_preserve_evidence_and_survive_rebuilding() {
    let f = Fixture::new();
    let (id, a, b) = group(&f, "Данька", true);
    group(&f, "Versions", false);
    let (_, queue) = f
        .req("GET", "/api/review?search=данька&limit=1", Value::Null)
        .await;
    assert_eq!(queue["total"], 1);
    assert_eq!(queue["groups"][0]["can_plan"], true);
    let stale = queue["groups"][0]["review_token"].clone();
    let saved = choose(&f, id, "keep").await;
    let (s, _) = f
        .req(
            "POST",
            &format!("/api/review/{id}"),
            json!({"state":"plan","token":stale}),
        )
        .await;
    assert_eq!(s, 400);
    {
        let db = f.state.db.lock().unwrap();
        assert!(plan::compute(&db, &plan::Policy::default())
            .unwrap()
            .candidates
            .is_empty());
        assert_eq!(db.review_choices().unwrap().len(), 2);
    }
    let (s, _) = f
        .req(
            "POST",
            "/api/review/undo",
            json!({"operation":saved["operation"]}),
        )
        .await;
    assert_eq!(s, 200);
    choose(&f, id, "plan").await;
    let db = f.state.db.lock().unwrap();
    let approved =
        || review::reviewed_plan(&db, &plan::Policy::default(), &plan::Scope::default()).unwrap();
    assert_eq!(approved().candidates[0].file_id, b);
    // Rebuilding changes the family id, not the evidence or the decision.
    db.conn
        .execute("DELETE FROM family_members WHERE family_id=?1", [id])
        .unwrap();
    db.conn
        .execute("DELETE FROM families WHERE id=?1", [id])
        .unwrap();
    let run = db.start_run(&[], "rebuild").unwrap();
    let new_id = db
        .insert_family("linked", None, None, Some(a), run)
        .unwrap();
    for (file, role) in [(a, "original"), (b, "copy")] {
        db.insert_family_member(new_id, file, role, None, 1.0, "test")
            .unwrap();
    }
    assert_eq!(approved().candidates.len(), 1);
    // Even equal pixel evidence must not approve a file that changed later.
    db.conn
        .execute("UPDATE files SET mtime=mtime+17 WHERE id=?1", [b])
        .unwrap();
    assert!(approved().candidates.is_empty());
    assert_eq!(
        review::groups(&db, Some(new_id)).unwrap()[0].state,
        "pending"
    );
    let policy = plan::Policy {
        remove_roles: [pc_family::Role::Original].into_iter().collect(),
        ..Default::default()
    };
    assert!(plan::compute(&db, &policy).is_err());
}

#[tokio::test]
async fn batch_is_previewed_atomic_and_does_not_override_prior_choices() {
    let f = Fixture::new();
    let (first, _, _) = group(&f, "First", true);
    let (second, _, _) = group(&f, "Second", true);
    let (version, _, _) = group(&f, "Different", false);
    let (_, before) = f.req("POST", "/api/review/batch-preview", json!({})).await;
    assert_eq!(before["groups"], 2);
    choose(&f, first, "defer").await;
    let (s, _) = f
        .req(
            "POST",
            "/api/review/batch",
            json!({"token":before["token"]}),
        )
        .await;
    assert_eq!(s, 400);
    let (_, preview) = f.req("POST", "/api/review/batch-preview", json!({})).await;
    assert_eq!(preview["groups"], 1);
    assert_eq!(preview["bytes"], 7832914);
    let (s, saved) = f
        .req(
            "POST",
            "/api/review/batch",
            json!({"token":preview["token"]}),
        )
        .await;
    assert_eq!(s, 200, "{saved}");
    let (_, queue) = f.req("GET", "/api/review?queue=all", Value::Null).await;
    let g = queue["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["id"] == version)
        .unwrap();
    assert_eq!(g["can_plan"], false);
    let (s, _) = f
        .req(
            "POST",
            &format!("/api/review/{version}"),
            json!({"state":"plan","token":g["review_token"]}),
        )
        .await;
    assert_eq!(s, 400);
    let later = choose(&f, second, "keep").await;
    let (s, _) = f
        .req(
            "POST",
            "/api/review/undo",
            json!({"operation":saved["operation"]}),
        )
        .await;
    assert_eq!(s, 400);
    for op in [&later, &saved] {
        let (s, _) = f
            .req(
                "POST",
                "/api/review/undo",
                json!({"operation":op["operation"]}),
            )
            .await;
        assert_eq!(s, 200);
    }
    let (_, queue) = f
        .req("GET", "/api/review?queue=pending&kind=exact", Value::Null)
        .await;
    assert_eq!(queue["total"], 1);
    assert_eq!(queue["groups"][0]["id"], second);
    let (_, jobs) = f.req("GET", "/api/jobs", Value::Null).await;
    assert!(
        jobs.as_array().unwrap().is_empty(),
        "A decision must not start a move"
    );
}

#[tokio::test]
async fn later_manual_choice_and_moved_file_cannot_be_overwritten_by_undo() {
    let f = Fixture::new();
    let (id, _, b) = group(&f, "Copies", true);
    let saved = choose(&f, id, "keep").await;
    let (s, _) = f
        .req(
            "POST",
            &format!("/api/files/{b}/reject"),
            json!({"rejected":true}),
        )
        .await;
    assert_eq!(s, 200);
    let (s, _) = f
        .req(
            "POST",
            "/api/review/undo",
            json!({"operation":saved["operation"]}),
        )
        .await;
    assert_eq!(s, 400);
    {
        let db = f.state.db.lock().unwrap();
        assert_eq!(
            plan::compute(&db, &plan::Policy::default())
                .unwrap()
                .candidates[0]
                .file_id,
            b
        );
    }
    let saved = choose(&f, id, "defer").await;
    f.state
        .db
        .lock()
        .unwrap()
        .conn
        .execute("UPDATE files SET state='quarantined' WHERE id=?1", [b])
        .unwrap();
    let (s, _) = f
        .req(
            "POST",
            "/api/review/undo",
            json!({"operation":saved["operation"]}),
        )
        .await;
    assert_eq!(s, 400);
}

#[tokio::test]
async fn queue_writer_lock_and_pagination_are_enforced() {
    let f = Fixture::new();
    let (id, _, _) = group(&f, "Copies", true);
    let (_, queue) = f
        .req("GET", "/api/review?offset=9999&limit=3", Value::Null)
        .await;
    assert_eq!(queue["offset"], 0);
    assert_eq!(queue["groups"].as_array().unwrap().len(), 1);
    *f.state.jobs.active.lock().unwrap() = Some((42, pc_core::work::Control::default()));
    let (s, _) = f
        .req(
            "POST",
            &format!("/api/review/{id}"),
            json!({"state":"keep","token":queue["groups"][0]["review_token"]}),
        )
        .await;
    assert_eq!(s, 409);
    assert!(f
        .state
        .db
        .lock()
        .unwrap()
        .review_choices()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn ten_thousand_groups_return_only_the_requested_window() {
    let f = Fixture::new();
    {
        let db = f.state.db.lock().unwrap();
        let tx = db.conn.unchecked_transaction().unwrap();
        let run = db.start_run(&[], "large-review").unwrap();
        for i in 0..10000 {
            let mut members = Vec::new();
            for n in 0..2 {
                members.push(
                    db.upsert_file(
                        &pc_db::NewFile {
                            path: format!("/archive/{i}/{n}.jpg"),
                            name: format!("{i}-{n}.jpg"),
                            size: 7832914,
                            mtime: 1723117843,
                            disk: "disk1".into(),
                            container: Some("jpeg".into()),
                            content_hash: Some(vec![17; 32]),
                            ..Default::default()
                        },
                        run,
                    )
                    .unwrap(),
                );
            }
            let id = db
                .insert_family("linked", None, None, Some(members[0]), run)
                .unwrap();
            for (file, role) in [(members[0], "original"), (members[1], "copy")] {
                db.insert_family_member(id, file, role, None, 1.0, "test")
                    .unwrap();
            }
        }
        tx.commit().unwrap();
    }
    let started = std::time::Instant::now();
    let (status, queue) = f
        .req(
            "GET",
            "/api/review?queue=pending&offset=9997&limit=3",
            Value::Null,
        )
        .await;
    eprintln!("10,000 group queue: {:?}", started.elapsed());
    assert_eq!(status, 200);
    assert_eq!(queue["total"], 10000);
    assert_eq!(queue["offset"], 9997);
    assert_eq!(queue["groups"].as_array().unwrap().len(), 3);
    assert!(
        queue.to_string().len() < 16000,
        "The response must not expand the entire archive"
    );
    assert_eq!(queue["groups"][0]["members"][0]["name"], "9997-0.jpg");
}

#[tokio::test]
async fn my_decisions_include_existing_keepers_rejections_and_originals_rules() {
    let f = Fixture::new();
    let (hand, a, b) = group(&f, "Chosen", true);
    let (_, _, copy) = group(&f, "Originals", true);
    let (versions, original, other) = group(&f, "Versions", false);
    let (_, _, untouched) = group(&f, "Automatic", true);
    // Preview checks the source filesystem before choosing a quarantine path.
    for name in ["Chosen", "Originals", "Versions", "Automatic"] {
        let dir = f.archive.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        for n in 0..2 {
            std::fs::write(dir.join(format!("{n}.jpg")), b"preview fixture").unwrap();
        }
    }

    {
        let db = f.state.db.lock().unwrap();
        db.set_manual_keeper(hand, a).unwrap();
        db.set_manual_keeper(versions, original).unwrap();
        db.mark_original(
            &f.archive.join("Originals").display().to_string(),
            pc_db::MarkScope::Absolute,
        )
        .unwrap();
    }
    let p = f
        .preview("plan-apply", json!({"roles":["copy"],"reviewed_only":true}))
        .await;
    let ids: Vec<_> = p["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["file_id"].as_i64().unwrap())
        .collect();
    assert!(ids.contains(&b), "{p}");
    assert!(ids.contains(&copy));
    assert!(!ids.contains(&untouched));
    assert!(
        !ids.contains(&other),
        "Choosing a keeper must not approve different pixels"
    );
    assert!(!p["refusals"].as_array().unwrap().is_empty());
    let (s, _) = f
        .req(
            "POST",
            &format!("/api/files/{other}/reject"),
            json!({"rejected":true}),
        )
        .await;
    assert_eq!(s, 200);
    let p = f
        .preview("plan-apply", json!({"roles":["copy"],"reviewed_only":true}))
        .await;
    assert!(p["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["file_id"] == other));
    // A later keep/defer is stronger than either an old rejection or a folder rule.
    choose(&f, hand, "keep").await;
    choose(&f, versions, "defer").await;
    let p = f
        .preview("plan-apply", json!({"roles":["copy"],"reviewed_only":true}))
        .await;
    assert_eq!(p["items"].as_array().unwrap().len(), 1);
    assert_eq!(p["items"][0]["file_id"], copy);
    let (_, summary) = f.req("GET", "/api/review/decisions", Value::Null).await;
    assert_eq!(summary["keep"], 1);
    assert_eq!(summary["defer"], 1);
    assert_eq!(summary["manual_keepers"], 2);
    assert_eq!(summary["manual_rejects"], 1);
    assert_eq!(summary["folders"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn folder_queue_merges_roots_and_filters_at_path_boundaries() {
    let f = Fixture::new();
    let (first, _, _) = group(&f, "disk1/D/Photo", true);
    group(&f, "disk2/D/Photo", true);
    group(&f, "disk2/D/Photographs", true);
    {
        let db = f.state.db.lock().unwrap();
        db.conn
            .execute(
                "INSERT OR REPLACE INTO settings(key,value) VALUES('roots',?1)",
                [json!([f.archive.join("disk1"), f.archive.join("disk2")]).to_string()],
            )
            .unwrap();
        // A singleton explains nonconsecutive group ids, but belongs in neither queue nor folder counts.
        let run = db.start_run(&[], "singleton").unwrap();
        let file = db
            .upsert_file(
                &pc_db::NewFile {
                    path: f.archive.join("disk1/D/Alone/1.jpg").display().to_string(),
                    name: "1.jpg".into(),
                    ..Default::default()
                },
                run,
            )
            .unwrap();
        let family = db
            .insert_family("linked", None, None, Some(file), run)
            .unwrap();
        db.insert_family_member(family, file, "original", None, 1.0, "")
            .unwrap();
    }
    let (_, tree) = f
        .req("GET", "/api/review?folders=true&queue=all", Value::Null)
        .await;
    let nodes = tree["folders"].as_array().unwrap();
    assert_eq!(
        nodes.iter().find(|v| v["path"] == "D/Photo").unwrap()["groups"],
        2
    );
    assert!(!nodes.iter().any(|v| v["path"] == "D/Alone"));
    let (_, queue) = f
        .req("GET", "/api/review?folder=D%2FPhoto&queue=all", Value::Null)
        .await;
    assert_eq!(queue["total"], 2);
    assert_eq!(queue["groups"][0]["id"], first);
    let (_, none) = f
        .req(
            "GET",
            "/api/review?folder=D%2FMissing&queue=all",
            Value::Null,
        )
        .await;
    assert_eq!(none["total"], 0);
}
