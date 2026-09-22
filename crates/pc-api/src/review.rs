use crate::{routes, service, AppState};
use anyhow::{ensure, Context, Result};
use axum::{
    extract::{Path, Query, State},
    response::Response,
    Json,
};
use pc_family::review::{self, Group};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

fn token(groups: &[Group]) -> String {
    blake3::hash(
        serde_json::to_string(groups)
            .expect("serializable groups")
            .as_bytes(),
    )
    .to_hex()
    .to_string()
}
#[derive(Default, Deserialize)]
pub struct Filter {
    #[serde(default)]
    search: String,
    #[serde(default)]
    queue: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    offset: usize,
    limit: Option<usize>,
}
pub async fn queue(State(st): State<Arc<AppState>>, Query(q): Query<Filter>) -> Response {
    let db = st.db.lock().unwrap();
    service::respond((|| {
        let groups = review::groups(&db, None)?;
        let mut counts = json!({"pending":0,"plan":0,"keep":0,"defer":0});
        for g in &groups {
            counts[&g.state] = json!(counts[&g.state].as_u64().unwrap_or(0) + 1);
        }
        let search = q.search.to_lowercase();
        let filtered: Vec<_> = groups
            .into_iter()
            .filter(|g| {
                (q.queue.is_empty() || q.queue == "all" || q.queue == g.state)
                    && (q.kind.is_empty()
                        || q.kind == "all"
                        || (q.kind == "exact" && g.exact)
                        || (q.kind == "versions" && !g.exact))
                    && (search.is_empty()
                        || g.paths.iter().any(|p| p.to_lowercase().contains(&search)))
            })
            .collect();
        let total = filtered.len();
        let offset = q.offset.min(total.saturating_sub(1));
        let mut items = Vec::new();
        for g in filtered
            .iter()
            .skip(offset)
            .take(q.limit.unwrap_or(50).clamp(1, 100))
        {
            if let Some(family) = db.family(g.id)? {
                let mut out = serde_json::to_value(routes::to_out(family, &db))?;
                out["review_state"] = json!(g.state);
                out["exact"] = json!(g.exact);
                out["can_plan"] = json!(g.eligible);
                out["review_reasons"] = json!(g.reasons);
                out["review_token"] = json!(token(std::slice::from_ref(g)));
                items.push(out);
            }
        }
        Ok(json!({"total":total,"offset":offset,"groups":items,"counts":counts}))
    })())
}
#[derive(Deserialize)]
pub struct Decision {
    state: String,
    token: String,
}
pub async fn decide(
    State(st): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(body): Json<Decision>,
) -> Response {
    service::mutate(&st, |db| {
        let groups = review::groups(db, Some(id))?;
        ensure!(
            !groups.is_empty() && token(&groups) == body.token,
            "The group changed; refresh the queue"
        );
        Ok(json!({"operation":review::decide(db,&groups,&body.state)?}))
    })
}
fn batch_groups(db: &pc_db::Db) -> Result<Vec<Group>> {
    Ok(review::groups(db, None)?
        .into_iter()
        .filter(|g| g.untouched && g.eligible)
        .collect())
}
pub async fn batch_preview(State(st): State<Arc<AppState>>) -> Response {
    let db = st.db.lock().unwrap();
    service::respond((|| {
        let groups = batch_groups(&db)?;
        Ok(
            json!({"token":token(&groups),"groups":groups.len(),"files":groups.iter().map(|g|g.files.len()-1).sum::<usize>(),"bytes":groups.iter().map(|g|g.bytes).sum::<i64>()}),
        )
    })())
}
pub async fn batch(State(st): State<Arc<AppState>>, Json(body): Json<Value>) -> Response {
    service::mutate(&st, |db| {
        let groups = batch_groups(db)?;
        ensure!(
            body["token"].as_str() == Some(token(&groups).as_str()),
            "The batch changed; preview it again"
        );
        Ok(json!({"operation":review::decide(db,&groups,"plan")?}))
    })
}
pub async fn undo(State(st): State<Arc<AppState>>, Json(body): Json<Value>) -> Response {
    service::mutate(&st, |db| {
        db.undo_review(body["operation"].as_i64().context("No decision given")?)?;
        Ok(json!({"ok":true}))
    })
}
