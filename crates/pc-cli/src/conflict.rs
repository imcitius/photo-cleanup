//! The command line's side of an undo whose original place is taken
//! (el-14vx0): which choice to pass to pc-apply. Only the question and the
//! answer live here; what each choice does — and whether it is offered at
//! all — is pc-apply's, the same for the web.
//!
//! Every conflict is shown, and every answer collected, before the first
//! file moves (el-14vx0 B5): [`show`], then [`Chooser::review`], then
//! `pc_apply::undo_reviewed` / `undo_run_reviewed`, which hold each entry
//! to what was read for it here — a choice only while its conflict is still
//! the one shown, a free place only while the unit coming back is still
//! the one read.
//!
//! Two choices exist (user decision 2026-10-10): keep it in quarantine, or
//! return it as `*_1` beside the existing file. Nothing here can touch the
//! existing file.

use anyhow::Result;
use pc_apply::{Choice, Conflict, Reviewed, Seen};
use std::collections::HashMap;
use std::io::{BufRead, Write};

/// The preview: every conflict, what it offers and why not more. Nothing
/// has moved when this is printed.
pub fn show(out: &mut dyn Write, seen: &[Seen]) -> Result<()> {
    let conflicts: Vec<&Conflict> = seen.iter().filter_map(Seen::conflict).collect();
    if conflicts.is_empty() {
        return Ok(());
    }
    writeln!(
        out,
        "\n{} — the original place is taken; nothing has moved yet:",
        pc_core::count_en(conflicts.len() as i64, "conflict", "conflicts")
    )?;
    for (i, c) in conflicts.iter().enumerate() {
        writeln!(out, "  {}. {}", i + 1, c.describe())?;
        for l in &c.limits {
            writeln!(out, "     ({l})")?;
        }
        let offered: Vec<&str> = c.choices.iter().map(|ch| ch.as_str()).collect();
        writeln!(out, "     choices: {}", offered.join(", "))?;
    }
    Ok(())
}

/// Answers the conflicts of one command: the one given on the command line
/// first, then — on a terminal — a question per conflict; "apply to all"
/// answers every one after it. Without a terminal and without a given
/// answer, every file stays in quarantine.
pub struct Chooser<'a> {
    given: Option<Choice>,
    all: bool,
    remembered: Option<Choice>,
    ask: Option<(&'a mut dyn BufRead, &'a mut dyn Write)>,
}

impl<'a> Chooser<'a> {
    /// `given` is `--on-conflict`, `all` is `--all`, `ask` the terminal to
    /// put the question on, if there is one.
    pub fn new(
        given: Option<Choice>,
        all: bool,
        ask: Option<(&'a mut dyn BufRead, &'a mut dyn Write)>,
    ) -> Self {
        Chooser {
            given,
            all,
            remembered: None,
            ask,
        }
    }

    /// What every entry read here is bound to, by entry — with an answer
    /// for every conflict, all of them before anything moves.
    pub fn review(&mut self, seen: &[Seen]) -> Result<HashMap<i64, Reviewed>> {
        let mut out = HashMap::new();
        for s in seen {
            let choice = match s.conflict() {
                Some(c) => self.decide(c)?,
                None => Choice::Keep,
            };
            out.insert(
                s.journal_id(),
                Reviewed {
                    seen: s.clone(),
                    choice,
                },
            );
        }
        Ok(out)
    }

    pub fn decide(&mut self, c: &Conflict) -> Result<Choice> {
        let mut said = format!("\n{}\n", c.describe());
        for l in &c.limits {
            said.push_str(&format!("  ({l})\n"));
        }
        let settled = if let Some(r) = self.remembered {
            Some((r, " (for all)"))
        } else if let Some(g) = self.given.take() {
            if self.all {
                self.remembered = Some(g);
            }
            Some((g, ""))
        } else if self.ask.is_none() {
            Some((Choice::Keep, " (no terminal to ask; see --on-conflict)"))
        } else {
            None
        };
        let Some((input, output)) = &mut self.ask else {
            let (ch, why) = settled.unwrap_or((Choice::Keep, ""));
            println!("{said}  → {}{why}", ch.as_str());
            return Ok(ch);
        };
        write!(output, "{said}")?;
        if let Some((ch, why)) = settled {
            writeln!(output, "  → {}{why}", ch.as_str())?;
            return Ok(ch);
        }
        loop {
            for (i, ch) in c.choices.iter().enumerate() {
                writeln!(output, "  {}) {} — {}", i + 1, ch.as_str(), ch.words())?;
            }
            write!(
                output,
                "Choose 1-{} (add `a` to apply to all the remaining ones, e.g. `2a`) [1]: ",
                c.choices.len()
            )?;
            output.flush()?;
            let mut line = String::new();
            if input.read_line(&mut line)? == 0 {
                return Ok(Choice::Keep);
            }
            match answer(&line, &c.choices) {
                Some((ch, all)) => {
                    if all {
                        self.remembered = Some(ch);
                    }
                    return Ok(ch);
                }
                None => writeln!(output, "  not one of the choices")?,
            }
        }
    }
}

/// `"2"` → the second choice offered; `"2a"` → it, for all the remaining
/// conflicts too; empty → keep. A choice's word (`rename-returning`) works
/// as well.
pub fn answer(line: &str, offered: &[Choice]) -> Option<(Choice, bool)> {
    let t = line.trim().to_lowercase();
    if t.is_empty() {
        return Some((Choice::Keep, false));
    }
    let (t, all) = match t.strip_suffix('a').filter(|r| r.parse::<usize>().is_ok()) {
        Some(r) => (r.to_string(), true),
        None => match t.strip_suffix(" all") {
            Some(r) => (r.trim().to_string(), true),
            None => (t, false),
        },
    };
    let ch = match t.parse::<usize>() {
        Ok(n) if n >= 1 => offered.get(n - 1).copied(),
        Ok(_) => None,
        Err(_) => Choice::parse(&t).filter(|c| offered.contains(c)),
    }?;
    Some((ch, all))
}

#[cfg(test)]
mod tests {
    use super::*;

    const OFFERED: [Choice; 2] = Choice::ALL;

    fn conflict(choices: &[Choice]) -> Conflict {
        Conflict {
            journal_id: 1,
            returning: vec![pc_apply::Returning {
                home: "/a/IMG.CR2".into(),
                held: "/a/.q/IMG.CR2".into(),
                proof: None,
                seen: None,
            }],
            occupants: Vec::new(),
            beside: Vec::new(),
            choices: choices.to_vec(),
            limits: Vec::new(),
        }
    }

    #[test]
    fn an_empty_answer_keeps_the_file_in_quarantine() {
        assert_eq!(answer("\n", &OFFERED), Some((Choice::Keep, false)));
    }

    #[test]
    fn a_number_picks_from_what_is_offered_and_a_trailing_a_means_all() {
        assert_eq!(answer("1", &OFFERED), Some((Choice::Keep, false)));
        assert_eq!(
            answer("2", &OFFERED),
            Some((Choice::RenameReturning, false))
        );
        assert_eq!(
            answer("2a\n", &OFFERED),
            Some((Choice::RenameReturning, true))
        );
        assert_eq!(
            answer("rename-returning all", &OFFERED),
            Some((Choice::RenameReturning, true))
        );
    }

    #[test]
    fn the_removed_choices_are_no_answer_at_all() {
        // The user's decision (a): nothing that touches the existing file.
        for gone in ["replace", "rename-existing", "3", "4"] {
            assert_eq!(answer(gone, &OFFERED), None, "{gone}");
        }
        // A unit that may only be kept: nothing else is an answer.
        assert_eq!(answer("2", &[Choice::Keep]), None);
        assert_eq!(answer("rename-returning", &[Choice::Keep]), None);
    }

    #[test]
    fn the_terminal_answer_for_all_answers_every_later_conflict_without_asking() {
        let c = conflict(&OFFERED);
        let mut input: &[u8] = b"2a\n";
        let mut output = Vec::new();
        let mut ch = Chooser::new(None, false, Some((&mut input, &mut output)));
        assert_eq!(ch.decide(&c).unwrap(), Choice::RenameReturning);
        assert_eq!(
            ch.decide(&c).unwrap(),
            Choice::RenameReturning,
            "asked again"
        );
        let shown = String::from_utf8(output).unwrap();
        assert_eq!(shown.matches("Choose").count(), 1, "{shown}");
        assert!(shown.contains("Choose 1-2"), "{shown}");
        assert!(!shown.contains("replace"), "{shown}");
    }

    #[test]
    fn a_given_answer_without_all_answers_only_the_first_conflict() {
        let c = conflict(&OFFERED);
        let mut ch = Chooser::new(Some(Choice::RenameReturning), false, None);
        assert_eq!(ch.decide(&c).unwrap(), Choice::RenameReturning);
        assert_eq!(ch.decide(&c).unwrap(), Choice::Keep);
        let mut ch = Chooser::new(Some(Choice::RenameReturning), true, None);
        assert_eq!(ch.decide(&c).unwrap(), Choice::RenameReturning);
        assert_eq!(ch.decide(&c).unwrap(), Choice::RenameReturning);
    }
}
