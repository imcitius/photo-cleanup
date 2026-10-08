//! The command line's side of an undo whose original place is taken
//! (el-14vx0): which choice to pass to `pc_apply::undo_with`. Only the
//! question and the answer live here; what each choice does — and whether
//! it is offered at all — is pc-apply's, the same for the web.

use anyhow::Result;
use pc_apply::{Choice, Conflict};
use std::io::{BufRead, Write};

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
            print!("{said}  → {}{why}\n", ch.as_str());
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
/// conflicts too; empty → keep. A choice's word (`replace`) works as well.
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

    const OFFERED: [Choice; 4] = Choice::ALL;

    #[test]
    fn an_empty_answer_keeps_the_file_in_quarantine() {
        assert_eq!(answer("\n", &OFFERED), Some((Choice::Keep, false)));
    }

    #[test]
    fn a_number_picks_from_what_is_offered_and_a_trailing_a_means_all() {
        assert_eq!(answer("2", &OFFERED), Some((Choice::Replace, false)));
        assert_eq!(
            answer("4a\n", &OFFERED),
            Some((Choice::RenameReturning, true))
        );
        assert_eq!(
            answer("rename-existing all", &OFFERED),
            Some((Choice::RenameExisting, true))
        );
    }

    #[test]
    fn a_choice_that_is_not_offered_is_not_an_answer() {
        // Lightroom: only keep and return as *_1.
        let lr = [Choice::Keep, Choice::RenameReturning];
        assert_eq!(answer("replace", &lr), None);
        assert_eq!(answer("3", &lr), None);
        assert_eq!(answer("2", &lr), Some((Choice::RenameReturning, false)));
    }

    #[test]
    fn the_terminal_answer_for_all_answers_every_later_conflict_without_asking() {
        let c = Conflict {
            journal_id: 1,
            returning: vec![pc_apply::Returning {
                home: "/a/IMG.CR2".into(),
                held: "/a/.q/IMG.CR2".into(),
                proof: None,
            }],
            occupants: Vec::new(),
            choices: OFFERED.to_vec(),
            limits: Vec::new(),
        };
        let mut input: &[u8] = b"3a\n";
        let mut output = Vec::new();
        let mut ch = Chooser::new(None, false, Some((&mut input, &mut output)));
        assert_eq!(ch.decide(&c).unwrap(), Choice::RenameExisting);
        assert_eq!(
            ch.decide(&c).unwrap(),
            Choice::RenameExisting,
            "asked again"
        );
        let shown = String::from_utf8(output).unwrap();
        assert_eq!(shown.matches("Choose").count(), 1, "{shown}");
    }

    #[test]
    fn a_given_answer_without_all_answers_only_the_first_conflict() {
        let c = Conflict {
            journal_id: 1,
            returning: Vec::new(),
            occupants: Vec::new(),
            choices: OFFERED.to_vec(),
            limits: Vec::new(),
        };
        let mut ch = Chooser::new(Some(Choice::Replace), false, None);
        assert_eq!(ch.decide(&c).unwrap(), Choice::Replace);
        assert_eq!(ch.decide(&c).unwrap(), Choice::Keep);
        let mut ch = Chooser::new(Some(Choice::Replace), true, None);
        assert_eq!(ch.decide(&c).unwrap(), Choice::Replace);
        assert_eq!(ch.decide(&c).unwrap(), Choice::Replace);
    }
}
