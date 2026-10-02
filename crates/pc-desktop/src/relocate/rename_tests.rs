//! A target whose volume cannot rename without replacing (el-21zyg).
//!
//! On macOS exFAT, `renameatx_np(RENAME_EXCL)` fails with `ENOTSUP` (45);
//! a run that met it there used to leave `photo-cleanup.db.partial` and the
//! `-wal`/`-shm`/`-journal` reservations behind, because the cleanup moves
//! its own entries aside with the very same call — and a retry was then
//! refused because of them. exFAT itself is refused before anything is
//! written (it ignores owners, el-2xri), so the call is made to fail here
//! the way exFAT fails it ([`at::fault`]) on a volume that passes the
//! admission. Nothing may fall back to a rename that could replace an
//! entry.

use super::caller_tests::{aside_dirs, marker, plenty, Env};
use super::publication_tests::own_target;
use super::*;

/// Every name in `dir`, sorted; `None` if `dir` does not exist.
fn listing(dir: &Path) -> Option<Vec<String>> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .ok()?
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    Some(names)
}

/// The names a run reserves or stages in `target`: none may be left.
fn staging_names(target: &Path) -> Vec<PathBuf> {
    let db = target.join(DB_FILE);
    let mut all = vec![db.clone(), target.join(THUMBS_DIR)];
    all.extend(SIDECARS.map(|s| sidecar(&db, s)));
    all.extend(legacy_partials(target));
    all.into_iter().filter(|p| occupied(p)).collect()
}

/// Every exclusive rename on this thread fails with `ENOTSUP`, as on
/// exFAT. For a folder that exists (with someone's file in it) and for one
/// still to be created: the move is refused with an error naming the
/// target and why, nothing of the run is left in the target — no staging
/// folder, no reservation, no private cleanup folder, no folder it made —
/// and a second attempt meets exactly the first one's state and answer.
/// The source, its data and the bootstrap stay as they were.
#[test]
fn a_volume_that_cannot_rename_without_replacing_is_refused_without_leftovers() {
    let env = Env::new();
    let existing = own_target(&env);
    fs::write(existing.join("foreign-photo.jpg"), b"\xff\xd8 foreign").unwrap();
    let missing = env.root.join("new/data");
    let bootstrap = env.bootstrap();
    let _unsupported = at::fault::refuse(|_, _| true);
    for target in [&existing, &missing] {
        let before = listing(target);
        let mut answers = Vec::new();
        for attempt in 0..2 {
            let error = move_data(&env.dirs, &env.layout, Source::System, target, plenty)
                .expect_err("refused");
            let text = error.to_string();
            assert!(
                text.contains(&target.display().to_string()),
                "{attempt}: {text}"
            );
            assert!(text.contains("without replacing"), "{attempt}: {text}");
            assert!(
                matches!(&error, RelocateError::Blocked { blockers }
                    if matches!(blockers.as_slice(), [Blocker::NoExclusiveRename { path, .. }]
                        if path == target)),
                "{attempt}: {error:?}"
            );
            assert_eq!(staging_names(target), Vec::<PathBuf>::new(), "{text}");
            assert_eq!(listing(target), before, "{attempt}: {text}");
            if target.exists() {
                assert!(aside_dirs(target).is_empty(), "{text}");
            }
            answers.push(error);
        }
        assert_eq!(answers[0], answers[1]);
    }
    assert!(!missing.parent().unwrap().exists());
    assert_eq!(
        fs::read(existing.join("foreign-photo.jpg")).unwrap(),
        b"\xff\xd8 foreign"
    );
    assert_eq!(env.bootstrap(), bootstrap);
    assert_eq!(env.chosen(), env.layout.dir);
    assert_eq!(marker(&env.layout.db), "исходная");
}

/// `ENOTSUP` from the publication itself (the volume changed, or the call
/// failed for once) while the cleanup's own renames still work: the run is
/// undone completely — nothing it staged or reserved is left — the error
/// names the target, and a retry once the call works again behaves exactly
/// like a first attempt and completes the move.
#[test]
fn an_unsupported_rename_at_publication_is_undone_and_a_retry_completes() {
    let env = Env::new();
    let target = own_target(&env);
    let bootstrap = env.bootstrap();
    for published in [THUMBS_DIR, DB_FILE] {
        let name = std::ffi::CString::new(published).unwrap();
        let unsupported = at::fault::refuse(move |from, _| from == name.as_c_str());
        let error = move_data(&env.dirs, &env.layout, Source::System, &target, plenty)
            .expect_err("refused");
        drop(unsupported);
        let text = error.to_string();
        assert!(text.contains(&target.display().to_string()), "{text}");
        assert!(
            text.contains(&io::Error::from_raw_os_error(libc::ENOTSUP).to_string()),
            "{text}"
        );
        assert!(!matches!(error, RelocateError::Cleanup { .. }), "{text}");
        assert_eq!(staging_names(&target), Vec::<PathBuf>::new(), "{text}");
        assert!(aside_dirs(&target).is_empty(), "{text}");
        assert_eq!(env.bootstrap(), bootstrap);
        assert_eq!(env.chosen(), env.layout.dir);
    }
    let preview = preview_move_with(&env.dirs, &env.layout, Source::System, &target, plenty);
    assert!(preview.blockers.is_empty(), "{:?}", preview.reasons);
    move_data(&env.dirs, &env.layout, Source::System, &target, plenty).unwrap();
    assert_eq!(env.chosen(), target);
    assert_eq!(marker(&target.join(DB_FILE)), "исходная");
    assert!(!target.join(PARTIAL_DB).exists());
    assert!(aside_dirs(&target).is_empty());
}

/// The trial rename works, every later one fails (a volume that changed
/// under the run). The cleanup has no rename left to move its own entries
/// aside with, and nothing replaces it: it does not fall back to deleting
/// or renaming by name. What it made is kept — the staging folder with the
/// copy inside, the empty reservations — and each is named in the error;
/// the next attempt is refused because of them, naming each path, and
/// changes nothing. The source and the bootstrap are untouched.
#[test]
fn leftovers_that_cannot_be_moved_aside_are_kept_and_named_never_removed_by_name() {
    let env = Env::new();
    let target = own_target(&env);
    let bootstrap = env.bootstrap();
    let unsupported = at::fault::refuse(|from, _| !from.to_bytes().starts_with(b"rename-probe"));
    let error =
        move_data(&env.dirs, &env.layout, Source::System, &target, plenty).expect_err("refused");
    drop(unsupported);
    let RelocateError::Cleanup { left, .. } = &error else {
        panic!("{error:?}");
    };
    let db = target.join(DB_FILE);
    let kept: Vec<PathBuf> = std::iter::once(target.join(PARTIAL_DB))
        .chain(SIDECARS.map(|s| sidecar(&db, s)))
        .collect();
    for path in &kept {
        assert!(occupied(path), "{}", path.display());
        let shown = path.display().to_string();
        assert!(
            left.iter()
                .any(|l| l.contains(&format!("{shown} was left in place"))),
            "{shown}: {left:?}"
        );
    }
    // The staging folder still holds the copy: nothing was deleted.
    assert!(target.join(PARTIAL_DB).join(DB_FILE).is_file());
    assert!(aside_dirs(&target).is_empty());
    let before = listing(&target);
    let retry = move_data(&env.dirs, &env.layout, Source::System, &target, plenty)
        .expect_err("blocked by the leftovers");
    let RelocateError::Blocked { blockers } = &retry else {
        panic!("{retry:?}");
    };
    for path in &kept {
        assert!(
            blockers.iter().any(|b| matches!(b,
                Blocker::SidecarExists { path: p } | Blocker::LeftoverPartial { path: p }
                    if p == path)),
            "{}: {blockers:?}",
            path.display()
        );
    }
    assert_eq!(listing(&target), before);
    assert_eq!(env.bootstrap(), bootstrap);
    assert_eq!(env.chosen(), env.layout.dir);
    assert_eq!(marker(&env.layout.db), "исходная");
}
