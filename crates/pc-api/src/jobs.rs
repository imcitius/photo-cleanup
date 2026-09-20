use crate::{service, AppState};
use anyhow::{bail, Context, Result};
use axum::{
    extract::{Path, Query, State},
    response::{sse::Event, IntoResponse, Response, Sse},
    Json,
};
use pc_core::work::Control;
use pc_db::Db;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    convert::Infallible,
    sync::{atomic::Ordering, Arc, Mutex},
    time::Duration,
};

#[derive(Default)]
pub struct Jobs {
    pub active: Mutex<Option<(i64, Control)>>,
}
#[derive(Clone, Deserialize, Serialize)]
pub struct Request {
    pub kind: String,
    #[serde(default = "empty")]
    pub params: Value,
    pub plan_token: Option<String>,
    pub confirmation: Option<String>,
}
fn empty() -> Value {
    json!({})
}
pub fn destructive(kind: &str) -> bool {
    matches!(
        kind,
        "plan-apply"
            | "derived-clean"
            | "derived-purge"
            | "organize-apply"
            | "organize-undo"
            | "journal-undo"
    )
}
pub fn idle(st: &AppState) -> Result<()> {
    if let Some((id, _)) = &*st.jobs.active.lock().unwrap() {
        bail!("Задача №{id} уже выполняется. Дождитесь окончания или остановите её.");
    }
    Ok(())
}
pub fn rows(db: &Db, sql: &str, args: &[&dyn rusqlite::ToSql]) -> Result<Vec<Value>> {
    let mut stmt = db.conn.prepare(sql)?;
    let names = stmt
        .column_names()
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let values = stmt
        .query_map(args, |row| {
            let mut out = serde_json::Map::new();
            for (i, name) in names.iter().enumerate() {
                use rusqlite::types::ValueRef;
                let v = match row.get_ref(i)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(n) => json!(n),
                    ValueRef::Real(n) => json!(n),
                    ValueRef::Text(s) => {
                        let s = String::from_utf8_lossy(s);
                        if matches!(name.as_str(), "params" | "progress" | "roots") {
                            serde_json::from_str(&s).unwrap_or(json!(s))
                        } else {
                            json!(s)
                        }
                    }
                    ValueRef::Blob(_) => Value::Null,
                };
                out.insert(name.clone(), v);
            }
            Ok(Value::Object(out))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(values)
}
pub async fn list(
    State(st): State<Arc<AppState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    service::respond((|| {
        let db = st.db.lock().unwrap();
        let state = q.get("state").cloned().unwrap_or_default();
        let mut jobs = rows(
            &db,
            "SELECT * FROM jobs WHERE (?1='' OR state=?1) ORDER BY id DESC LIMIT 200",
            &[&state],
        )?;
        if let Some((id, c)) = &*st.jobs.active.lock().unwrap() {
            for j in &mut jobs {
                if j["id"] == *id {
                    j["progress"] = json!(*c.progress.lock().unwrap());
                }
            }
        }
        Ok(json!(jobs))
    })())
}
pub fn get(st: &AppState, id: i64) -> Result<Value> {
    let db = st.db.lock().unwrap();
    let mut j = rows(&db, "SELECT * FROM jobs WHERE id=?1", &[&id])?
        .pop()
        .context("Задача не найдена")?;
    if let Some((active, c)) = &*st.jobs.active.lock().unwrap() {
        if *active == id {
            j["progress"] = json!(*c.progress.lock().unwrap());
        }
    }
    Ok(j)
}
pub async fn detail(State(st): State<Arc<AppState>>, Path(id): Path<i64>) -> Response {
    service::respond(get(&st, id))
}
pub async fn cancel(State(st): State<Arc<AppState>>, Path(id): Path<i64>) -> Response {
    let active = st.jobs.active.lock().unwrap();
    if let Some((active, c)) = &*active {
        if *active == id {
            c.cancel.store(true, Ordering::Relaxed);
            return Json(json!({"ok":true})).into_response();
        }
    }
    service::error(409, "Задача уже завершена или была прервана перезапуском")
}
pub async fn events(State(st): State<Arc<AppState>>, Path(id): Path<i64>) -> Response {
    if get(&st, id).is_err() {
        return service::error(404, "Задача не найдена");
    }
    Sse::new(async_stream::stream! {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tick.tick().await;
            match get(&st,id) {
                Ok(j) => {
                    let finished = !matches!(j["state"].as_str(), Some("running" | "queued"));
                    yield Ok::<_,Infallible>(Event::default().data(j.to_string()));
                    if finished { break; }
                },
                Err(_) => break,
            }
        }
    })
    .keep_alive(axum::response::sse::KeepAlive::default())
    .into_response()
}
pub async fn start(State(st): State<Arc<AppState>>, Json(req): Json<Request>) -> Response {
    // Reserve under the same lock as every manual mutation. No second writer
    // can pass the gate while a worker is being installed.
    let _gate = st.mutation.lock().unwrap();
    if let Err(e) = idle(&st) {
        return service::error(409, &e.to_string());
    }
    if let Err(e) = service::validate(&req) {
        return service::error(400, &format!("{e:#}"));
    }
    if destructive(&req.kind) && req.plan_token.is_none() {
        return service::error(400, "Сначала откройте предпросмотр плана");
    }
    if req.kind == "derived-purge" && req.confirmation.as_deref() != Some("УДАЛИТЬ") {
        return service::error(400, "Для окончательного удаления введите УДАЛИТЬ");
    }
    let result = (|| -> Result<i64> {
        let db = st.db.lock().unwrap();
        db.conn.execute(
            "INSERT INTO jobs(kind,params,state,started_at) VALUES(?1,?2,'queued',?3)",
            rusqlite::params![req.kind, req.params.to_string(), pc_core::time::now_unix()],
        )?;
        Ok(db.conn.last_insert_rowid())
    })();
    let id = match result {
        Ok(id) => id,
        Err(e) => return service::error(500, &e.to_string()),
    };
    let control = Control::default();
    *st.jobs.active.lock().unwrap() = Some((id, control.clone()));
    drop(_gate);
    let monitor = st.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let snapshot = {
                let active = monitor.jobs.active.lock().unwrap();
                active
                    .as_ref()
                    .filter(|(job, _)| *job == id)
                    .map(|(_, c)| json!(*c.progress.lock().unwrap()).to_string())
            };
            let Some(snapshot) = snapshot else { break };
            let path = monitor.db_path.clone();
            let _ = tokio::task::spawn_blocking(move || {
                if let Ok(db) = Db::open(&path) {
                    let _ = db.conn.busy_timeout(Duration::from_millis(100));
                    let _ = db.conn.execute(
                        "UPDATE jobs SET progress=?1 WHERE id=?2 AND state='running'",
                        rusqlite::params![snapshot, id],
                    );
                }
            })
            .await;
        }
    });
    tokio::task::spawn_blocking(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            execute(&st, id, &req, &control)
        }));
        let result = result.unwrap_or_else(|_| {
            Err(anyhow::anyhow!(
                "Внутренняя ошибка задачи; проверьте незавершённые записи журнала"
            ))
        });
        // Opening a fresh connection also rolls back an interrupted metadata
        // transaction before recording the terminal state.
        let state = match &result {
            Ok(_) => "done",
            Err(e) if e.is::<pc_core::work::Cancelled>() => "cancelled",
            Err(_) => "failed",
        };
        if let Ok(db) = Db::open(&st.db_path) {
            let err = result.err().map(|e| format!("{e:#}"));
            let _ = db.conn.execute(
                "UPDATE jobs SET state=?1,finished_at=?2,error=?3,progress=?4 WHERE id=?5",
                rusqlite::params![
                    state,
                    pc_core::time::now_unix(),
                    err,
                    json!(*control.progress.lock().unwrap()).to_string(),
                    id
                ],
            );
        }
        *st.jobs.active.lock().unwrap() = None;
    });
    (axum::http::StatusCode::ACCEPTED, Json(json!({"job_id":id}))).into_response()
}
fn execute(st: &AppState, id: i64, req: &Request, control: &Control) -> Result<()> {
    let db = Db::open(&st.db_path)?;
    db.conn.busy_timeout(Duration::from_secs(5))?;
    db.conn
        .execute("UPDATE jobs SET state='running' WHERE id=?1", [id])?;
    let settings = service::settings_value(st, &db)?;
    service::apply_language(&settings);
    let roots: Vec<std::path::PathBuf> = req.params["roots"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(Into::into))
                .collect()
        })
        .unwrap_or_default();
    let mut run_id = None;
    // Disk operations use exactly the reviewed set, then recheck each file at
    // the point of mutation. A stale plan is refused before the first rename.
    if destructive(&req.kind) {
        let (preview, actions) = service::make_preview(st, &db, req)?;
        if req.plan_token.as_deref() != preview["token"].as_str() {
            bail!("План изменился. Обновите предпросмотр и проверьте числа ещё раз.");
        }
        let run = db.start_run(&[], env!("CARGO_PKG_VERSION"))?;
        run_id = Some(run);
        db.conn
            .execute("UPDATE jobs SET run_id=?1 WHERE id=?2", [run, id])?;
        control.begin(
            "Выполнение плана",
            actions.len() as u64,
            preview["total_bytes"].as_u64().unwrap_or(0),
        )?;
        let reviewed_bytes: HashMap<String, u64> = preview["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| {
                (
                    v["path"].as_str().unwrap_or("").to_string(),
                    v["size"].as_u64().unwrap_or(0),
                )
            })
            .collect();
        for action in actions {
            control.current(action.source_path())?;
            let bytes = reviewed_bytes
                .get(action.source_path())
                .copied()
                .unwrap_or(action.size());
            if let Err(e) = service::apply_action(st, &db, run, &action, req, control) {
                if e.is::<pc_core::work::Cancelled>() {
                    return Err(e);
                }
                control.refuse(action.path(), &format!("{e:#}"));
            }
            control.advance(bytes, None);
            db.conn.execute(
                "UPDATE jobs SET progress=?1 WHERE id=?2",
                rusqlite::params![json!(*control.progress.lock().unwrap()).to_string(), id],
            )?;
        }
        db.finish_run(run)?;
    } else {
        // Every read-only stage is one closure, so "run the lot" is the same
        // code path as running them one at a time — there is no second
        // pipeline to keep in step with this one.
        let scan = || -> Result<()> {
            pc_work::scan::run_controlled(&db, &roots, env!("CARGO_PKG_VERSION"), control)
        };
        let index = || -> Result<()> {
            pc_work::index::run_controlled(
                &db,
                &roots,
                &st.thumbs,
                &pc_work::index::Options {
                    min_file_size: service::num(
                        &req.params,
                        "min_size",
                        settings["min_size"].as_i64().unwrap_or(102400),
                    ) as u64,
                    readers_per_disk: service::num(&req.params, "readers_per_disk", 2) as usize,
                    reindex: service::flag(&req.params, "reindex"),
                    workers: service::num(
                        &req.params,
                        "workers",
                        settings["workers"].as_i64().unwrap_or(0),
                    )
                    .max(0) as usize,
                },
                control,
            )?;
            Ok(())
        };
        let families = || -> Result<()> {
            pc_family::build_controlled(
                &db,
                &st.thumbs,
                &pc_family::Params {
                    phash_max: service::num(
                        &req.params,
                        "phash_max",
                        settings["phash_max"].as_i64().unwrap_or(10),
                    ) as u32,
                    ssim_min: req.params["ssim_min"]
                        .as_f64()
                        .unwrap_or(settings["ssim_min"].as_f64().unwrap_or(0.9)),
                    ..Default::default()
                },
                control,
            )?;
            service::restore_curation(&db)
        };
        let series = || -> Result<()> {
            pc_family::series::build_controlled(
                &db,
                service::num(
                    &req.params,
                    "gap_secs",
                    settings["series_gap_secs"].as_i64().unwrap_or(3),
                ),
                control,
            )?;
            service::restore_curation(&db)
        };
        let categories = || -> Result<()> {
            pc_family::categories::build_controlled(&db, control)?;
            Ok(())
        };

        match req.kind.as_str() {
            "scan" => scan()?,
            "index" => index()?,
            "families" => families()?,
            "series" => series()?,
            "categories" => categories()?,
            "build-all" => {
                let steps: [&dyn Fn() -> Result<()>; 3] = [&families, &series, &categories];
                for (n, step) in steps.iter().enumerate() {
                    control.stage(n as u32 + 1, steps.len() as u32);
                    step()?;
                }
            }
            // The whole path in one job: nothing to watch for, nothing to
            // start by hand between stages.
            "all" => {
                let steps: [&dyn Fn() -> Result<()>; 5] =
                    [&scan, &index, &families, &series, &categories];
                for (n, step) in steps.iter().enumerate() {
                    control.stage(n as u32 + 1, steps.len() as u32);
                    step()?;
                }
            }
            _ => bail!("Неизвестный вид задачи: {}", req.kind),
        }
    }
    if run_id.is_none() {
        db.conn.execute(
            "UPDATE jobs SET run_id=?1 WHERE id=?2",
            rusqlite::params![db.latest_run()?, id],
        )?;
    }
    Ok(())
}
