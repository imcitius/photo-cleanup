//! Web-only orchestration and reviewed disk plans. Core safety gates stay in pc-apply.
use crate::{
    jobs::{self, Request},
    AppState,
};
use anyhow::{bail, Context, Result};
use axum::{
    extract::{Path, Query, State},
    response::{IntoResponse, Response},
    Json,
};
use pc_db::Db;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Path as FsPath, PathBuf},
    sync::Arc,
};

pub fn error(code: u16, message: &str) -> Response {
    (
        axum::http::StatusCode::from_u16(code).unwrap(),
        Json(json!({"error":message})),
    )
        .into_response()
}
pub fn respond(value: Result<Value>) -> Response {
    match value {
        Ok(v) => Json(v).into_response(),
        Err(e) => error(400, &format!("{e:#}")),
    }
}
pub fn num(p: &Value, k: &str, default: i64) -> i64 {
    p[k].as_i64().unwrap_or(default)
}
pub fn flag(p: &Value, k: &str) -> bool {
    p[k].as_bool().unwrap_or(false)
}
fn strings(p: &Value, k: &str) -> Vec<String> {
    p[k].as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}
pub fn validate(r: &Request) -> Result<()> {
    if !matches!(
        r.kind.as_str(),
        "scan"
            | "index"
            | "families"
            | "series"
            | "categories"
            | "thumbs"
            | "build-all"
            | "all"
            | "plan-apply"
            | "derived-clean"
            | "derived-purge"
            | "organize-apply"
            | "organize-undo"
            | "journal-undo"
            | "quarantine-adopt"
            | "quarantine-purge"
    ) {
        bail!(
            "{}",
            pc_core::tf!(
                "Неизвестная операция: {0}",
                "Unknown operation: {0}",
                r.kind
            )
        );
    }
    if !r.params.is_object() {
        bail!(
            "{}",
            pc_core::tr!(
                "Параметры должны быть объектом",
                "Parameters have to be an object"
            )
        );
    }
    if matches!(r.kind.as_str(), "scan" | "index" | "all") {
        let roots = strings(&r.params, "roots");
        if roots.is_empty() {
            bail!(
                "{}",
                pc_core::tr!(
                    "Выберите хотя бы один корень архива",
                    "Choose at least one archive root"
                )
            );
        }
        let mut seen = Vec::<PathBuf>::new();
        for root in roots {
            let path = checked_dir(&root)?;
            if path.starts_with("/mnt/user") {
                bail!(
                    "{}",
                    pc_core::tf!(
                        "{0}: укажите /mnt/diskN, а не /mnt/user",
                        "{0}: use /mnt/diskN, not /mnt/user",
                        root
                    )
                );
            }
            if seen
                .iter()
                .any(|p| path.starts_with(p) || p.starts_with(&path))
            {
                bail!(
                    "{}",
                    pc_core::tf!("{0}: корни пересекаются", "{0}: the roots overlap", root)
                );
            }
            seen.push(path);
        }
    }
    for (key, min, max) in [
        ("min_size", 0, i64::MAX),
        ("readers_per_disk", 1, 8),
        ("phash_max", 0, 64),
        ("gap_secs", 1, 259200),
        ("resize_below", 0, 1_000_000_000),
        ("older_than_secs", 0, 315360000),
    ] {
        if let Some(v) = r.params.get(key) {
            if !v.as_i64().is_some_and(|n| n >= min && n <= max) {
                bail!(
                    "{}",
                    pc_core::tf!("Некорректное значение {0}", "Invalid value for {0}", key)
                );
            }
        }
    }
    if let Some(v) = r.params.get("ssim_min") {
        if !v.as_f64().is_some_and(|n| (0.0..=1.0).contains(&n)) {
            bail!(
                "{}",
                pc_core::tr!(
                    "SSIM должен быть от 0 до 1",
                    "SSIM has to be between 0 and 1"
                )
            );
        }
    }
    Ok(())
}
pub fn checked_dir(path: &str) -> Result<PathBuf> {
    // Read the text before handing it to `Path`. A Windows verbatim path —
    // `\\?\C:\…`, which is what `canonicalize` hands back there — is passed to
    // the OS untouched, and Rust stops treating `..` inside it as a parent
    // component. The walk below then let it through, the OS resolved it, and
    // the browse endpoint happily listed the directory above the archive.
    // Matching on the characters has no such corner.
    if pc_core::path_parts(path).contains(&"..") {
        bail!(
            "{}",
            pc_core::tf!(
                "{0}: переход через .. запрещён",
                "{0}: going up through .. is refused",
                path
            )
        );
    }
    let path = FsPath::new(path);
    if !path.is_absolute() {
        bail!(
            "{}",
            pc_core::tf!(
                "{0}: нужен абсолютный путь",
                "{0}: an absolute path is required",
                path.display()
            )
        );
    }
    let mut cur = PathBuf::new();
    for part in path.components() {
        if matches!(part, std::path::Component::ParentDir) {
            bail!(
                "{}",
                pc_core::tf!(
                    "{0}: переход через .. запрещён",
                    "{0}: going up through .. is refused",
                    path.display()
                )
            );
        }
        cur.push(part);
        // A prefix or a bare root is not a directory entry and cannot be a
        // symlink: `C:` on its own has no metadata to read, and asking for it
        // fails outright. Only the named components are worth checking.
        if matches!(
            part,
            std::path::Component::Prefix(_) | std::path::Component::RootDir
        ) {
            continue;
        }
        let md = std::fs::symlink_metadata(&cur)
            .with_context(|| pc_core::tf!("не прочитать {0}", "cannot read {0}", cur.display()))?;
        if md.file_type().is_symlink() {
            bail!(
                "{}: переход по символической ссылке запрещён",
                cur.display()
            );
        }
    }
    if !path.is_dir() {
        bail!(
            "{}",
            pc_core::tf!(
                "{0}: это не каталог",
                "{0}: not a directory",
                path.display()
            )
        );
    }
    Ok(path.canonicalize()?)
}
pub async fn fs(
    State(_st): State<Arc<AppState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    respond((|| {
        let path = checked_dir(q.get("path").map(String::as_str).unwrap_or("/"))?;
        let mount = pc_core::disk::mount_root(&path)?;
        let mut dirs = Vec::new();
        for entry in std::fs::read_dir(&path)
            .with_context(|| pc_core::tf!("не открыть {0}", "cannot open {0}", path.display()))?
        {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                dirs.push(json!({"name":entry.file_name().to_string_lossy(),"path":entry.path().to_string_lossy()}));
            }
        }
        dirs.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        Ok(
            json!({"path":path,"mount":mount,"parent":path.parent().filter(|p|p.starts_with(&mount)),"directories":dirs}),
        )
    })())
}

/// Put the process into the language the settings ask for.
///
/// The server renders stage names, refusal reasons and the like, and they
/// have to match the interface around them. One process serves one archive
/// and one person, so a process-wide setting is the honest model.
pub fn apply_language(settings: &Value) {
    pc_core::lang::set(pc_core::lang::Lang::parse(
        settings["language"].as_str().unwrap_or("en"),
    ));
}

/// Folders that look like they hold an archive, offered when none are chosen.
///
/// A container makes the question "which path?" genuinely confusing: the
/// operator picked host paths in the Docker form, and the tool only ever sees
/// the container ones. Rather than make them guess, the server says what it
/// can actually see.
fn suggested_roots() -> Vec<String> {
    // The union view is deliberately left out: a move through it would stop
    // being a rename, and the tool refuses it anyway.
    const HIDDEN: [&str; 6] = ["user", "user0", "disks", "remotes", "addons", "rootshare"];
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/mnt") {
        let mut found: Vec<String> = entries
            .flatten()
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| !HIDDEN.contains(&name.as_str()) && !name.starts_with('.'))
            .collect();
        found.sort();
        out.extend(found.into_iter().map(|name| format!("/mnt/{name}")));
    }
    if out.is_empty() {
        // Windows names it differently, and there is no /mnt to look in.
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
        if let Some(home) = home {
            for name in ["Pictures", "Photos", "Изображения"] {
                let p = FsPath::new(&home).join(name);
                if p.is_dir() {
                    out.push(p.display().to_string());
                }
            }
        }
    }
    out.truncate(8);
    out
}

pub fn settings_value(st: &AppState, db: &Db) -> Result<Value> {
    let mut v = json!({"db_path":st.db_path,"thumbs_path":st.thumbs.root(),"quarantine":st.quarantine,"phash_max":10,"ssim_min":0.9,"min_size":102400,"series_gap_secs":3,"event_gap_secs":21600,"theme":"system","density":"comfortable","language":"en","network":st.network,"roots":[],"suggested_roots":suggested_roots(),"workers":0,"cores":std::thread::available_parallelism().map(|n|n.get()).unwrap_or(1)});
    for row in jobs::rows(db, "SELECT key,value FROM settings", &[])? {
        if let Some(k) = row["key"].as_str() {
            v[k] = serde_json::from_str(row["value"].as_str().unwrap_or("null"))?;
        }
    }
    Ok(v)
}
pub async fn settings(State(st): State<Arc<AppState>>) -> Response {
    let db = st.db.lock().unwrap();
    respond(settings_value(&st, &db))
}
pub async fn save_settings(State(st): State<Arc<AppState>>, Json(v): Json<Value>) -> Response {
    let _gate = st.mutation.lock().unwrap();
    if let Err(e) = jobs::idle(&st) {
        return error(409, &e.to_string());
    }
    respond((|| {
        validate(&Request {
            kind: "families".into(),
            params: v.clone(),
            plan_token: None,
            confirmation: None,
        })?;
        if let Some(s) = v.get("theme") {
            if !matches!(s.as_str(), Some("system" | "light" | "dark")) {
                bail!("{}", pc_core::tr!("Неизвестная тема", "Unknown theme"));
            }
        }
        if let Some(s) = v.get("density") {
            if !matches!(s.as_str(), Some("comfortable" | "compact")) {
                bail!(
                    "{}",
                    pc_core::tr!("Неизвестная плотность", "Unknown density")
                );
            }
        }
        if let Some(s) = v.get("language") {
            if !matches!(s.as_str(), Some("ru" | "en")) {
                bail!("{}", pc_core::tr!("Неизвестный язык", "Unknown language"));
            }
        }
        // Zero means "decide for me"; anything above the core count would
        // only make the threads fight each other.
        if let Some(n) = v.get("workers") {
            let cores = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1) as i64;
            if !n.as_i64().is_some_and(|n| (0..=cores).contains(&n)) {
                bail!(
                    "{}",
                    pc_core::tf!(
                        "Потоков декодирования: от 0 до {0}",
                        "Decoding threads: between 0 and {0}",
                        cores
                    )
                );
            }
        }
        for key in ["series_gap_secs", "event_gap_secs"] {
            if let Some(n) = v.get(key) {
                if !n.as_i64().is_some_and(|n| (1..=259200).contains(&n)) {
                    bail!(
                        "{}",
                        pc_core::tf!("Некорректный разрыв: {0}", "Invalid gap: {0}", key)
                    );
                }
            }
        }
        if let Some(path) = v["quarantine"].as_str() {
            checked_dir(path)?;
        }
        let db = st.db.lock().unwrap();
        let tx = db.conn.unchecked_transaction()?;
        for key in [
            "roots",
            "phash_max",
            "ssim_min",
            "min_size",
            "series_gap_secs",
            "event_gap_secs",
            "workers",
            "language",
            "theme",
            "density",
            "quarantine",
        ] {
            if let Some(value) = v.get(key) {
                tx.execute("INSERT INTO settings VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[key,&value.to_string()])?;
            }
        }
        tx.commit()?;
        let updated = settings_value(&st, &db)?;
        apply_language(&updated);
        Ok(updated)
    })())
}
/// Everything the index knows about one file.
///
/// The score breakdown on a family card says *what* tipped the decision; this
/// is the evidence behind it — what the camera wrote, where the date came
/// from, what the frame measured. Without it the numbers are an assertion the
/// user has no way to check.
pub async fn file_details(State(st): State<Arc<AppState>>, Path(id): Path<i64>) -> Response {
    respond((|| {
        let db = st.db.lock().unwrap();
        let mut file = jobs::rows(
            &db,
            "SELECT f.id, f.path, f.name, f.disk, f.size, f.mtime, f.inode, f.nlink,
                    f.container, f.extension_lied, f.width, f.height, f.orientation,
                    f.pixel_source, f.thumb_key, f.state, f.skipped_reason,
                    f.sharpness, f.clip_low, f.clip_high, f.entropy, f.contrast,
                    f.saturation, f.white_fraction, f.bimodality, f.text_rows, f.text_banding,
                    f.chroma, f.tonal_range
               FROM files f WHERE f.id=?1",
            &[&id],
        )?
        .pop()
        .context(pc_core::tr!("Файл не найден", "File not found"))?;
        let meta = jobs::rows(&db, "SELECT * FROM meta WHERE file_id=?1", &[&id])?.pop();
        let categories = jobs::rows(
            &db,
            "SELECT category, confidence, evidence, manual FROM file_categories WHERE file_id=?1",
            &[&id],
        )?;
        file["meta"] = meta.unwrap_or(Value::Null);
        file["categories"] = json!(categories);
        Ok(file)
    })())
}

/// The tail of the index, newest first.
///
/// Indexing commits every couple of seconds, so a page that asks for this
/// while a pass is running watches the archive arrive photograph by
/// photograph instead of staring at a bar. Skipped files are included on
/// purpose: seeing what was refused, as it is refused, is half the value.
pub async fn recent(
    State(st): State<Arc<AppState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let limit = q
        .get("limit")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(24)
        .clamp(1, 200);
    respond((|| {
        let db = st.db.lock().unwrap();
        Ok(json!(jobs::rows(
            &db,
            "SELECT id, path, name, size, width, height, thumb_key, skipped_reason
               FROM files
              WHERE state='present'
              ORDER BY id DESC
              LIMIT ?1",
            &[&limit],
        )?))
    })())
}

/// The word that has to be typed to wipe the index.
///
/// Spelled out rather than a checkbox for the same reason the purge screen
/// asks for one: a reset throws away an hour of reading, and a mis-click
/// should not be able to do that.
pub fn reset_word() -> &'static str {
    pc_core::tr!("СБРОСИТЬ", "RESET")
}

/// Throw away the index and the thumbnail cache, and start from nothing.
///
/// Nothing in the archive itself is touched — this only forgets what was
/// *learned* about it. The journal stays, so anything already in quarantine
/// can still be put back; the chosen folders and thresholds stay too, since
/// "rescan from scratch" does not mean "ask me everything again".
pub async fn reset(State(st): State<Arc<AppState>>, Json(v): Json<Value>) -> Response {
    let _gate = st.mutation.lock().unwrap();
    if let Err(e) = jobs::idle(&st) {
        return error(409, &e.to_string());
    }
    if v["confirmation"].as_str() != Some(reset_word()) {
        return error(
            400,
            &pc_core::tf!(
                "Для сброса индекса введите {0}",
                "Type {0} to reset the index",
                reset_word()
            ),
        );
    }
    respond((|| {
        let db = st.db.lock().unwrap();
        db.reset_index()?;
        let thumbs = st.thumbs.clear()?;
        Ok(json!({"ok": true, "thumbs_removed": thumbs}))
    })())
}

fn quarantine_root(st: &AppState, db: &Db) -> Result<Option<PathBuf>> {
    Ok(settings_value(st, db)?["quarantine"]
        .as_str()
        .map(Into::into))
}
pub async fn catalogs(State(st): State<Arc<AppState>>) -> Response {
    respond((|| {
        let db = st.db.lock().unwrap();
        let mut rows = jobs::rows(
            &db,
            "SELECT * FROM lr_catalogs ORDER BY is_locked DESC,name",
            &[],
        )?;
        for c in &mut rows {
            c["is_locked"] =
                json!(FsPath::new(&format!("{}.lock", c["path"].as_str().unwrap_or(""))).exists());
        }
        Ok(json!(rows))
    })())
}
pub async fn journal(
    State(st): State<Arc<AppState>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    respond((|| {
        let db = st.db.lock().unwrap();
        let run = q.get("run").cloned().unwrap_or_default();
        let op = q.get("op").cloned().unwrap_or_default();
        let status = q.get("status").cloned().unwrap_or_default();
        Ok(json!(jobs::rows(&db,"SELECT * FROM journal WHERE (?1='' OR run_id=CAST(?1 AS INTEGER)) AND (?2='' OR op=?2) AND (?3='' OR status=?3) ORDER BY (status='pending') DESC,id DESC", &[&run,&op,&status])?))
    })())
}
pub async fn runs(State(st): State<Arc<AppState>>) -> Response {
    respond((|| {
        let db = st.db.lock().unwrap();
        Ok(json!(jobs::rows(&db,"SELECT r.*, COUNT(j.id) AS operations, COALESCE(SUM(j.size),0) AS bytes, SUM(j.op='organize' AND j.status='done') AS undoable FROM runs r LEFT JOIN journal j ON j.run_id=r.id GROUP BY r.id ORDER BY r.id DESC",&[])?))
    })())
}

pub enum Action {
    Copy(pc_family::plan::Candidate),
    Bundle(pc_db::Bundle),
    Move(pc_organize::Move),
    Undo(pc_db::JournalEntry),
    Purge(pc_db::JournalEntry),
    /// A file in a quarantine folder that this database never put there,
    /// carried back to where it came from, or deleted for good.
    Adopt(pc_db::QuarantineFound),
    Abandon(pc_db::QuarantineFound),
}
impl Action {
    pub fn path(&self) -> &str {
        match self {
            Self::Copy(x) => &x.path,
            Self::Bundle(x) => &x.path,
            Self::Adopt(x) | Self::Abandon(x) => &x.path,
            Self::Move(x) => &x.src,
            Self::Undo(x) | Self::Purge(x) => &x.src,
        }
    }
    pub fn source_path(&self) -> &str {
        match self {
            Self::Undo(e) | Self::Purge(e) => e.dst.as_deref().unwrap_or(&e.src),
            _ => self.path(),
        }
    }
    pub fn size(&self) -> u64 {
        match self {
            Self::Copy(x) => x.size,
            Self::Bundle(x) => x.size,
            Self::Move(x) => x.size,
            Self::Adopt(x) | Self::Abandon(x) => x.size,
            Self::Undo(x) | Self::Purge(x) => x.size,
        }
        .max(0) as u64
    }
}
pub fn make_preview(st: &AppState, db: &Db, r: &Request) -> Result<(Value, Vec<Action>)> {
    validate(r)?;
    let root = quarantine_root(st, db)?;
    let mut actions = Vec::new();
    let mut items = Vec::new();
    let mut refusals = Vec::new();
    let mut add_refusal = |path: String, why: String| refusals.push(json!({"path":path,"why":why}));
    match r.kind.as_str() {
        "plan-apply" => {
            let mut policy = pc_family::Policy {
                respect_lightroom: !flag(&r.params, "allow_lightroom"),
                resize_below_pixels: num(&r.params, "resize_below", 2_000_000),
                ..Default::default()
            };
            if r.params.get("roles").is_some() {
                policy.remove_roles.clear();
                for name in strings(&r.params, "roles") {
                    let role = pc_family::Role::parse(&name)
                        .context(pc_core::tr!("Неизвестная роль", "Unknown role"))?;
                    if role == pc_family::Role::Original {
                        bail!(
                            "{}",
                            pc_core::tr!("ORIGINAL удалять нельзя", "ORIGINAL is never removed")
                        );
                    }
                    policy.remove_roles.insert(role);
                }
            }
            // Narrowed where the rows are read, not after: a press on one
            // group of ten thousand should not walk the whole archive twice.
            let scope = pc_family::plan::Scope {
                family: r.params.get("family_id").and_then(Value::as_i64),
                folder: r
                    .params
                    .get("folder")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                keeper_folder: r
                    .params
                    .get("keeper_folder")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            };
            let mut plan = pc_family::plan::compute_scoped(db, &policy, &scope)?;
            // One group at a time. Ten thousand groups is not a decision
            // anybody makes in one press, so the interface offers each group
            // its own, and the plan behind it is the same plan — narrowed,
            // not a second code path with its own rules.
            if let Some(family) = r.params.get("family_id").and_then(Value::as_i64) {
                // A hand-made decision belongs to the file, not to a group,
                // so it arrives with no family on it — and the rows were
                // already narrowed to this group when they were read.
                plan.candidates
                    .retain(|c| c.family_id == family || c.manual);
                plan.refusals.clear();
            }
            // ...or to one folder. A folder that copies another is cleared in
            // one go rather than in a thousand presses; what leaves is only
            // what the plan already called a copy, and only from there.
            if let Some(dir) = r.params.get("folder").and_then(Value::as_str) {
                plan.candidates
                    .retain(|c| pc_core::dir_name(&c.path) == dir);
                plan.refusals.clear();
            }
            // The groups this folder keeps: their copies go, wherever they
            // are. This is what follows from calling a folder the main one.
            if let Some(dir) = r.params.get("keeper_folder").and_then(Value::as_str) {
                // By the file the group keeps, not by the one that proves the
                // copy: a decision made by hand has no such proof, and
                // filtering on it dropped exactly the versions the user had
                // just set aside for this folder.
                plan.candidates
                    .retain(|c| pc_core::dir_name(&c.group_keeper) == dir);
                plan.refusals.clear();
            }
            for refusal in plan.refusals {
                add_refusal(refusal.path, refusal.why);
            }
            for c in plan.candidates {
                match pc_apply::quarantine_dest_for(&c.path, c.file_id, db, root.as_deref()) {
                    Ok(dst) => {
                        if dst.exists() {
                            add_refusal(
                                c.path.clone(),
                                pc_core::tf!(
                                    "Цель занята: {0}",
                                    "Destination taken: {0}",
                                    dst.display()
                                ),
                            );
                            continue;
                        }
                        let thumb = |id| db.file(id).ok().flatten().and_then(|f| f.thumb_key);
                        items.push(json!({"file_id":c.file_id,"path":c.path,"dst":dst,"size":c.size,"file_count":1,"role":c.role.as_str(),"manual":c.manual,"reason":c.reason,"keeper_id":c.keeper_id,"keeper_path":c.keeper_path,"thumb":thumb(c.file_id),"keeper_thumb":thumb(c.keeper_id)}));
                        actions.push(Action::Copy(c));
                    }
                    Err(e) => add_refusal(c.path, format!("{e:#}")),
                }
            }
        }
        "derived-clean" => {
            let kinds = strings(&r.params, "kinds");
            for b in db.list_bundles(&pc_db::model::BundleFilter {
                state: Some(pc_db::BundleState::Present),
                ..Default::default()
            })? {
                if !kinds.contains(&b.kind.as_str().to_string())
                    || b.size < num(&r.params, "min_size", 0)
                {
                    continue;
                }
                if !b.removable() {
                    add_refusal(
                        b.path.clone(),
                        b.blocked_detail.clone().unwrap_or(
                            pc_core::tr!(
                                "Удаление запрещено видом данных",
                                "This kind of data is never removed"
                            )
                            .into(),
                        ),
                    );
                    continue;
                }
                if let Err(e) = pc_apply::lightroom_gate(&b) {
                    add_refusal(b.path.clone(), format!("{e:#}"));
                    continue;
                }
                match pc_apply::quarantine_dest(&b, root.as_deref()) {
                    Ok(dst) => {
                        if dst.exists() {
                            add_refusal(
                                b.path.clone(),
                                pc_core::tf!(
                                    "Цель занята: {0}",
                                    "Destination taken: {0}",
                                    dst.display()
                                ),
                            );
                            continue;
                        }
                        items.push(json!({"path":b.path,"dst":dst,"size":b.size,"file_count":b.file_count,"kind":b.kind.as_str()}));
                        actions.push(Action::Bundle(b));
                    }
                    Err(e) => add_refusal(b.path.clone(), format!("{e:#}")),
                }
            }
        }
        "organize-apply" => {
            if !flag(&r.params, "allow_duplicates")
                && !pc_family::plan::compute(db, &pc_family::Policy::default())?
                    .candidates
                    .is_empty()
            {
                bail!(
                    "{}",
                    pc_core::tr!(
                        "Сначала разберите точные копии на экране «План и перенос». Раскладка иначе перенесёт и дубликаты.",
                        "Resolve the exact copies on the “Plan and move” screen first, or the sorting carries the duplicates along."
                    )
                );
            }
            let root = r.params["root"].as_str().context(pc_core::tr!(
                "Не выбран корень нового дерева",
                "No root chosen for the new tree"
            ))?;
            checked_dir(root)?;
            let plan = pc_organize::compute(
                db,
                &pc_organize::Options {
                    root: root.into(),
                    gap_secs: num(&r.params, "gap_secs", 21600),
                    respect_lightroom: !flag(&r.params, "allow_lightroom"),
                    skip_uncertain: flag(&r.params, "skip_uncertain"),
                },
            )?;
            for refusal in plan.refusals {
                add_refusal(refusal.path, refusal.why);
            }
            for m in plan.moves {
                let thumb = db.file(m.file_id)?.and_then(|f| f.thumb_key);
                items.push(json!({"file_id":m.file_id,"path":m.src,"dst":m.dst,"size":m.size,"file_count":1,"source":m.date.source.label(),"uncertain":m.date.uncertain(),"taken_at":m.date.ts,"event":m.event,"year":pc_core::time::civil_from_unix(m.date.ts).0,"renamed_from":m.renamed_from,"thumb":thumb}));
                actions.push(Action::Move(m));
            }
        }
        "derived-purge" => {
            let cutoff = pc_core::time::now_unix() - num(&r.params, "older_than_secs", 604800);
            for e in db.journal_quarantined(None)? {
                if e.applied_at >= cutoff {
                    add_refusal(
                        e.src.clone(),
                        pc_core::tr!(
                            "Срок удержания ещё не прошёл",
                            "The holding period has not passed yet"
                        )
                        .into(),
                    );
                    continue;
                }
                let mut item = json!({"journal_id":e.id,"path":e.dst,"dst":"Окончательное удаление","original":e.src,"size":e.size,"file_count":e.file_count});
                // The entry already counts what travelled with the frame, so
                // the companions are read off it rather than looked for on
                // disk — and counted once.
                if let Some(rest) = manifest_companions(&e, |_| {
                    json!(pc_core::tr!("Окончательное удаление", "Deleted for good"))
                }) {
                    item["companions"] = rest;
                }
                items.push(item);
                actions.push(Action::Purge(e));
            }
        }
        "quarantine-adopt" | "quarantine-purge" => {
            // Files the journal cannot account for: left by a database that
            // is no longer here. They are decided on as a set, like every
            // other disk operation — reviewed, counted, and refused one by
            // one with a reason, instead of a button that acts on whatever
            // the last walk happened to see.
            let purge = r.kind == "quarantine-purge";
            for f in db.quarantine_found()?.into_iter().filter(|f| !f.known) {
                if !FsPath::new(&f.path).exists() {
                    add_refusal(
                        f.path.clone(),
                        pc_core::tr!("Файла уже нет", "The file is already gone").into(),
                    );
                    continue;
                }
                if purge {
                    items.push(
                        json!({"path":f.path,"dst":pc_core::tr!("Окончательное удаление","Deleted for good"),"size":f.size,"file_count":1}),
                    );
                    actions.push(Action::Abandon(f));
                    continue;
                }
                let Some(dst) = pc_core::quarantine_origin(&f.path) else {
                    add_refusal(
                        f.path.clone(),
                        pc_core::tr!(
                            "Непонятно, откуда этот файл: он не лежит в папке карантина",
                            "There is no telling where this came from: it is not inside a quarantine folder"
                        )
                        .into(),
                    );
                    continue;
                };
                if FsPath::new(&dst).exists() {
                    add_refusal(
                        f.path.clone(),
                        pc_core::tf!(
                            "На месте уже лежит файл: {0}",
                            "A file is already back in place: {0}",
                            dst
                        ),
                    );
                    continue;
                }
                items.push(json!({"path":f.path,"dst":dst,"size":f.size,"file_count":1}));
                actions.push(Action::Adopt(f));
            }
        }
        "journal-undo" | "organize-undo" => {
            let entries = if r.kind == "journal-undo" {
                vec![db
                    .journal_entry(num(&r.params, "journal_id", 0))?
                    .context(pc_core::tr!("Нет записи журнала", "No such journal entry"))?]
            } else {
                db.journal_by_run_op(num(&r.params, "run_id", 0), "organize")?
            };
            for e in entries {
                if e.status != pc_db::JournalStatus::Done {
                    add_refusal(
                        e.src.clone(),
                        pc_core::tr!(
                            "Запись не завершена или уже отменена",
                            "The entry is unfinished or already undone"
                        )
                        .into(),
                    );
                    continue;
                }
                if FsPath::new(&e.src).exists() {
                    add_refusal(
                        e.src.clone(),
                        pc_core::tr!(
                            "Исходный путь занят, перезапись запрещена",
                            "The original path is taken; overwriting is refused"
                        )
                        .into(),
                    );
                    continue;
                }
                let mut item = json!({"journal_id":e.id,"path":e.dst,"dst":e.src,"size":e.size,"file_count":e.file_count});
                if let Some(rest) = manifest_companions(&e, |m| json!(m.src)) {
                    item["companions"] = rest;
                }
                items.push(item);
                actions.push(Action::Undo(e));
            }
        }
        _ => bail!(
            "{}",
            pc_core::tr!(
                "У этой задачи нет дискового плана",
                "This job has no disk plan"
            )
        ),
    }
    if r.kind == "organize-apply" {
        let best: std::collections::HashSet<i64> = db
            .conn
            .prepare("SELECT best_file FROM series WHERE best_file IS NOT NULL")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut covers = std::collections::BTreeMap::<String, (bool, Value)>::new();
        for item in &items {
            let key = format!("{}/{}", item["year"], item["event"]);
            let is_best = best.contains(&item["file_id"].as_i64().unwrap_or(0));
            if !item["thumb"].is_null() && covers.get(&key).is_none_or(|(b, _)| !*b && is_best) {
                covers.insert(key, (is_best, item["thumb"].clone()));
            }
        }
        for item in &mut items {
            let key = format!("{}/{}", item["year"], item["event"]);
            item["event_cover"] = covers
                .get(&key)
                .map(|(_, thumb)| thumb.clone())
                .unwrap_or(Value::Null);
        }
    }
    // Companions move with a photograph, so they belong in the reviewed
    // count, byte total and destinations too. Refuse occupied sidecar paths
    // before moving the photograph, not after a partially successful rename.
    let mut blocked = std::collections::HashSet::new();
    for item in &mut items {
        // An item that brought its own list has it from the journal, which
        // knows what actually moved; the disk beside the file does not.
        if item.get("companions").is_some() {
            continue;
        }
        let src = item["path"].as_str().unwrap_or("");
        let dst = item["dst"].as_str().unwrap_or("");
        if !FsPath::new(src).is_file() {
            continue;
        }
        let companions = pc_apply::companions(FsPath::new(src));
        let mut listed = Vec::new();
        let mut extra = 0;
        for side in companions {
            let target = companion_destination(FsPath::new(src), FsPath::new(dst), &side);
            if r.kind != "derived-purge" && target.exists() {
                blocked.insert(src.to_string());
                add_refusal(
                    src.into(),
                    pc_core::tf!(
                        "Путь спутника занят: {0}",
                        "The companion path is taken: {0}",
                        target.display()
                    ),
                );
            }
            let size = std::fs::metadata(&side)
                .with_context(|| {
                    pc_core::tf!("не прочитать {0}", "cannot read {0}", side.display())
                })?
                .len();
            extra += size;
            listed.push(json!({"path":side,"dst":if r.kind=="derived-purge" {json!("Окончательное удаление")} else {json!(target)},"size":size}));
        }
        item["file_count"] = json!(item["file_count"].as_u64().unwrap_or(1) + listed.len() as u64);
        item["size"] = json!(item["size"].as_u64().unwrap_or(0) + extra);
        item["companions"] = json!(listed);
    }
    if !blocked.is_empty() {
        items.retain(|i| !blocked.contains(i["path"].as_str().unwrap_or("")));
        actions.retain(|a| {
            let src = match a {
                Action::Undo(e) => e.dst.as_deref().unwrap_or(&e.src),
                _ => a.path(),
            };
            !blocked.contains(src)
        });
    }
    // Stable order is essential: the family planner uses a HashMap.
    items.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    refusals.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    let count: i64 = items
        .iter()
        .map(|x| x["file_count"].as_i64().unwrap_or(1))
        .sum();
    let bytes: i64 = items.iter().map(|x| x["size"].as_i64().unwrap_or(0)).sum();
    let mut out = json!({"kind":r.kind,"params":r.params,"items":items,"refusals":refusals,"total_files":count,"total_bytes":bytes});
    out["token"] = json!(blake3::hash(out.to_string().as_bytes())
        .to_hex()
        .to_string());
    Ok((out, actions))
}
pub fn apply_action(
    st: &AppState,
    db: &Db,
    run: i64,
    a: &Action,
    r: &Request,
    control: &pc_core::work::Control,
) -> Result<()> {
    let root = quarantine_root(st, db)?;
    match a {
        Action::Copy(c) => {
            let (out, why) = pc_apply::files::quarantine_file(db, run, c, root.as_deref())?;
            if out == pc_apply::FileOutcome::Refused {
                bail!("{why}");
            }
        }
        Action::Bundle(b) => {
            if matches!(
                pc_apply::quarantine(db, run, b, root.as_deref())?,
                pc_apply::Outcome::Skipped
            ) {
                bail!(
                    "{}",
                    pc_core::tf!(
                        "Изменился с момента описи: {0}",
                        "Changed since the inventory: {0}",
                        b.path
                    )
                );
            }
        }
        Action::Move(m) => {
            if !flag(&r.params, "allow_lightroom") && db.lightroom_protected()?.contains_key(&m.src)
            {
                bail!(
                    "{}",
                    pc_core::tf!(
                        "Файл защищён каталогом Lightroom: {0}",
                        "Protected by a Lightroom catalogue: {0}",
                        m.src
                    )
                );
            }
            let report = pc_apply::organize(db, run, std::slice::from_ref(m))?;
            if let Some((path, why)) = report.refused.first() {
                bail!("{path}: {why}");
            }
        }
        Action::Undo(e) => pc_apply::undo(db, e.id)?,
        Action::Purge(e) => pc_apply::purge_entry_controlled(db, e.id, control)?,
        Action::Adopt(f) => {
            let dst = pc_core::quarantine_origin(&f.path).context(pc_core::tr!(
                "Непонятно, откуда этот файл",
                "There is no telling where this came from"
            ))?;
            pc_apply::adopt_orphan(db, run, &f.path, &dst)?;
            db.forget_quarantine_found(&f.path)?;
        }
        Action::Abandon(f) => {
            pc_apply::abandon_orphan(db, run, &f.path, control)?;
            db.forget_quarantine_found(&f.path)?;
        }
    }
    Ok(())
}
pub async fn preview(State(st): State<Arc<AppState>>, Json(r): Json<Request>) -> Response {
    respond((|| {
        let db = st.db.lock().unwrap();
        Ok(make_preview(&st, &db, &r)?.0)
    })())
}

fn split(db: &Db, family: i64, file: i64) -> Result<()> {
    let member = db
        .family(family)?
        .context(pc_core::tr!("Семейство не найдено", "Group not found"))?
        .members
        .into_iter()
        .find(|m| m.file_id == file)
        .context(pc_core::tr!(
            "Файл не входит в семейство",
            "The file is not in that group"
        ))?;
    let new = db.insert_family(
        "manual",
        None,
        None,
        Some(file),
        db.latest_run()?.unwrap_or(0),
    )?;
    db.conn.execute(
        "DELETE FROM family_members WHERE family_id=?1 AND file_id=?2",
        [family, file],
    )?;
    db.insert_family_member(
        new,
        file,
        "unknown",
        None,
        member.quality,
        &member.breakdown,
    )?;
    db.conn.execute("UPDATE families SET keeper_file=(SELECT file_id FROM family_members WHERE family_id=?1 ORDER BY quality DESC LIMIT 1) WHERE id=?1 AND keeper_file=?2",[family,file])?;
    db.conn.execute("DELETE FROM families WHERE id=?1 AND NOT EXISTS(SELECT 1 FROM family_members WHERE family_id=?1)",[family])?;
    Ok(())
}
pub fn restore_curation(db: &Db) -> Result<()> {
    for row in jobs::rows(db,"SELECT s.file_id,m.family_id FROM manual_splits s JOIN family_members m USING(file_id) WHERE (SELECT COUNT(*) FROM family_members x WHERE x.family_id=m.family_id)>1",&[])? {
        split(db,row["family_id"].as_i64().unwrap(),row["file_id"].as_i64().unwrap())?;
    }
    db.conn.execute_batch("UPDATE families SET keeper_file=(SELECT k.file_id FROM manual_keepers k JOIN family_members m ON m.file_id=k.file_id WHERE m.family_id=families.id ORDER BY k.marked_at DESC, k.file_id DESC LIMIT 1) WHERE EXISTS(SELECT 1 FROM manual_keepers k JOIN family_members m ON m.file_id=k.file_id WHERE m.family_id=families.id); UPDATE series SET best_file=(SELECT k.file_id FROM manual_best k JOIN series_members m ON m.file_id=k.file_id WHERE m.series_id=series.id LIMIT 1) WHERE EXISTS(SELECT 1 FROM manual_best k JOIN series_members m ON m.file_id=k.file_id WHERE m.series_id=series.id);")?;
    Ok(())
}
pub async fn split_family(
    State(st): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(v): Json<Value>,
) -> Response {
    mutate(&st, |db| {
        let file = v["file_id"]
            .as_i64()
            .context(pc_core::tr!("Не указан файл", "No file given"))?;
        let tx = db.conn.unchecked_transaction()?;
        split(db, id, file)?;
        db.conn
            .execute("INSERT OR IGNORE INTO manual_splits VALUES(?1)", [file])?;
        tx.commit()?;
        Ok(json!({"ok":true}))
    })
}
/// Treat one folder as where the archive's originals live.
///
/// Going through ten thousand groups one at a time is the work this is meant
/// to save: an archive usually has a folder the photographs were worked in
/// and other folders that are copies of it. Told which folder that is, every
/// group that has a file there keeps that file, and the rest become the
/// copies they are.
///
/// The choice is written down as a decision of the user's, not a guess, so
/// rebuilding the groups does not quietly undo it.
pub async fn prefer_folder(State(st): State<Arc<AppState>>, Json(v): Json<Value>) -> Response {
    mutate(&st, |db| {
        let dir = v["dir"]
            .as_str()
            .filter(|d| !d.is_empty())
            .context(pc_core::tr!("Не указана папка", "No folder given"))?
            .to_string();

        type Row = (i64, i64, String, f64, Option<Vec<u8>>, Option<Vec<u8>>);
        let rows: Vec<Row> = {
            let mut st = db.conn.prepare(
                "SELECT fm.family_id, fm.file_id, f.path, COALESCE(fm.quality, 0),
                        COALESCE(f.content_hash, f.pixel_hash),
                        (SELECT COALESCE(k.content_hash, k.pixel_hash) FROM files k
                          WHERE k.id = fa.keeper_file)
                   FROM family_members fm
                   JOIN files f     ON f.id = fm.file_id
                   JOIN families fa ON fa.id = fm.family_id
                  WHERE f.state = 'present'",
            )?;
            let rows = st
                .query_map([], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                })?
                .collect::<rusqlite::Result<_>>()?;
            rows
        };

        // The best file the folder holds, per group — but only among files
        // that hold the same pixels as the one being kept now.
        //
        // A group is one photograph, and that is not the same as one set of
        // pixels: a scan and the JPEG exported from it sit in the same group
        // and differ byte for byte. Moving the keeper onto the export would
        // leave every scan in the group a "copy" of something it does not
        // match — and the move refuses at the last moment, every time,
        // because it re-reads both files and compares. The group then sits in
        // the list for ever, refusing to be dealt with. So a folder can only
        // take over the groups whose picture it actually holds.
        let mut best: std::collections::HashMap<i64, (i64, f64)> = std::collections::HashMap::new();
        let mut untouched: std::collections::HashSet<i64> = std::collections::HashSet::new();
        for (family, file, path, quality, pixels, keeper_pixels) in rows {
            if pc_core::dir_name(&path) != dir {
                continue;
            }
            let same = match (&pixels, &keeper_pixels) {
                (Some(a), Some(b)) => a == b,
                _ => false,
            };
            if !same {
                untouched.insert(family);
                continue;
            }
            let e = best.entry(family).or_insert((file, quality));
            if quality > e.1 {
                *e = (file, quality);
            }
        }
        for family in best.keys() {
            untouched.remove(family);
        }

        let tx = db.conn.unchecked_transaction()?;
        let mut changed = 0u64;
        for (family, (file, _)) in &best {
            if db.set_manual_keeper(*family, *file)? {
                changed += 1;
            }
        }
        tx.commit()?;
        Ok(json!({"groups": changed, "untouched": untouched.len(), "dir": dir}))
    })
}

/// What sits in quarantine folders that no journal entry accounts for.
///
/// These are files a previous database put there. Nothing in the index knows
/// them, the quarantine screen cannot show them, and the disk still carries
/// them. Counting them is the whole of the answer; what to do about them is
/// the user's call.
pub async fn quarantine_orphans(State(st): State<Arc<AppState>>) -> Response {
    respond((|| {
        let db = st.db.lock().unwrap();
        let found = db.quarantine_found()?;
        let orphans: Vec<_> = found.iter().filter(|f| !f.known).collect();
        Ok(json!({
            "files": orphans.len(),
            "bytes": orphans.iter().map(|f| f.size).sum::<i64>(),
            "known_files": found.len() - orphans.len(),
            "items": orphans.iter().take(200).map(|f| json!({
                "path": f.path,
                "name": pc_core::base_name(&f.path),
                "restore_to": pc_core::quarantine_origin(&f.path),
                "size": f.size,
                "mtime": f.mtime,
            })).collect::<Vec<_>>(),
        }))
    })())
}

pub async fn keep_only(
    State(st): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(v): Json<Value>,
) -> Response {
    mutate(&st, |db| {
        let keep = v["file_id"]
            .as_i64()
            .context(pc_core::tr!("Не указан файл", "No file given"))?;
        let members: Vec<i64> = {
            let mut st = db.conn.prepare(
                "SELECT fm.file_id FROM family_members fm JOIN files f ON f.id = fm.file_id
                  WHERE fm.family_id = ?1 AND f.state = 'present'",
            )?;
            let rows = st
                .query_map([id], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            rows
        };
        if !members.contains(&keep) {
            bail!(
                "{}",
                pc_core::tr!(
                    "Этот файл не из этой группы",
                    "That file is not in this group"
                )
            );
        }
        let tx = db.conn.unchecked_transaction()?;
        db.set_manual_keeper(id, keep)?;
        let mut marked = 0u64;
        for m in members.iter().filter(|m| **m != keep) {
            db.conn.execute(
                "INSERT OR IGNORE INTO manual_rejects(file_id, marked_at) VALUES(?1, ?2)",
                rusqlite::params![m, pc_core::time::now_unix()],
            )?;
            marked += 1;
        }
        tx.commit()?;
        Ok(json!({"kept": keep, "marked": marked}))
    })
}

/// The same decision, for every group this folder has a hand in.
///
/// One press settles one group; an archive holds ten thousand. When the
/// answer is always the same — these scans are the ones to keep, the exports
/// beside them are not — it should be said once. For every group holding a
/// file in this folder, that file becomes the kept one and every other
/// version is set aside.
pub async fn keep_folder_only(State(st): State<Arc<AppState>>, Json(v): Json<Value>) -> Response {
    mutate(&st, |db| {
        let dir = v["dir"]
            .as_str()
            .filter(|d| !d.is_empty())
            .context(pc_core::tr!("Не указана папка", "No folder given"))?
            .to_string();

        let rows: Vec<(i64, i64, String, f64)> = {
            let mut st = db.conn.prepare(
                "SELECT fm.family_id, fm.file_id, f.path, COALESCE(fm.quality, 0)
                   FROM family_members fm JOIN files f ON f.id = fm.file_id
                  WHERE f.state = 'present'",
            )?;
            let rows = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<rusqlite::Result<_>>()?;
            rows
        };

        // The best file the folder holds, per group. No pixel test here: the
        // whole point is the groups where the pixels differ and only a person
        // can choose.
        let mut keep: std::collections::HashMap<i64, (i64, f64)> = std::collections::HashMap::new();
        for (family, file, path, quality) in &rows {
            if pc_core::dir_name(path) != dir {
                continue;
            }
            let e = keep.entry(*family).or_insert((*file, *quality));
            if *quality > e.1 {
                *e = (*file, *quality);
            }
        }

        let tx = db.conn.unchecked_transaction()?;
        let (mut groups, mut marked) = (0u64, 0u64);
        for (family, (file, _)) in &keep {
            if !db.set_manual_keeper(*family, *file)? {
                continue;
            }
            groups += 1;
            for (fam, other, _, _) in &rows {
                if fam == family && other != file {
                    db.conn.execute(
                        "INSERT OR IGNORE INTO manual_rejects(file_id, marked_at) VALUES(?1, ?2)",
                        rusqlite::params![other, pc_core::time::now_unix()],
                    )?;
                    marked += 1;
                }
            }
        }
        tx.commit()?;
        Ok(json!({"groups": groups, "marked": marked, "dir": dir}))
    })
}

/// Take back that choice: the group goes back to being undecided.
pub async fn keep_all_versions(State(st): State<Arc<AppState>>, Path(id): Path<i64>) -> Response {
    mutate(&st, |db| {
        let n = db.conn.execute(
            "DELETE FROM manual_rejects WHERE file_id IN
               (SELECT file_id FROM family_members WHERE family_id = ?1)",
            [id],
        )?;
        Ok(json!({"cleared": n}))
    })
}

pub fn mutate(st: &AppState, f: impl FnOnce(&Db) -> Result<Value>) -> Response {
    let _gate = st.mutation.lock().unwrap();
    if let Err(e) = jobs::idle(st) {
        return error(409, &e.to_string());
    }
    let db = st.db.lock().unwrap();
    respond(f(&db))
}
pub async fn best(
    State(st): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(v): Json<Value>,
) -> Response {
    mutate(&st, |db| {
        let file = v["file_id"]
            .as_i64()
            .context(pc_core::tr!("Не указан файл", "No file given"))?;
        let tx = db.conn.unchecked_transaction()?;
        let n=db.conn.execute("UPDATE series SET best_file=?1 WHERE id=?2 AND EXISTS(SELECT 1 FROM series_members WHERE series_id=?2 AND file_id=?1)",[file,id])?;
        if n == 0 {
            bail!(
                "{}",
                pc_core::tr!("Файл не входит в серию", "The file is not in that burst")
            );
        }
        db.conn.execute("DELETE FROM manual_best WHERE file_id IN(SELECT file_id FROM series_members WHERE series_id=?1)",[id])?;
        db.conn
            .execute("INSERT OR IGNORE INTO manual_best VALUES(?1)", [file])?;
        tx.commit()?;
        Ok(json!({"ok":true}))
    })
}
/// Mark one frame as not worth keeping, or take the mark back.
///
/// Nothing moves here. The mark puts the file in the plan, which the user
/// still has to read and approve, and even then it goes to quarantine.
pub async fn reject(
    State(st): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(v): Json<Value>,
) -> Response {
    mutate(&st, |db| {
        let on = v["rejected"].as_bool().unwrap_or(true);
        let known: i64 =
            db.conn
                .query_row("SELECT count(*) FROM files WHERE id=?1", [id], |r| r.get(0))?;
        if known == 0 {
            bail!("{}", pc_core::tr!("Файл не найден", "File not found"));
        }
        if on {
            db.conn.execute(
                "INSERT OR IGNORE INTO manual_rejects VALUES(?1,?2)",
                rusqlite::params![id, pc_core::time::now_unix()],
            )?;
        } else {
            db.conn
                .execute("DELETE FROM manual_rejects WHERE file_id=?1", [id])?;
        }
        Ok(json!({"ok":true,"rejected":on}))
    })
}

/// Reject every frame of a series except the best one.
///
/// The point of ranking a burst is to keep one frame from it. Doing that
/// seventy times by hand is the sort of work people abandon halfway, so it is
/// one action — and it is as reversible as the individual marks it sets.
pub async fn reject_rest(State(st): State<Arc<AppState>>, Path(id): Path<i64>) -> Response {
    mutate(&st, |db| {
        let tx = db.conn.unchecked_transaction()?;
        let n = db.conn.execute(
            "INSERT OR IGNORE INTO manual_rejects
             SELECT m.file_id, ?2 FROM series_members m
              JOIN series s ON s.id = m.series_id
              WHERE m.series_id = ?1
                AND s.protected = 0
                AND (s.best_file IS NULL OR m.file_id <> s.best_file)",
            rusqlite::params![id, pc_core::time::now_unix()],
        )?;
        tx.commit()?;
        Ok(json!({"ok":true,"rejected":n}))
    })
}

/// Take back every rejection in one series.
pub async fn keep_all(State(st): State<Arc<AppState>>, Path(id): Path<i64>) -> Response {
    mutate(&st, |db| {
        let n = db.conn.execute(
            "DELETE FROM manual_rejects WHERE file_id IN
               (SELECT file_id FROM series_members WHERE series_id=?1)",
            [id],
        )?;
        Ok(json!({"ok":true,"restored":n}))
    })
}

pub async fn category(
    State(st): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(v): Json<Value>,
) -> Response {
    mutate(&st, |db| {
        let cat = v["category"]
            .as_str()
            .context(pc_core::tr!("Не указан вид", "No kind given"))?;
        pc_family::categories::Category::parse(cat)
            .context(pc_core::tr!("Неизвестный вид", "Unknown kind"))?;
        db.set_category_manual(id, cat)?;
        Ok(json!({"ok":true}))
    })
}
pub async fn date(
    State(st): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(v): Json<Value>,
) -> Response {
    mutate(&st, |db| {
        let ts = v["taken_at"].as_i64().context(pc_core::tr!(
            "Дата должна быть unix timestamp",
            "The date has to be a unix timestamp"
        ))?;
        if !(-5_364_662_400..=pc_core::time::now_unix() + 86400).contains(&ts) {
            bail!(
                "{}",
                pc_core::tr!(
                    "Дата за пределами допустимого диапазона",
                    "The date is out of range"
                )
            );
        }
        let mut ids = vec![id];
        if let Some(a) = v["file_ids"].as_array() {
            ids = a
                .iter()
                .map(|v| {
                    v.as_i64().filter(|id| *id > 0).context(pc_core::tr!(
                        "Некорректный номер файла",
                        "Invalid file number"
                    ))
                })
                .collect::<Result<Vec<_>>>()?;
            if ids.is_empty() {
                bail!(
                    "{}",
                    pc_core::tr!(
                        "Не выбраны файлы для правки даты",
                        "No files chosen for the date change"
                    )
                );
            }
        }
        let tx = db.conn.unchecked_transaction()?;
        for file in ids {
            db.conn.execute("INSERT INTO meta(file_id,taken_at,date_source) VALUES(?1,?2,'manual') ON CONFLICT(file_id) DO UPDATE SET taken_at=excluded.taken_at,date_source='manual'",[file,ts])?;
        }
        tx.commit()?;
        Ok(json!({"ok":true}))
    })
}

/// What travelled with the frame, as the journal recorded it, for a preview
/// of undoing or deleting that operation. `None` for entries written before
/// the journal kept a list, which are still read off the disk.
fn manifest_companions(
    e: &pc_db::JournalEntry,
    dst_of: impl Fn(&pc_db::Moved) -> Value,
) -> Option<Value> {
    if e.manifest.is_empty() {
        return None;
    }
    Some(json!(e
        .manifest
        .iter()
        .filter(|m| m.src != e.src)
        .map(|m| {
            let size = std::fs::metadata(&m.dst).map(|md| md.len()).unwrap_or(0);
            json!({"path": m.dst, "dst": dst_of(m), "size": size})
        })
        .collect::<Vec<_>>()))
}

fn companion_destination(src: &FsPath, dst: &FsPath, side: &FsPath) -> PathBuf {
    let stem = |p: &FsPath| {
        p.file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    };
    let old = stem(src);
    let new = stem(dst);
    let name = side.file_name().unwrap_or_default().to_string_lossy();
    let renamed = if old == new {
        name.into_owned()
    } else if let Some(rest) = name.strip_prefix(&old) {
        format!("{new}{rest}")
    } else if let Some(rest) = name.strip_prefix("._").and_then(|n| n.strip_prefix(&old)) {
        format!("._{new}{rest}")
    } else {
        name.into_owned()
    };
    dst.with_file_name(renamed)
}
