//! A directory held open, and entries reached through it — never by a path
//! looked up again (el-usdqi, el-5vue3 R1/D1–D3).
//!
//! One contract, two implementations: [`unix`] (descriptors and the `*at`
//! calls) and [`windows`] (handles and `NtCreateFile` with a root directory,
//! el-543ou). What decides where an object is ([`locate`]) is written once,
//! here, against both.
//!
//! Nothing is ever removed through this module on Windows: the removal
//! methods exist only in the Unix implementation (for `purge`), so a removal
//! on Windows does not compile.

use std::fs::File;
use std::path::{Path, PathBuf};

pub use crate::whereabouts::Whereabouts;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as imp;
#[cfg(unix)]
pub use unix::{ident_of, ident_of_file, Dir, Entry};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;
#[cfg(windows)]
pub use windows::{ident_of_file, rename_into, Dir, Entry};

#[cfg(test)]
mod tests;
#[cfg(all(test, windows))]
mod windows_tests;

/// What identifies an object: `(device, inode)` as `fstat`/`fstatat` report
/// it on Unix; `(volume serial number, NTFS file ID)` from `FileIdInfo` on
/// Windows.
pub type Ident = (u64, u64);

pub fn current_path_of(file: &File) -> Option<PathBuf> {
    imp::current_path(file)
}

/// Where the object `obj` is, proven (el-lvtmk §3.1).
///
/// `dir` is the folder held for it and `name` the name it was given there;
/// `recorded` is the full path the caller would state. `Verified` needs all
/// of: the name in the held folder bears `obj`; a path to that folder — the
/// recorded one first, else the one the system names from the descriptor —
/// leads to the held folder now; and that path joined with the name bears
/// `obj`. If the object is not in the folder, the open file `file` (when
/// there is one) says whether it still has a name at all (`Unlinked`) and
/// where the system thinks it is, which is proven the same way.
///
/// Nothing is searched. Anything short of proof is `Uncertain`, with the
/// last place seen, unverified.
pub fn locate(
    dir: &Dir,
    name: &str,
    obj: Ident,
    recorded: Option<&Path>,
    file: Option<&File>,
) -> Whereabouts {
    let bears = |p: &Path| imp::ident_at_path(p) == Some(obj);
    if matches!(dir.stat_at(name), Ok((id, _)) if id == obj) {
        let mut tried = Vec::new();
        if let Some(r) = recorded {
            if r.file_name().and_then(|n| n.to_str()) == Some(name) {
                tried.push(r.to_path_buf());
            }
        }
        tried.push(dir.join(name));
        let now = dir.current_path().map(|p| p.join(name));
        if let Some(p) = &now {
            tried.push(p.clone());
        }
        for p in &tried {
            let folder = p.parent().unwrap_or(Path::new("."));
            if dir.is_at(folder) && bears(p) {
                return Whereabouts::verified(p.clone());
            }
        }
        return Whereabouts::uncertain(
            now.or_else(|| Some(dir.join(name))),
            crate::tr!(
                "папку, где он лежит, нельзя назвать путём, который туда ведёт",
                "the folder it is in cannot be named by a path that leads there"
            ),
        );
    }
    let Some(f) = file else {
        return Whereabouts::uncertain(
            None,
            crate::tf!(
                "под именем {0} в его папке его больше нет",
                "it no longer bears the name {0} in its folder",
                name
            ),
        );
    };
    if imp::unlinked(f, obj) {
        return Whereabouts::Unlinked;
    }
    match imp::current_path(f) {
        Some(p) if bears(&p) => Whereabouts::verified(p),
        Some(p) => Whereabouts::uncertain(
            Some(p),
            crate::tr!(
                "система называет это место, но под этим именем уже не он",
                "the system names this place, but the name no longer bears it"
            ),
        ),
        None => Whereabouts::uncertain(
            None,
            crate::tr!(
                "его нет в его папке, а система не говорит, где он",
                "it is not in its folder, and the system does not say where it is"
            ),
        ),
    }
}

/// Tests only: what another program does at a boundary of this module.
#[cfg(test)]
pub(crate) mod seam {
    use std::cell::RefCell;
    use std::path::Path;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Stage {
        /// Before a new file is created at the path.
        Create,
        /// After the file is written, before it is synced.
        Written,
        /// Before the temporary at the path is published.
        Publish,
    }

    type Hook = Box<dyn FnMut(Stage, &Path) -> std::io::Result<()>>;

    thread_local! {
        static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }

    pub(crate) struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            HOOK.with(|h| *h.borrow_mut() = None);
        }
    }

    pub(crate) fn set(hook: impl FnMut(Stage, &Path) -> std::io::Result<()> + 'static) -> Guard {
        HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
        Guard
    }

    /// Run the hook; its error is returned to the caller as the call's.
    pub(crate) fn fire_result(stage: Stage, path: &Path) -> std::io::Result<()> {
        HOOK.with(|h| match h.borrow_mut().as_mut() {
            Some(hook) => hook(stage, path),
            None => Ok(()),
        })
    }
}
