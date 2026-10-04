//! What a run actually did, counted once, and how a stop carries it.
//!
//! A run that stops halfway has to tell the truth about the half that
//! happened: those moves are done, journaled and undoable. It used to say it
//! in words early — a rendered `"1 file, 702 B"` inside the error — and the
//! web, which runs one action at a time, overwrote that with its own count of
//! *planned* actions, which knew nothing of a frame that moved before its
//! litter sweep stopped (el-5vue3 R4). Now every operation of this crate
//! reports a [`Tally`] of what actually moved, in units that say what they
//! are; a stop carries it typed, each level adds its own earlier work exactly
//! once ([`stop_run`]), and the command line and the web render the same
//! value with the same words.

use pc_core::fmt_bytes;
use std::path::PathBuf;

/// Work actually done. Planned quantities never enter it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tally {
    /// Derived bundles (a preview cache, a folder of them) moved whole.
    pub bundles: u64,
    /// The files inside those bundles.
    pub bundle_files: u64,
    /// Photographs moved: into quarantine, or to their place in a new tree.
    pub frames: u64,
    /// Sidecars that travelled with them.
    pub companions: u64,
    /// Service files carried out of folders a reorganisation emptied.
    pub litter: u64,
    /// Bytes of everything moved above.
    pub bytes: u64,
    /// Journal entries walked back completely.
    pub entries_back: u64,
    /// Journal entries of which only part came back: still open, retryable.
    pub entries_partial: u64,
    /// Files brought back, in complete and partial entries alike.
    pub files_back: u64,
}

impl Tally {
    pub fn add(&mut self, o: &Tally) {
        self.bundles += o.bundles;
        self.bundle_files += o.bundle_files;
        self.frames += o.frames;
        self.companions += o.companions;
        self.litter += o.litter;
        self.bytes += o.bytes;
        self.entries_back += o.entries_back;
        self.entries_partial += o.entries_partial;
        self.files_back += o.files_back;
    }

    pub fn is_empty(&self) -> bool {
        *self == Tally::default()
    }

    /// One derived bundle, moved whole.
    pub fn bundle(b: &pc_db::Bundle) -> Tally {
        Tally {
            bundles: 1,
            bundle_files: b.file_count.max(0) as u64,
            bytes: b.size.max(0) as u64,
            ..Default::default()
        }
    }

    /// Every file moved forward, whatever its kind.
    pub fn files_moved(&self) -> u64 {
        self.bundle_files + self.frames + self.companions + self.litter
    }

    /// In words, for the command line and the web alike. Only what
    /// happened is named; nothing at all is said as such.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.bundles > 0 {
            parts.push(format!(
                "{} ({})",
                pc_core::count(
                    self.bundles as i64,
                    ["объект", "объекта", "объектов"],
                    ["object", "objects"]
                ),
                pc_core::count(
                    self.bundle_files as i64,
                    ["файл", "файла", "файлов"],
                    ["file", "files"]
                )
            ));
        }
        if self.frames > 0 {
            parts.push(pc_core::count(
                self.frames as i64,
                ["файл", "файла", "файлов"],
                ["file", "files"],
            ));
        }
        if self.companions > 0 {
            parts.push(pc_core::count(
                self.companions as i64,
                ["спутник", "спутника", "спутников"],
                ["companion", "companions"],
            ));
        }
        if self.litter > 0 {
            parts.push(pc_core::count(
                self.litter as i64,
                ["служебный файл", "служебных файла", "служебных файлов"],
                ["service file", "service files"],
            ));
        }
        if self.files_moved() > 0 {
            parts.push(fmt_bytes(self.bytes));
        }
        if self.entries_back > 0 {
            parts.push(pc_core::count(
                self.entries_back as i64,
                [
                    "запись отменена целиком",
                    "записи отменены целиком",
                    "записей отменено целиком",
                ],
                ["entry walked back", "entries walked back"],
            ));
        }
        if self.entries_partial > 0 {
            parts.push(pc_core::count(
                self.entries_partial as i64,
                [
                    "запись отменена частично",
                    "записи отменены частично",
                    "записей отменено частично",
                ],
                ["entry walked back in part", "entries walked back in part"],
            ));
        }
        if self.files_back > 0 {
            parts.push(pc_core::count(
                self.files_back as i64,
                ["файл вернулся", "файла вернулись", "файлов вернулось"],
                ["file back", "files back"],
            ));
        }
        if parts.is_empty() {
            return pc_core::tr!("ничего", "nothing").into();
        }
        parts.join(", ")
    }
}

/// How the moves a stopped run made are walked back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Into quarantine: each entry comes back on its own.
    Quarantine,
    /// A reorganisation: the run comes back as a whole.
    Organize { run_id: i64 },
    /// An undo or a recovery itself: what came back stays back, the rest
    /// waits, and asking again carries on.
    Restore,
}

/// What a run had done when it stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stopped {
    pub done: Tally,
    pub route: Route,
    /// `path — why` for every file the run refused before it stopped.
    pub refused: Vec<String>,
    /// Journal entries the database could not complete after the disk was
    /// touched (el-1y8uo B3): they stay `pending`, and what they moved is
    /// recovered by reconciling them, not by an ordinary undo.
    pub pending: Vec<i64>,
}

impl Stopped {
    fn tail(&self) -> String {
        let moved = self.done.summary();
        if !self.pending.is_empty() {
            let ids = self
                .pending
                .iter()
                .map(|id| format!("#{id}"))
                .collect::<Vec<_>>()
                .join(", ");
            return pc_core::tf!(
                "Остановлено. До остановки: {0}. Записи журнала {1} не удалось завершить: \
                 они остаются незавершёнными, и то, что по ним перенесено, возвращается сверкой \
                 (страница «Журнал» в веб-интерфейсе), а не обычным откатом.",
                "Stopped. Before the stop: {0}. Journal entries {1} could not be completed: \
                 they stay pending, and what they moved is recovered by reconciling them (the \
                 Journal page in the web interface), not by an ordinary undo.",
                moved,
                ids
            );
        }
        match self.route {
            Route::Restore => pc_core::tf!(
                "Откат остановлен. До остановки: {0}; остальное там, где было, и повторный откат \
                 продолжит с этого места.",
                "The undo stopped. Before the stop: {0}; the rest is where it was, and asking again \
                 carries on from here.",
                moved
            ),
            route => {
                let how = match route {
                    Route::Organize { run_id } => pc_core::tf!(
                        "`photo-cleanup organize undo --run {0} --yes` или журнал в веб-интерфейсе",
                        "`photo-cleanup organize undo --run {0} --yes`, or the journal in the web interface",
                        run_id
                    ),
                    _ => pc_core::tr!(
                        "страница «Карантин» или `photo-cleanup derived undo --journal <id>`",
                        "the Quarantine page, or `photo-cleanup derived undo --journal <id>`"
                    )
                    .to_string(),
                };
                pc_core::tf!(
                    "Прогон остановлен. До остановки перенесено: {0} — эти переносы выполнены, \
                     записаны в журнал и отменяемы: {1}.",
                    "The run stopped. Before the stop it moved {0} — those moves are done, in the \
                     journal, and can be undone: {1}.",
                    moved,
                    how
                )
            }
        }
    }
}

/// The volume a move would happen on cannot rename without replacing.
///
/// Not a fact about one file: every move onto that volume would meet it, so
/// a run stops at it rather than collecting one refusal per photograph —
/// wherever it is met, a sidecar or a litter sweep included. Where the
/// volume can be asked (macOS) it is met before anything moves; where it
/// cannot (Linux), the first refused call itself moved nothing, but earlier
/// moves of the same run may have gone through. Those stay done and
/// journaled, and [`Stopped`] says how much.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoExclusiveRename {
    /// The destination that could not be reached safely.
    pub path: PathBuf,
    pub reason: String,
    /// What the run that stopped here had already done.
    pub run: Option<Stopped>,
}

impl NoExclusiveRename {
    pub fn new(path: PathBuf, reason: String) -> Self {
        Self {
            path,
            reason,
            run: None,
        }
    }
}

impl std::fmt::Display for NoExclusiveRename {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            pc_core::tf!(
                "перенос в {0} отменён: том не умеет переименовывать без замены существующего ({1}), \
                 а обычный перенос мог бы молча заменить файл, появившийся там в последний момент. \
                 Этот файл не перенесён.",
                "the move to {0} is refused: the volume cannot rename without replacing what is \
                 there ({1}), and a plain move could silently replace a file that appeared there \
                 at the last moment. This file was not moved.",
                self.path.display(),
                self.reason
            )
        )?;
        match &self.run {
            None => write!(
                f,
                " {}",
                pc_core::tr!(
                    "Переносы, сделанные раньше в этой же операции, если они были, выполнены, \
                     записаны в журнал и отменяемы.",
                    "Moves made earlier in the same operation, if any, are done, in the journal, \
                     and can be undone."
                )
            ),
            Some(run) => write!(f, " {}", run.tail()),
        }
    }
}

impl std::error::Error for NoExclusiveRename {}

/// Any other error that stopped a run after it had done something: the
/// cause, and what was done before it. Without this, a database error after
/// three moves read exactly like one before the first.
#[derive(Debug)]
pub struct Halted {
    pub cause: anyhow::Error,
    pub run: Stopped,
}

impl std::fmt::Display for Halted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}. {}", self.cause, self.run.tail())
    }
}

// No `source()`: the cause is already in the words above, and a chain
// would print it twice.
impl std::error::Error for Halted {}

/// Whether `e` is [`NoExclusiveRename`]: a run stops at it.
pub fn is_no_exclusive_rename(e: &anyhow::Error) -> bool {
    e.downcast_ref::<NoExclusiveRename>().is_some()
}

/// What a run that stopped at `e` had done, if `e` says.
pub fn stopped_run(e: &anyhow::Error) -> Option<&Stopped> {
    if let Some(n) = e.downcast_ref::<NoExclusiveRename>() {
        return n.run.as_ref();
    }
    e.downcast_ref::<Halted>().map(|h| &h.run)
}

/// `e` stopped work at some level; `earlier` is what *this* level had done
/// before it — never what a deeper level already put into `e`. Each level
/// adds its own share exactly once, so a nested stop is summed, not
/// overwritten. A plain error after no work at all passes unchanged.
pub fn stop_run(
    mut e: anyhow::Error,
    earlier: &Tally,
    route: Route,
    refused: Vec<String>,
) -> anyhow::Error {
    fn merge(run: &mut Option<Stopped>, earlier: &Tally, route: Route, refused: Vec<String>) {
        match run {
            Some(inner) => {
                inner.done.add(earlier);
                let mut all = refused;
                all.append(&mut inner.refused);
                inner.refused = all;
            }
            None => {
                *run = Some(Stopped {
                    done: earlier.clone(),
                    route,
                    refused,
                    pending: Vec::new(),
                })
            }
        }
    }
    if let Some(n) = e.downcast_mut::<NoExclusiveRename>() {
        merge(&mut n.run, earlier, route, refused);
        return e;
    }
    if let Some(h) = e.downcast_mut::<Halted>() {
        let mut run = Some(h.run.clone());
        merge(&mut run, earlier, route, refused);
        h.run = run.expect("merged");
        return e;
    }
    if earlier.is_empty() && refused.is_empty() {
        return e;
    }
    Halted {
        cause: e,
        run: Stopped {
            done: earlier.clone(),
            route,
            refused,
            pending: Vec::new(),
        },
    }
    .into()
}

/// The journal entry `id` could not be completed after the disk was touched:
/// the stop `e` names it as pending. An error that carried no stop yet
/// becomes one, with nothing done, so the pending entry is still said.
pub(crate) fn left_pending(mut e: anyhow::Error, id: i64, route: Route) -> anyhow::Error {
    let run = if let Some(n) = e.downcast_mut::<NoExclusiveRename>() {
        n.run.get_or_insert_with(|| Stopped {
            done: Tally::default(),
            route,
            refused: Vec::new(),
            pending: Vec::new(),
        })
    } else if let Some(h) = e.downcast_mut::<Halted>() {
        &mut h.run
    } else {
        return Halted {
            cause: e,
            run: Stopped {
                done: Tally::default(),
                route,
                refused: Vec::new(),
                pending: vec![id],
            },
        }
        .into();
    };
    if !run.pending.contains(&id) {
        run.pending.push(id);
    }
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nested_stop_is_summed_not_overwritten() {
        // The inner action moved a frame and a service file; the outer run
        // had moved two frames before it. Neither number may replace the
        // other (el-5vue3 R4: the web reported 0 files after 1 had moved).
        let inner = Tally {
            frames: 1,
            litter: 1,
            bytes: 702 + 6,
            ..Default::default()
        };
        let e: anyhow::Error = NoExclusiveRename::new("/q/x".into(), "unsupported".into()).into();
        let e = stop_run(e, &inner, Route::Organize { run_id: 7 }, vec![]);
        let before = Tally {
            frames: 2,
            bytes: 100,
            ..Default::default()
        };
        let e = stop_run(
            e,
            &before,
            Route::Organize { run_id: 7 },
            vec!["a — b".into()],
        );
        let run = stopped_run(&e).unwrap();
        assert_eq!(run.done.frames, 3);
        assert_eq!(run.done.litter, 1);
        assert_eq!(run.done.bytes, 808);
        assert_eq!(run.refused, vec!["a — b".to_string()]);

        // A plain error after work becomes a typed stop too.
        let e = stop_run(
            anyhow::anyhow!("disk full"),
            &before,
            Route::Quarantine,
            vec![],
        );
        assert_eq!(stopped_run(&e).unwrap().done.frames, 2);
        assert!(format!("{e:#}").contains("disk full"));
    }
}
