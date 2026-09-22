//! Durable review choices belong to files, never to rebuildable group ids.
use crate::Db;
use anyhow::{ensure, Context, Result};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Choice {
    pub state: String,
    pub snapshot: String,
    pub operation: i64,
}
impl Db {
    pub fn review_choices(&self) -> Result<HashMap<i64, Choice>> {
        let mut stmt = self
            .conn
            .prepare("SELECT file_id,state,snapshot,operation FROM review_choices")?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    Choice {
                        state: r.get(1)?,
                        snapshot: r.get(2)?,
                        operation: r.get(3)?,
                    },
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn save_review(&self, files: &[(i64, String)], state: &str) -> Result<i64> {
        ensure!(
            matches!(state, "plan" | "keep" | "defer"),
            "Unknown review choice"
        );
        ensure!(!files.is_empty(), "No groups to review");
        let tx = self.conn.unchecked_transaction()?;
        let choices = self.review_choices()?;
        let before: Vec<_> = files
            .iter()
            .map(|(id, _)| (*id, choices.get(id).cloned()))
            .collect();
        self.conn.execute(
            "INSERT INTO review_history(before_json) VALUES(?1)",
            [serde_json::to_string(&before)?],
        )?;
        let operation = self.conn.last_insert_rowid();
        for (id, snapshot) in files {
            self.conn.execute("INSERT INTO review_choices(file_id,state,snapshot,operation) VALUES(?1,?2,?3,?4) ON CONFLICT(file_id) DO UPDATE SET state=excluded.state,snapshot=excluded.snapshot,operation=excluded.operation", params![id,state,snapshot,operation])?;
        }
        tx.commit()?;
        Ok(operation)
    }

    pub fn undo_review(&self, operation: i64) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let before: String = self
            .conn
            .query_row(
                "SELECT before_json FROM review_history WHERE id=?1 AND undone=0",
                [operation],
                |r| r.get(0),
            )
            .optional()?
            .context("This decision cannot be undone")?;
        let before: Vec<(i64, Option<Choice>)> = serde_json::from_str(&before)?;
        // An old browser must not overwrite a later choice, or undo a move
        // by pretending the files are still at home.
        for (id, _) in &before {
            let valid: bool = self.conn.query_row("SELECT EXISTS(SELECT 1 FROM review_choices c JOIN files f ON f.id=c.file_id WHERE c.file_id=?1 AND c.operation=?2 AND f.state='present')", params![id,operation], |r| r.get(0))?;
            ensure!(
                valid,
                "The files or their decisions changed; refresh the queue"
            );
        }
        for (id, previous) in before {
            if let Some(c) = previous {
                self.conn.execute(
                    "UPDATE review_choices SET state=?2,snapshot=?3,operation=?4 WHERE file_id=?1",
                    params![id, c.state, c.snapshot, c.operation],
                )?;
            } else {
                self.conn
                    .execute("DELETE FROM review_choices WHERE file_id=?1", [id])?;
            }
        }
        self.conn.execute(
            "UPDATE review_history SET undone=1 WHERE id=?1",
            [operation],
        )?;
        tx.commit()?;
        Ok(())
    }
}
