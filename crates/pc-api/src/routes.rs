use axum::extract::{Path as AxPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::Json;
use pc_db::{BundleState, FamilyRow};
use pc_family::Role;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::AppState;

type Api<T> = Result<Json<T>, Fail>;

/// An error that reaches the browser as JSON rather than an empty 500.
pub struct Fail(anyhow::Error);

impl<E: Into<anyhow::Error>> From<E> for Fail {
    fn from(e: E) -> Self {
        Self(e.into())
    }
}

impl IntoResponse for Fail {
    fn into_response(self) -> Response {
        tracing::error!("{:#}", self.0);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": self.0.to_string() })),
        )
            .into_response()
    }
}

// ---- static assets ------------------------------------------------------

pub async fn index() -> Html<&'static str> {
    Html(include_str!("../web/dist/index.html"))
}

pub async fn asset(AxPath(file): AxPath<String>) -> Response {
    let body: &'static [u8] = match file.as_str() {
        "app.css" => include_bytes!("../web/dist/static/app.css"),
        "app.js" => include_bytes!("../web/dist/static/app.js"),
        _ => {
            return (
                StatusCode::NOT_FOUND,
                pc_core::tr!("нет такого файла", "no such file"),
            )
                .into_response()
        }
    };
    let mime = mime_guess::from_path(&file).first_or_octet_stream();
    // The bundle is compiled into the binary under a fixed name, so a browser
    // that caches it keeps showing the previous version after an upgrade —
    // with no way for the user to tell. The build stamps a tag; a browser that
    // holds a stale one is told to fetch again.
    (
        [
            (header::CONTENT_TYPE, mime.as_ref()),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        [(header::ETAG, asset_tag())],
        body,
    )
        .into_response()
}

/// One tag per build, so a rebuilt interface invalidates every cached bundle.
fn asset_tag() -> String {
    format!(
        "\"{}-{}\"",
        env!("CARGO_PKG_VERSION"),
        pc_core::thumbstore::hex32(&blake3_of(include_bytes!("../web/dist/static/app.js"))[..8])
    )
}

fn blake3_of(bytes: &[u8]) -> [u8; 32] {
    *blake3::Hasher::new().update(bytes).finalize().as_bytes()
}

// ---- dashboard ----------------------------------------------------------

#[derive(Serialize)]
pub struct RoleTotal {
    role: &'static str,
    label: &'static str,
    count: i64,
    bytes: i64,
    removable: bool,
}

#[derive(Serialize)]
pub struct Status {
    /// What is actually running, so an upgrade can be seen rather than
    /// assumed. Taken from the build, never written down by hand.
    version: &'static str,
    files: i64,
    images: i64,
    skipped: i64,
    families: i64,
    families_multi: i64,
    /// Built series and classified files, so the overview can tell a stage
    /// that has produced something from one that was never run.
    series: i64,
    categorised: i64,
    roles: Vec<RoleTotal>,
    derived_removable_bytes: i64,
    derived_blocked: i64,
    quarantined_bytes: i64,
    mislabelled: i64,
}

pub async fn status(State(st): State<Arc<AppState>>) -> Api<Status> {
    let db = st.db.lock().unwrap();
    let idx = db.index_stats()?;

    let roles = db
        .role_counts()?
        .into_iter()
        .map(|(role, count, bytes)| {
            let r = Role::parse(&role).unwrap_or(Role::Unknown);
            RoleTotal {
                role: r.as_str(),
                label: r.label(),
                count,
                bytes,
                removable: r.removable_by_default(),
            }
        })
        .collect();

    let bundles = db.list_bundles(&pc_db::model::BundleFilter {
        state: Some(BundleState::Present),
        ..Default::default()
    })?;
    let derived_removable_bytes = bundles
        .iter()
        .filter(|b| b.removable())
        .map(|b| b.size)
        .sum();
    let derived_blocked = bundles.iter().filter(|b| !b.removable()).count() as i64;
    let quarantined_bytes = db.journal_quarantined(None)?.iter().map(|e| e.size).sum();

    Ok(Json(Status {
        version: env!("CARGO_PKG_VERSION"),
        files: idx.total,
        images: idx.images,
        skipped: idx.skipped,
        families: db.family_count(false)?,
        families_multi: db.family_count(true)?,
        series: db
            .conn
            .query_row("SELECT count(*) FROM series", [], |r| r.get(0))?,
        categorised: db
            .conn
            .query_row("SELECT count(*) FROM file_categories", [], |r| r.get(0))?,
        roles,
        derived_removable_bytes,
        derived_blocked,
        quarantined_bytes,
        mislabelled: db.mislabelled_count()?,
    }))
}

// ---- families -----------------------------------------------------------

#[derive(Deserialize)]
pub struct FamiliesQuery {
    #[serde(default = "default_limit")]
    limit: i64,
    #[serde(default)]
    offset: i64,
    /// Include families of a single file.
    #[serde(default)]
    all: bool,
    #[serde(default)]
    search: String,
    #[serde(default)]
    role: String,
    #[serde(default)]
    disk: String,
    #[serde(default)]
    min_bytes: i64,
    #[serde(default)]
    sort: String,
}

fn default_limit() -> i64 {
    30
}

#[derive(Serialize)]
pub struct MemberOut {
    file_id: i64,
    name: String,
    dir: String,
    role: &'static str,
    role_label: &'static str,
    removable: bool,
    size: i64,
    width: i64,
    height: i64,
    container: String,
    quality: f64,
    breakdown: String,
    evidence: Option<serde_json::Value>,
    thumb: Option<String>,
    is_keeper: bool,
    sidecars: Vec<String>,
    catalogs: Vec<String>,
    rating: Option<i64>,
}

#[derive(Serialize)]
pub struct FamilyOut {
    id: i64,
    taken_at: Option<i64>,
    camera: Option<String>,
    total_size: i64,
    removable_bytes: i64,
    members: Vec<MemberOut>,
}

fn role_rank(r: Role) -> u8 {
    match r {
        Role::Original => 0,
        Role::CameraJpeg => 1,
        Role::Converted => 2,
        Role::Export => 3,
        Role::Resize => 4,
        Role::Copy => 5,
        Role::Unknown => 6,
    }
}

fn to_out(f: FamilyRow, db: &pc_db::Db) -> FamilyOut {
    let total_size = f.total_size();
    let mut members: Vec<MemberOut> = f
        .members
        .into_iter()
        .map(|m| {
            let role = Role::parse(&m.role).unwrap_or(Role::Unknown);
            let catalogs = crate::jobs::rows(db,"SELECT c.name,lf.rating FROM lr_files lf JOIN lr_catalogs c ON c.id=lf.catalog_id WHERE lf.path=?1 AND c.is_backup=0",&[&m.path]).unwrap_or_default();
            MemberOut {
                sidecars: pc_apply::companions(std::path::Path::new(&m.path)).iter().map(|p|p.display().to_string()).collect(),
                rating: catalogs.iter().filter_map(|v|v["rating"].as_i64()).max(),
                catalogs: catalogs.iter().filter_map(|v|v["name"].as_str().map(str::to_string)).collect(),
                file_id: m.file_id,
                dir: pc_core::dir_name(&m.path).to_string(),
                name: m.name,
                role: role.as_str(),
                role_label: role.label(),
                removable: role.removable_by_default(),
                size: m.size,
                width: m.width,
                height: m.height,
                container: m.container,
                quality: m.quality,
                breakdown: m.breakdown,
                evidence: m.evidence.and_then(|e| serde_json::from_str(&e).ok()),
                thumb: m.thumb_key,
                is_keeper: m.is_keeper,
            }
        })
        .collect();
    // Derivation order, so the tree reads top-down from the photograph.
    members.sort_by_key(|m| {
        (
            !m.is_keeper,
            role_rank(Role::parse(m.role).unwrap_or(Role::Unknown)),
            std::cmp::Reverse(m.size),
        )
    });
    FamilyOut {
        id: f.id,
        taken_at: f.taken_at,
        camera: f.camera,
        total_size,
        removable_bytes: members
            .iter()
            .filter(|m| m.removable && !m.is_keeper)
            .map(|m| m.size)
            .sum(),
        members,
    }
}

#[derive(Serialize)]
pub struct FamiliesPage {
    total: i64,
    families: Vec<FamilyOut>,
}

pub async fn families(
    State(st): State<Arc<AppState>>,
    Query(q): Query<FamiliesQuery>,
) -> Api<FamiliesPage> {
    let db = st.db.lock().unwrap();
    // Ten thousand groups is more than anyone reads through, so the order is
    // the tool: heaviest first to win space, by folder to work an archive
    // through section by section, oldest first to go year by year.
    let order = match q.sort.as_str() {
        "date" => "fa.taken_at DESC",
        "date-asc" => "fa.taken_at IS NULL,fa.taken_at ASC",
        "count" => "COUNT(*) DESC",
        "size" => "SUM(f.size) DESC",
        "biggest" => "MAX(f.size) DESC",
        "path" => "MIN(f.path) ASC",
        _ => "SUM(CASE WHEN fm.role='copy' AND f.id != fa.keeper_file THEN f.size ELSE 0 END) DESC",
    };
    let base = "FROM families fa JOIN family_members fm ON fm.family_id=fa.id JOIN files f ON f.id=fm.file_id WHERE f.state='present' GROUP BY fa.id HAVING (?1 OR COUNT(*)>1) AND (?2='' OR MAX(instr(lower(f.path),lower(?2)))>0) AND (?3='' OR MAX(fm.role=?3)) AND (?4='' OR MAX(instr(f.disk,?4))>0) AND SUM(f.size)>=?5";
    let args: [&dyn rusqlite::ToSql; 5] = [&q.all, &q.search, &q.role, &q.disk, &q.min_bytes];
    let total: i64 = db.conn.query_row(
        &format!("SELECT COUNT(*) FROM (SELECT fa.id {base})"),
        args,
        |r| r.get(0),
    )?;
    let mut stmt = db.conn.prepare(&format!(
        "SELECT fa.id {base} ORDER BY {order},fa.id LIMIT {} OFFSET {}",
        q.limit.clamp(1, 200),
        q.offset.max(0)
    ))?;
    let ids = stmt
        .query_map(args, |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut families = Vec::new();
    for id in ids {
        if let Some(f) = db.family(id)? {
            families.push(to_out(f, &db));
        }
    }
    Ok(Json(FamiliesPage { total, families }))
}

pub async fn family(State(st): State<Arc<AppState>>, AxPath(id): AxPath<i64>) -> Response {
    let db = st.db.lock().unwrap();
    match db.family(id) {
        Ok(Some(f)) => Json(to_out(f, &db)).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            pc_core::tr!("нет такого семейства", "no such group"),
        )
            .into_response(),
        Err(e) => Fail(e).into_response(),
    }
}

#[derive(Deserialize)]
pub struct KeeperBody {
    file_id: i64,
}

/// Change which member the family presents as its best version.
pub async fn set_keeper(
    State(st): State<Arc<AppState>>,
    AxPath(id): AxPath<i64>,
    Json(body): Json<KeeperBody>,
) -> Response {
    crate::service::mutate(&st, |db| {
        let tx = db.conn.unchecked_transaction()?;
        if !db.set_family_keeper(id, body.file_id)? {
            anyhow::bail!(
                "{}",
                pc_core::tr!(
                    "файл не входит в это семейство",
                    "the file is not in that group"
                )
            );
        }
        db.conn.execute("DELETE FROM manual_keepers WHERE file_id IN (SELECT file_id FROM family_members WHERE family_id=?1)",[id])?;
        db.conn.execute(
            "INSERT OR IGNORE INTO manual_keepers VALUES(?1)",
            [body.file_id],
        )?;
        tx.commit()?;
        Ok(serde_json::json!({"ok":true}))
    })
}

// ---- derived data -------------------------------------------------------

#[derive(Serialize)]
pub struct BundleOut {
    id: i64,
    path: String,
    kind: &'static str,
    kind_label: &'static str,
    file_count: i64,
    size: i64,
    removable: bool,
    regenerable: bool,
    blocked: Option<String>,
    hint: Option<String>,
    state: &'static str,
}

pub async fn derived(State(st): State<Arc<AppState>>) -> Api<Vec<BundleOut>> {
    let db = st.db.lock().unwrap();
    let rows = db.list_bundles(&pc_db::model::BundleFilter::default())?;
    Ok(Json(
        rows.into_iter()
            .map(|b| BundleOut {
                id: b.id,
                path: b.path,
                kind: b.kind.as_str(),
                kind_label: b.kind.label(),
                file_count: b.file_count,
                size: b.size,
                removable: b.regenerable
                    && b.blocked_code.is_none()
                    && b.state == BundleState::Present,
                regenerable: b.regenerable,
                blocked: b.blocked_detail,
                hint: b.rebuild_cost_hint,
                state: match b.state {
                    BundleState::Present => "present",
                    BundleState::Quarantined => "quarantined",
                    BundleState::Purged => "purged",
                },
            })
            .collect(),
    ))
}

// ---- pixels -------------------------------------------------------------

pub async fn thumb(State(st): State<Arc<AppState>>, AxPath(key): AxPath<String>) -> Response {
    // The key addresses content, so it is safe to cache hard; but reject
    // anything that is not a plain hex key before touching the filesystem.
    if key.len() != 32 || !key.bytes().all(|c| c.is_ascii_hexdigit()) {
        return (
            StatusCode::BAD_REQUEST,
            pc_core::tr!("некорректный ключ", "malformed key"),
        )
            .into_response();
    }
    match st.thumbs.get(&key) {
        Some(bytes) => (
            [
                (header::CONTENT_TYPE, "image/jpeg"),
                (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
            ],
            bytes,
        )
            .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            pc_core::tr!("нет тамбнейла", "no thumbnail"),
        )
            .into_response(),
    }
}

/// The original frame, for looking closely.
///
/// Only paths the index already knows are served: the id is looked up in the
/// database and the stored path used, so nothing the caller sends can reach
/// a file the tool has not itself catalogued.
pub async fn original(State(st): State<Arc<AppState>>, AxPath(id): AxPath<i64>) -> Response {
    let path = {
        let db = st.db.lock().unwrap();
        match db.file_path_now(id) {
            Ok(Some(p)) => p,
            Ok(None) => {
                return (
                    StatusCode::NOT_FOUND,
                    pc_core::tr!("нет такого файла", "no such file"),
                )
                    .into_response()
            }
            Err(e) => return Fail(e).into_response(),
        }
    };
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.as_ref())], bytes).into_response()
        }
        Err(e) => (
            StatusCode::NOT_FOUND,
            pc_core::tf!("не прочитать {0}: {1}", "cannot read {0}: {1}", path, e),
        )
            .into_response(),
    }
}

// ---- policy, plan and the move ------------------------------------------

#[derive(Deserialize)]
pub struct PlanQuery {
    /// Comma separated role names. Empty means the default: copies only.
    #[serde(default)]
    roles: String,
    /// Lift the protection on frames a Lightroom catalog curates.
    #[serde(default)]
    allow_lightroom: bool,
    #[serde(default = "default_resize")]
    resize_below: i64,
}

fn default_resize() -> i64 {
    2_000_000
}

impl PlanQuery {
    fn to_policy(&self) -> Result<pc_family::Policy, String> {
        let mut p = pc_family::Policy {
            resize_below_pixels: self.resize_below.max(0),
            respect_lightroom: !self.allow_lightroom,
            ..Default::default()
        };
        let names: Vec<&str> = self
            .roles
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        if !names.is_empty() {
            let mut set = std::collections::BTreeSet::new();
            for n in names {
                let role = Role::parse(n).ok_or_else(|| {
                    pc_core::tf!("неизвестная роль «{0}»", "unknown role “{0}”", n)
                })?;
                // Refusing this in the API as well as the CLI: the original
                // is the photograph, and no combination of toggles in a
                // browser should be able to schedule it for removal.
                if role == Role::Original {
                    return Err(pc_core::tr!(
                        "роль original удалять нельзя: это сам снимок",
                        "the original role is never removed: it is the photograph itself"
                    )
                    .into());
                }
                set.insert(role);
            }
            p.remove_roles = set;
        }
        Ok(p)
    }
}

#[derive(Serialize)]
pub struct PlanItem {
    file_id: i64,
    path: String,
    name: String,
    size: i64,
    role: &'static str,
    role_label: &'static str,
    reason: String,
    keeper_id: i64,
    keeper_path: String,
    thumb: Option<String>,
    keeper_thumb: Option<String>,
}

#[derive(Serialize)]
pub struct PlanOut {
    total_files: usize,
    total_bytes: i64,
    items: Vec<PlanItem>,
    refusals: Vec<RefusalOut>,
    roles: Vec<&'static str>,
    respect_lightroom: bool,
}

#[derive(Serialize)]
pub struct RefusalOut {
    path: String,
    why: String,
}

fn build_plan(st: &AppState, q: &PlanQuery) -> Result<PlanOut, Box<Response>> {
    let policy = q.to_policy().map_err(|e| {
        Box::new(
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": e })),
            )
                .into_response(),
        )
    })?;

    let db = st.db.lock().unwrap();
    let plan = pc_family::plan::compute(&db, &policy).map_err(|e| Fail(e).into_response())?;

    // Thumbnails for both sides, so the user can see what goes and what
    // stays rather than reading two paths and hoping.
    let thumb_of = |id: i64| db.file(id).ok().flatten().and_then(|f| f.thumb_key);

    let items = plan
        .candidates
        .iter()
        .take(500)
        .map(|c| PlanItem {
            file_id: c.file_id,
            name: pc_core::base_name(&c.path).to_string(),
            path: c.path.clone(),
            size: c.size,
            role: c.role.as_str(),
            role_label: c.role.label(),
            reason: c.reason.clone(),
            keeper_id: c.keeper_id,
            keeper_path: c.keeper_path.clone(),
            thumb: thumb_of(c.file_id),
            keeper_thumb: thumb_of(c.keeper_id),
        })
        .collect();

    Ok(PlanOut {
        total_files: plan.candidates.len(),
        total_bytes: plan.bytes(),
        items,
        refusals: plan
            .refusals
            .iter()
            .take(200)
            .map(|r| RefusalOut {
                path: r.path.clone(),
                why: r.why.clone(),
            })
            .collect(),
        roles: policy.remove_roles.iter().map(|r| r.as_str()).collect(),
        respect_lightroom: policy.respect_lightroom,
    })
}

pub async fn plan(State(st): State<Arc<AppState>>, Query(q): Query<PlanQuery>) -> Response {
    match build_plan(&st, &q) {
        Ok(p) => Json(p).into_response(),
        Err(r) => *r,
    }
}

/// Move what the plan proposes. Reversible, and the caller has confirmed.
pub async fn apply_plan(State(st): State<Arc<AppState>>, Query(q): Query<PlanQuery>) -> Response {
    let _ = (st, q);
    crate::service::error(
        409,
        pc_core::tr!(
            "Откройте предпросмотр /api/preview и запустите /api/jobs с его plan_token",
            "Open the /api/preview preview and start /api/jobs with its plan_token"
        ),
    )
}

#[derive(Serialize)]
pub struct QuarantineItem {
    journal_id: i64,
    /// The indexed file, when the entry is a photograph rather than a bundle
    /// of regenerable previews. What makes a thumbnail possible.
    file_id: Option<i64>,
    src: String,
    /// Where it sits now, so the user can find it without the tool.
    dst: Option<String>,
    name: String,
    size: i64,
    file_count: i64,
    applied_at: i64,
    kind: String,
    thumb: Option<String>,
}

pub async fn quarantine(State(st): State<Arc<AppState>>) -> Api<Vec<QuarantineItem>> {
    let db = st.db.lock().unwrap();
    Ok(Json(
        db.journal_quarantined(None)?
            .into_iter()
            .map(|e| {
                let is_file = e.op == "quarantine-file";
                let file_id = is_file.then_some(e.target_id).flatten();
                QuarantineItem {
                    journal_id: e.id,
                    file_id,
                    name: pc_core::base_name(&e.src).to_string(),
                    kind: if is_file {
                        pc_core::tr!("снимок", "photograph").into()
                    } else {
                        pc_core::tr!("производные данные", "derived data").into()
                    },
                    thumb: file_id
                        .and_then(|id| db.file(id).ok().flatten())
                        .and_then(|f| f.thumb_key),
                    src: e.src,
                    dst: e.dst,
                    size: e.size,
                    file_count: e.file_count,
                    applied_at: e.applied_at,
                }
            })
            .collect(),
    ))
}

pub async fn undo(State(st): State<Arc<AppState>>, AxPath(id): AxPath<i64>) -> Response {
    let _ = (st, id);
    crate::service::error(
        409,
        pc_core::tr!(
            "Откат требует предпросмотра /api/preview и задачи /api/jobs",
            "Undoing needs an /api/preview preview and an /api/jobs job"
        ),
    )
}

// ---- reorganisation -----------------------------------------------------

#[derive(Deserialize)]
pub struct OrganizeQuery {
    /// Root of the new tree. Empty means the user has not chosen one yet.
    #[serde(default)]
    root: String,
    /// Hours between shots that separate two events.
    #[serde(default)]
    gap_hours: Option<f64>,
    #[serde(default)]
    allow_lightroom: bool,
    #[serde(default)]
    skip_uncertain: bool,
}

#[derive(Serialize)]
pub struct OrganizeMoveOut {
    src: String,
    dst: String,
    /// `dst` without the root, which is what the user is actually reading.
    rel: String,
    event: String,
    size: i64,
    source: &'static str,
    uncertain: bool,
    renamed_from: Option<String>,
}

#[derive(Serialize)]
pub struct OrganizeEventOut {
    name: String,
    year: String,
    count: usize,
    bytes: i64,
}

#[derive(Serialize)]
pub struct CountOut {
    label: String,
    count: usize,
}

#[derive(Serialize)]
pub struct OrganizeOut {
    root: String,
    gap_hours: f64,
    respect_lightroom: bool,
    total_files: usize,
    total_bytes: i64,
    events: Vec<OrganizeEventOut>,
    already_placed: usize,
    renamed: usize,
    uncertain: usize,
    by_source: Vec<CountOut>,
    refusals: Vec<CountOut>,
    sample: Vec<OrganizeMoveOut>,
    /// What to type to carry it out. The browser shows the plan; the move
    /// itself is a deliberate act at the command line.
    command: String,
}

const ORGANIZE_SAMPLE: usize = 60;

pub async fn organize(State(st): State<Arc<AppState>>, Query(q): Query<OrganizeQuery>) -> Response {
    if q.root.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": pc_core::tr!(
                    "не указан корень нового дерева",
                    "no root given for the new tree"
                )
            })),
        )
            .into_response();
    }
    let gap_hours = q.gap_hours.unwrap_or(6.0).clamp(0.25, 72.0);
    let opts = pc_organize::Options {
        root: std::path::PathBuf::from(q.root.trim()),
        gap_secs: (gap_hours * 3600.0) as i64,
        respect_lightroom: !q.allow_lightroom,
        skip_uncertain: q.skip_uncertain,
    };

    let db = st.db.lock().unwrap();
    let plan = match pc_organize::compute(&db, &opts) {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": format!("{e:#}") })),
            )
                .into_response()
        }
    };

    let root_str = opts.root.to_string_lossy().into_owned();
    let rel = |dst: &str| -> String {
        dst.strip_prefix(&root_str)
            .map(|p| pc_core::trim_leading_separators(p).to_string())
            .unwrap_or_else(|| dst.to_string())
    };

    let mut events: Vec<OrganizeEventOut> = Vec::new();
    for m in &plan.moves {
        let year = pc_core::path_parts(&rel(&m.dst))
            .first()
            .copied()
            .unwrap_or("")
            .to_string();
        match events
            .iter_mut()
            .find(|e| e.name == m.event && e.year == year)
        {
            Some(e) => {
                e.count += 1;
                e.bytes += m.size;
            }
            None => events.push(OrganizeEventOut {
                name: m.event.clone(),
                year,
                count: 1,
                bytes: m.size,
            }),
        }
    }
    events.sort_by(|a, b| a.year.cmp(&b.year).then(a.name.cmp(&b.name)));

    let mut refusals: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for r in &plan.refusals {
        let head = r.why.split(" — ").next().unwrap_or(&r.why).to_string();
        *refusals.entry(head).or_default() += 1;
    }

    Json(OrganizeOut {
        total_files: plan.moves.len(),
        total_bytes: plan.bytes(),
        already_placed: plan.already_placed,
        renamed: plan.renamed,
        uncertain: plan.uncertain,
        by_source: plan
            .by_source
            .iter()
            .map(|(s, n)| CountOut {
                label: s.label().to_string(),
                count: *n,
            })
            .collect(),
        refusals: refusals
            .into_iter()
            .map(|(label, count)| CountOut { label, count })
            .collect(),
        sample: plan
            .moves
            .iter()
            .take(ORGANIZE_SAMPLE)
            .map(|m| OrganizeMoveOut {
                rel: rel(&m.dst),
                src: m.src.clone(),
                dst: m.dst.clone(),
                event: m.event.clone(),
                size: m.size,
                source: m.date.source.label(),
                uncertain: m.date.uncertain(),
                renamed_from: m.renamed_from.clone(),
            })
            .collect(),
        events,
        command: format!(
            "photo-cleanup --db {} organize apply --root {} --gap {}h{}{} --yes",
            st.db_path.display(),
            root_str,
            gap_hours,
            if q.allow_lightroom {
                " --allow-lightroom"
            } else {
                ""
            },
            if q.skip_uncertain {
                " --skip-uncertain"
            } else {
                ""
            }
        ),
        respect_lightroom: opts.respect_lightroom,
        gap_hours,
        root: root_str,
    })
    .into_response()
}

// ---- series -------------------------------------------------------------

#[derive(Deserialize)]
pub struct SeriesQuery {
    #[serde(default = "default_series_limit")]
    limit: i64,
    #[serde(default)]
    offset: i64,
}

fn default_series_limit() -> i64 {
    20
}

#[derive(Serialize)]
pub struct SeriesMemberOut {
    file_id: i64,
    name: String,
    dir: String,
    rank: i64,
    score: f64,
    breakdown: String,
    sharpness: Option<f64>,
    thumb: Option<String>,
    taken_at: Option<i64>,
    is_best: bool,
    is_rejected: bool,
    family_id: Option<i64>,
    family_size: i64,
    is_family_keeper: bool,
}

#[derive(Serialize)]
pub struct SeriesOut {
    id: i64,
    kind: String,
    label: &'static str,
    started_at: Option<i64>,
    camera: Option<String>,
    protected: bool,
    members: Vec<SeriesMemberOut>,
}

#[derive(Serialize)]
pub struct SeriesPage {
    total: i64,
    series: Vec<SeriesOut>,
}

pub async fn series(
    State(st): State<Arc<AppState>>,
    Query(q): Query<SeriesQuery>,
) -> Api<SeriesPage> {
    let db = st.db.lock().unwrap();
    let rows = db.series_list(q.limit.clamp(1, 100), q.offset.max(0))?;
    Ok(Json(SeriesPage {
        total: db.series_count()?,
        series: rows
            .into_iter()
            .map(|s| SeriesOut {
                id: s.id,
                label: match s.kind.as_str() {
                    "pixel-shift" => "pixel-shift",
                    "bracket" => pc_core::tr!("брекетинг", "bracketing"),
                    _ => pc_core::tr!("серия", "burst"),
                },
                kind: s.kind,
                started_at: s.started_at,
                camera: s.camera,
                protected: s.protected,
                members: s
                    .members
                    .into_iter()
                    .map(|m| SeriesMemberOut {
                        file_id: m.file_id,
                        dir: pc_core::dir_name(&m.path).to_string(),
                        name: m.name,
                        rank: m.rank,
                        score: m.score,
                        breakdown: m.breakdown,
                        sharpness: m.sharpness,
                        thumb: m.thumb_key,
                        taken_at: m.taken_at,
                        is_best: m.is_best,
                        is_rejected: m.is_rejected,
                        family_id: m.family_id,
                        family_size: m.family_size,
                        is_family_keeper: m.is_family_keeper,
                    })
                    .collect(),
            })
            .collect(),
    }))
}

// ---- categories ---------------------------------------------------------

#[derive(Serialize)]
pub struct CategoryFile {
    file_id: i64,
    name: String,
    dir: String,
    size: i64,
    width: i64,
    height: i64,
    confidence: f64,
    manual: bool,
    evidence: String,
    thumb: Option<String>,
}

#[derive(Serialize)]
pub struct CategoryGroup {
    key: String,
    label: &'static str,
    count: i64,
    bytes: i64,
    files: Vec<CategoryFile>,
}

pub async fn categories(State(st): State<Arc<AppState>>) -> Api<Vec<CategoryGroup>> {
    use pc_family::categories::Category;
    let db = st.db.lock().unwrap();
    let mut out = Vec::new();
    let manual: std::collections::HashSet<i64> = db
        .conn
        .prepare("SELECT file_id FROM file_categories WHERE manual=1")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for c in db.category_counts()? {
        let label = Category::parse(&c.category)
            .map(|x| x.label())
            .unwrap_or(pc_core::tr!("Прочее", "Other"));
        let files = db
            .files_in_category(&c.category, 100_000)?
            .into_iter()
            .map(|m| CategoryFile {
                file_id: m.file_id,
                dir: pc_core::dir_name(&m.path).to_string(),
                name: m.name,
                size: m.size,
                width: m.width,
                height: m.height,
                // files_in_category reuses MemberRow: confidence rides in
                // `quality`, the evidence string in `breakdown`.
                manual: manual.contains(&m.file_id),
                confidence: m.quality,
                evidence: m.breakdown,
                thumb: m.thumb_key,
            })
            .collect();
        out.push(CategoryGroup {
            key: c.category,
            label,
            count: c.count,
            bytes: c.bytes,
            files,
        });
    }
    Ok(Json(out))
}

/// Full raster on demand; RAW uses its embedded JPEG, not a thumbnail upsample.
pub async fn full_preview(State(st): State<Arc<AppState>>, AxPath(id): AxPath<i64>) -> Response {
    // Wherever the bytes are now: a frame waiting in quarantine is precisely
    // the one worth looking at closely before it is deleted for good.
    let path = {
        let db = st.db.lock().unwrap();
        match db.file_path_now(id) {
            Ok(Some(p)) => p,
            _ => {
                return crate::service::error(
                    404,
                    pc_core::tr!("Файл не найден в индексе", "File not found in the index"),
                )
            }
        }
    };
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
        let p = std::path::Path::new(&path);
        let read = pc_image::read_for_probe(
            p,
            std::fs::metadata(p)
                .map_err(|e| anyhow::anyhow!("{path}: {e}"))?
                .len(),
        )?;
        if read.complete && read.container == pc_image::Container::Jpeg {
            return Ok(read.head);
        }
        if let Some(preview) = read.preview {
            return Ok(preview);
        }
        let image =
            image::load_from_memory(&read.head).map_err(|e| anyhow::anyhow!("{path}: {e}"))?;
        let mut bytes = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 95).encode_image(&image)?;
        Ok(bytes)
    })
    .await;
    match result {
        Ok(Ok(bytes)) => ([(header::CONTENT_TYPE, "image/jpeg")], bytes).into_response(),
        Ok(Err(e)) => crate::service::error(400, &format!("{e:#}")),
        Err(e) => crate::service::error(500, &e.to_string()),
    }
}
