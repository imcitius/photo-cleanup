//! The held-directory contract, on every platform (el-543ou): the same
//! assertions against the Unix descriptors and the Windows handles.

use super::*;
use std::fs;

fn held(tmp: &tempfile::TempDir) -> Dir {
    Dir::open(tmp.path()).unwrap()
}

#[test]
fn a_rename_never_replaces_and_keeps_both_entries() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("a.jpg"), b"photograph 4031").unwrap();
    fs::write(tmp.path().join("b.jpg"), b"stranger 7719").unwrap();
    let dir = held(&tmp);
    let e = dir.rename_no_replace("a.jpg", "b.jpg").unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists, "{e}");
    assert_eq!(
        fs::read(tmp.path().join("a.jpg")).unwrap(),
        b"photograph 4031"
    );
    assert_eq!(
        fs::read(tmp.path().join("b.jpg")).unwrap(),
        b"stranger 7719"
    );
    dir.rename_no_replace("a.jpg", "c.jpg").unwrap();
    assert_eq!(
        fs::read(tmp.path().join("c.jpg")).unwrap(),
        b"photograph 4031"
    );
    assert!(!tmp.path().join("a.jpg").exists());
}

#[test]
fn create_new_never_touches_an_existing_entry() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("note"), b"stranger 7719").unwrap();
    fs::create_dir(tmp.path().join("folder")).unwrap();
    let dir = held(&tmp);
    let e = dir.create_new("note", 0o600).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists, "{e}");
    // A folder at the name refuses too; Windows says "is a directory" as
    // access denied (STATUS_FILE_IS_A_DIRECTORY).
    let e = dir.create_new("folder", 0o600).unwrap_err();
    assert!(
        matches!(
            e.kind(),
            std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::PermissionDenied
        ),
        "{e}"
    );
    assert!(dir.entry_at("folder").unwrap().is_dir());
    assert_eq!(fs::read(tmp.path().join("note")).unwrap(), b"stranger 7719");
    use std::io::Write;
    let mut f = dir.create_new("fresh", 0o600).unwrap();
    f.write_all(b"ours").unwrap();
    drop(f);
    assert_eq!(fs::read(tmp.path().join("fresh")).unwrap(), b"ours");
}

#[test]
fn an_object_keeps_its_identity_across_a_rename_and_another_has_its_own() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("a.jpg"), b"frame").unwrap();
    fs::write(tmp.path().join("twin.jpg"), b"frame").unwrap();
    let dir = held(&tmp);
    let (before, _) = dir.stat_at("a.jpg").unwrap();
    dir.rename_no_replace("a.jpg", "moved.jpg").unwrap();
    assert_eq!(dir.stat_at("moved.jpg").unwrap().0, before);
    assert_ne!(dir.stat_at("twin.jpg").unwrap().0, before);
    let f = dir.open_file("moved.jpg", false).unwrap();
    assert_eq!(ident_of_file(&f).unwrap(), before);
    assert!(dir.stat_at("a.jpg").is_err());
}

#[test]
fn a_second_name_is_counted() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("a.jpg"), b"frame").unwrap();
    let dir = held(&tmp);
    assert_eq!(dir.entry_at("a.jpg").unwrap().nlink, 1);
    fs::hard_link(tmp.path().join("a.jpg"), tmp.path().join("b.jpg")).unwrap();
    let (a, b) = (
        dir.entry_at("a.jpg").unwrap(),
        dir.entry_at("b.jpg").unwrap(),
    );
    assert_eq!((a.nlink, b.nlink), (2, 2));
    assert_eq!(a.ident, b.ident);
    assert!(a.is_file() && !a.is_dir());
}

#[test]
fn only_one_entry_name_is_accepted() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join("sub")).unwrap();
    fs::write(tmp.path().join("sub").join("x.jpg"), b"frame").unwrap();
    let dir = held(&tmp);
    for name in ["", ".", "..", "sub/x.jpg"] {
        assert!(dir.stat_at(name).is_err(), "{name:?}");
        assert!(dir.create_new(name, 0o600).is_err(), "{name:?}");
    }
    #[cfg(windows)]
    for name in [r"sub\x.jpg", r"..\x.jpg"] {
        let e = dir.stat_at(name).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput, "{name:?}: {e}");
    }
}

#[test]
fn folders_are_made_and_listed_through_the_handle() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = held(&tmp);
    let sub = dir.mkdir("Квартал").unwrap();
    assert!(dir.entry_at("Квартал").unwrap().is_dir());
    drop(sub.create_new("кадр.jpg", 0o600).unwrap());
    let again = dir.open_dir("Квартал").unwrap();
    assert_eq!(again.ident().unwrap(), sub.ident().unwrap());
    let mut names = again.names().unwrap();
    names.sort();
    assert_eq!(names, vec![std::ffi::OsString::from("кадр.jpg")]);
    assert!(
        dir.mkdir("Квартал").is_err(),
        "an existing folder is not made again"
    );
}

#[test]
fn a_held_folder_knows_where_it_is_and_where_it_is_not() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join("a")).unwrap();
    fs::create_dir(tmp.path().join("b")).unwrap();
    let a = Dir::open(&tmp.path().join("a")).unwrap();
    assert!(a.is_at(&tmp.path().join("a")));
    assert!(!a.is_at(&tmp.path().join("b")));
    assert!(!a.is_at(&tmp.path().join("missing")));
    let now = a.current_path().expect("the system names the folder");
    assert!(a.is_at(&now), "{}", now.display());
}

#[test]
fn locate_proves_the_place_and_follows_a_held_file() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join("home")).unwrap();
    fs::create_dir(tmp.path().join("away")).unwrap();
    fs::write(tmp.path().join("home").join("a.jpg"), b"frame").unwrap();
    let home = Dir::open(&tmp.path().join("home")).unwrap();
    let (obj, _) = home.stat_at("a.jpg").unwrap();
    let recorded = tmp.path().join("home").join("a.jpg");
    match locate(&home, "a.jpg", obj, Some(&recorded), None) {
        Whereabouts::Verified { at: path } => assert_eq!(path, recorded),
        other => panic!("{other:?}"),
    }
    // Moved away by name: the held file says where it went.
    let f = home.open_file("a.jpg", false).unwrap();
    fs::rename(&recorded, tmp.path().join("away").join("b.jpg")).unwrap();
    match locate(&home, "a.jpg", obj, Some(&recorded), Some(&f)) {
        Whereabouts::Verified { at: path } => {
            assert_eq!(path.file_name().unwrap(), "b.jpg");
            assert!(
                path.parent().unwrap().ends_with("away"),
                "{}",
                path.display()
            );
        }
        other => panic!("{other:?}"),
    }
    // Without the file nothing is guessed.
    assert!(matches!(
        locate(&home, "a.jpg", obj, Some(&recorded), None),
        Whereabouts::Uncertain { .. }
    ));
}
