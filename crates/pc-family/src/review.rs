//! Review proposes exact-copy decisions; it never moves a file.
use crate::{
    plan::{self, Plan, Policy, Scope},
    Role,
};
use anyhow::{ensure, Result};
use pc_db::{Db, PlanRow};
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};

#[derive(Serialize)]
pub struct Group {
    pub id: i64,
    pub state: String,
    pub exact: bool,
    pub eligible: bool,
    pub bytes: i64,
    pub files: Vec<i64>,
    pub snapshot: String,
    pub revision: Vec<Option<i64>>,
    pub paths: Vec<String>,
    pub untouched: bool,
    pub reasons: Vec<String>,
    pub decision_source: Option<String>,
}

/// Existing decisions are authoritative too: the queue is another view of
/// the same archive, not a separate approval database.
struct Sources {
    keepers: HashSet<i64>,
    rejected: HashSet<i64>,
    marks: pc_db::Marks,
}
impl Sources {
    fn load(db: &Db) -> Result<Self> {
        let mut stmt = db
            .conn
            .prepare("SELECT file_id FROM manual_keepers WHERE source='hand'")?;
        let keepers = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let rejected = db
            .rejected_rows_scoped(None)?
            .into_iter()
            .map(|r| r.file_id)
            .collect();
        Ok(Self {
            keepers,
            rejected,
            marks: db.original_marks()?,
        })
    }
    fn keeper_source(&self, keeper: &PlanRow) -> Option<&'static str> {
        if self.keepers.contains(&keeper.file_id) {
            Some("manual")
        } else if self.marks.covering(&keeper.path).is_some() {
            Some("folder")
        } else {
            None
        }
    }
}

pub fn groups(db: &Db, family: Option<i64>) -> Result<Vec<Group>> {
    let rows = db.plan_rows_scoped(family, None, None)?;
    let choices = db.review_choices()?;
    let sources = Sources::load(db)?;
    let plan = plan::compute_before_review(
        db,
        &Policy::default(),
        &Scope {
            family,
            ..Default::default()
        },
    )?;
    let candidates: HashSet<_> = plan
        .candidates
        .iter()
        .filter(|c| c.role == Role::Copy && !c.manual)
        .map(|c| c.file_id)
        .collect();
    let mut reasons: BTreeMap<i64, Vec<String>> = BTreeMap::new();
    for refusal in &plan.refusals {
        reasons
            .entry(refusal.family_id)
            .or_default()
            .push(refusal.why.clone());
    }
    let mut grouped: BTreeMap<i64, Vec<PlanRow>> = BTreeMap::new();
    for row in rows {
        grouped.entry(row.family_id).or_default().push(row);
    }
    let mut result = Vec::new();
    for (id, mut members) in grouped {
        if members.len() < 2 {
            continue;
        }
        members.sort_by_key(|m| m.file_id);
        // Group ids can be reused after rebuilding; identity contains the
        // actual members, keeper and indexed evidence, excluding that id.
        let snapshot = serde_json::to_string(
            &members
                .iter()
                .map(|m| {
                    serde_json::json!([
                        m.file_id,
                        m.is_keeper,
                        m.path,
                        m.role,
                        m.size,
                        m.mtime,
                        m.content_hash,
                        m.pixel_hash,
                        m.pixel_source,
                        m.partial_hash
                    ])
                })
                .collect::<Vec<_>>(),
        )?;
        let keeper = members.iter().find(|m| m.is_keeper);
        let exact = keeper.and_then(plan::identity).is_some_and(|identity| {
            members
                .iter()
                .all(|m| plan::identity(m).as_ref() == Some(&identity))
        });
        let eligible = exact
            && keeper.is_some()
            && members
                .iter()
                .filter(|m| !m.is_keeper)
                .all(|m| candidates.contains(&m.file_id));
        let states: Vec<_> = members.iter().map(|m| choices.get(&m.file_id)).collect();
        let inherited = keeper.and_then(|k| sources.keeper_source(k)).or_else(|| {
            members
                .iter()
                .any(|m| sources.rejected.contains(&m.file_id))
                .then_some("manual")
        });
        let state = states
            .first()
            .copied()
            .flatten()
            .filter(|first| {
                states.iter().all(|v| {
                    v.is_some_and(|c| {
                        c.state == first.state && (c.state != "plan" || c.snapshot == snapshot)
                    })
                })
            })
            .map(|v| v.state.clone())
            .unwrap_or_else(|| {
                if inherited.is_some() {
                    "reviewed"
                } else {
                    "pending"
                }
                .into()
            });
        result.push(Group {
            decision_source: if state != "pending" && state != "reviewed" {
                Some("queue".into())
            } else {
                inherited.map(str::to_string)
            },
            reasons: reasons.remove(&id).unwrap_or_default(),
            id,
            state,
            exact,
            eligible,
            bytes: members
                .iter()
                .filter(|m| !m.is_keeper)
                .map(|m| m.size)
                .sum(),
            files: members.iter().map(|m| m.file_id).collect(),
            snapshot,
            revision: states.iter().map(|v| v.map(|c| c.operation)).collect(),
            paths: members.iter().map(|m| m.path.clone()).collect(),
            untouched: inherited.is_none() && states.iter().all(|s| s.is_none()),
        });
    }
    Ok(result)
}

pub fn decide(db: &Db, groups: &[Group], state: &str) -> Result<i64> {
    ensure!(
        state != "plan" || groups.iter().all(|g| g.eligible),
        "Only verified exact-copy groups can be added to the plan"
    );
    let files = groups
        .iter()
        .flat_map(|g| g.files.iter().map(|id| (*id, g.snapshot.clone())))
        .collect::<Vec<_>>();
    db.save_review(&files, state)
}

pub fn reviewed_plan(db: &Db, policy: &Policy, scope: &Scope) -> Result<Plan> {
    let approved: HashSet<_> = groups(db, scope.family)?
        .into_iter()
        .filter(|g| g.state == "plan")
        .map(|g| g.id)
        .collect();
    let sources = Sources::load(db)?;
    let rows = db.plan_rows_scoped(
        scope.family,
        scope.folder.as_deref(),
        scope.keeper_folder.as_deref(),
    )?;
    let decided: HashSet<_> = rows
        .iter()
        .filter(|r| r.is_keeper && sources.keeper_source(r).is_some())
        .map(|r| r.family_id)
        .collect();
    let included = |family, file| {
        approved.contains(&family) || decided.contains(&family) || sources.rejected.contains(&file)
    };
    let mut plan = plan::compute_scoped(db, policy, scope)?;
    // A keeper choice only proposes proven copies. Different versions require
    // an explicit file rejection; all existing safety gates still run first.
    plan.candidates
        .retain(|c| included(c.family_id, c.file_id) && (c.role == Role::Copy || c.manual));
    plan.refusals.retain(|r| included(r.family_id, r.file_id));
    Ok(plan)
}
