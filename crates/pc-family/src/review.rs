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
}

pub fn groups(db: &Db, family: Option<i64>) -> Result<Vec<Group>> {
    let rows = db.plan_rows_scoped(family, None, None)?;
    let choices = db.review_choices()?;
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
            .unwrap_or_else(|| "pending".into());
        result.push(Group {
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
            untouched: states.iter().all(|s| s.is_none()),
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
    let mut plan = plan::compute_scoped(db, policy, scope)?;
    plan.candidates
        .retain(|c| approved.contains(&c.family_id) && c.role == Role::Copy && !c.manual);
    plan.refusals.retain(|r| approved.contains(&r.family_id));
    Ok(plan)
}
