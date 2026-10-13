//! Native Windows tests of the held-directory primitives (el-543ou; the
//! observations they pin are el-61ar8 §3.2, numbered P/T there). Synthetic
//! files in a temporary folder only.

use super::*;
use crate::proof::{Kind, Proof};
use std::fs;
use std::io::ErrorKind;
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;
use std::process::Command;

const ERROR_ACCESS_DENIED: i32 = 5;
const ERROR_NOT_SAME_DEVICE: i32 = 17;
const ERROR_SHARING_VIOLATION: i32 = 32;

fn junction(link: &Path, target: &Path) {
    let out = Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(link)
        .arg(target)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "mklink /J failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn fsutil(args: &[&std::ffi::OsStr]) -> Result<(), String> {
    let out = Command::new("fsutil").args(args).output().unwrap();
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "fsutil {:?}: {}{}",
            args,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ))
    }
}

fn setup() -> (tempfile::TempDir, Dir, Dir) {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join("home")).unwrap();
    fs::create_dir(tmp.path().join("quarantine")).unwrap();
    let home = Dir::open(&tmp.path().join("home")).unwrap();
    let q = Dir::open(&tmp.path().join("quarantine")).unwrap();
    (tmp, home, q)
}

fn read(dir: &Dir, name: &str) -> Vec<u8> {
    fs::read(dir.join(name)).unwrap()
}

/// P2b: a name that differs only in case is the same name, and the
/// rename fails; neither file is touched.
#[test]
fn a_case_only_collision_is_refused() {
    let (_tmp, home, q) = setup();
    fs::write(home.join("a.jpg"), b"photograph 4031").unwrap();
    fs::write(q.join("A.JPG"), b"stranger 7719").unwrap();
    let held = home.hold("a.jpg").unwrap();
    let e = rename_into(&held, &q, "a.jpg").unwrap_err();
    assert_eq!(e.kind(), ErrorKind::AlreadyExists, "{e}");
    drop(held);
    assert_eq!(read(&home, "a.jpg"), b"photograph 4031");
    assert_eq!(read(&q, "A.JPG"), b"stranger 7719");
}

/// P2c: a junction whose target is gone still holds its name.
#[test]
fn a_dangling_junction_at_the_name_is_refused() {
    let (tmp, home, q) = setup();
    fs::write(home.join("a.jpg"), b"photograph 4031").unwrap();
    let gone = tmp.path().join("gone");
    fs::create_dir(&gone).unwrap();
    junction(&q.join("a.jpg"), &gone);
    fs::remove_dir(&gone).unwrap();
    let held = home.hold("a.jpg").unwrap();
    let e = rename_into(&held, &q, "a.jpg").unwrap_err();
    assert_eq!(e.kind(), ErrorKind::AlreadyExists, "{e}");
    drop(held);
    assert_eq!(read(&home, "a.jpg"), b"photograph 4031");
    assert!(q.entry_at("a.jpg").unwrap().is_link());
}

/// P7c/d: the 8.3 alias of another file is that file's name. Setting an
/// alias needs administrator rights (SeRestorePrivilege), which the CI
/// runner and the test VM have; without them this fails and says so.
#[test]
fn a_short_name_alias_of_another_file_is_refused() {
    let (_tmp, home, q) = setup();
    fs::write(home.join("a.jpg"), b"photograph 4031").unwrap();
    let long = q.join("longphotograph.jpg");
    fs::write(&long, b"stranger 7719").unwrap();
    fsutil(&[
        "file".as_ref(),
        "setshortname".as_ref(),
        long.as_os_str(),
        "LONGPH~9.JPG".as_ref(),
    ])
    .expect("setting an 8.3 alias needs an elevated test run");
    assert_eq!(
        q.stat_at("LONGPH~9.JPG").unwrap().0,
        q.stat_at("longphotograph.jpg").unwrap().0,
        "the alias opens the long-named file"
    );
    let held = home.hold("a.jpg").unwrap();
    let e = rename_into(&held, &q, "LONGPH~9.JPG").unwrap_err();
    assert_eq!(e.kind(), ErrorKind::AlreadyExists, "{e}");
    drop(held);
    assert_eq!(read(&home, "a.jpg"), b"photograph 4031");
    assert_eq!(fs::read(&long).unwrap(), b"stranger 7719");
}

/// P3a: a file another program has open without FILE_SHARE_DELETE (as
/// Lightroom or a preview does) cannot be held: busy, nothing done.
#[test]
fn a_file_another_program_has_open_is_busy() {
    let (_tmp, home, _q) = setup();
    fs::write(home.join("a.jpg"), b"photograph 4031").unwrap();
    let other = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0x1 | 0x2) // FILE_SHARE_READ | FILE_SHARE_WRITE
        .open(home.join("a.jpg"))
        .unwrap();
    let e = home.hold("a.jpg").unwrap_err();
    assert_eq!(e.raw_os_error(), Some(ERROR_SHARING_VIOLATION), "{e}");
    drop(other);
    assert_eq!(read(&home, "a.jpg"), b"photograph 4031");
    home.hold("a.jpg")
        .expect("free again once the other handle closes");
}

/// P3c/d: while held, nobody else renames, removes or writes the file;
/// the move then takes exactly the object that was held.
#[test]
fn a_held_photo_cannot_be_rewritten_or_renamed_meanwhile() {
    let (_tmp, home, q) = setup();
    fs::write(home.join("a.jpg"), b"photograph 4031").unwrap();
    let held = home.hold("a.jpg").unwrap();
    let before = Proof::of_file(&held).unwrap();
    let renamed = fs::rename(home.join("a.jpg"), home.join("swapped.jpg")).unwrap_err();
    assert_eq!(renamed.raw_os_error(), Some(ERROR_SHARING_VIOLATION));
    let removed = fs::remove_file(home.join("a.jpg")).unwrap_err();
    assert_eq!(removed.raw_os_error(), Some(ERROR_SHARING_VIOLATION));
    let written = fs::OpenOptions::new()
        .write(true)
        .open(home.join("a.jpg"))
        .unwrap_err();
    assert_eq!(written.raw_os_error(), Some(ERROR_SHARING_VIOLATION));
    rename_into(&held, &q, "a.jpg").unwrap();
    assert_eq!(before.check_file(&held), crate::proof::Verdict::Same);
    drop(held);
    assert_eq!(read(&q, "a.jpg"), b"photograph 4031");
    assert!(home.stat_at("a.jpg").is_err());
}

/// P3g/h: the folder of a held file cannot be renamed; a folder held
/// locked cannot be renamed even with nothing open inside it; and a folder
/// someone else holds open for renaming cannot be locked (busy).
#[test]
fn the_folder_of_a_held_photo_cannot_be_moved_meanwhile() {
    let (tmp, home, _q) = setup();
    fs::write(home.join("a.jpg"), b"photograph 4031").unwrap();
    let held = home.hold("a.jpg").unwrap();
    let e = fs::rename(tmp.path().join("home"), tmp.path().join("moved")).unwrap_err();
    assert_eq!(e.raw_os_error(), Some(ERROR_ACCESS_DENIED), "{e}");
    drop(held);

    let locked = Dir::open_locked(&tmp.path().join("home")).unwrap();
    let e = fs::rename(tmp.path().join("home"), tmp.path().join("moved")).unwrap_err();
    assert_eq!(e.raw_os_error(), Some(ERROR_SHARING_VIOLATION), "{e}");
    // A child of a locked folder can still be moved by this process.
    locked.rename_no_replace("a.jpg", "b.jpg").unwrap();
    let sub = locked.mkdir("sub").unwrap();
    drop(sub);
    let sub_locked = locked.open_dir_locked("sub").unwrap();
    let e = fs::rename(locked.join("sub"), locked.join("sub2")).unwrap_err();
    assert_eq!(e.raw_os_error(), Some(ERROR_SHARING_VIOLATION), "{e}");
    drop((sub_locked, locked));

    let renamer = fs::OpenOptions::new()
        .access_mode(0x0001_0000 | 0x80) // DELETE | FILE_READ_ATTRIBUTES
        .share_mode(0x7)
        .custom_flags(0x0200_0000) // FILE_FLAG_BACKUP_SEMANTICS
        .open(tmp.path().join("home"))
        .unwrap();
    let e = Dir::open_locked(&tmp.path().join("home")).err().unwrap();
    assert_eq!(e.raw_os_error(), Some(ERROR_SHARING_VIOLATION), "{e}");
    drop(renamer);
    assert_eq!(read(&home, "b.jpg"), b"photograph 4031");
}

/// P4: a read-only photograph moves and comes back, still read-only.
#[test]
fn a_readonly_photo_moves_and_comes_back_readonly() {
    let (_tmp, home, q) = setup();
    let path = home.join("a.jpg");
    fs::write(&path, b"photograph 4031").unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_readonly(true);
    fs::set_permissions(&path, perms).unwrap();
    let held = home.hold("a.jpg").unwrap();
    rename_into(&held, &q, "a.jpg").unwrap();
    rename_into(&held, &home, "a.jpg").unwrap();
    drop(held);
    assert!(fs::metadata(&path).unwrap().permissions().readonly());
    assert_eq!(read(&home, "a.jpg"), b"photograph 4031");
}

/// P6: a junction is held and moved as itself; what it points to is not
/// touched, and it is never opened as the folder it points to unless asked.
#[test]
fn a_junction_is_held_and_moved_as_itself() {
    let (tmp, home, q) = setup();
    let target = tmp.path().join("elsewhere");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("x.jpg"), b"photograph 4031").unwrap();
    junction(&home.join("j"), &target);

    let e = home.entry_at("j").unwrap();
    assert!(e.is_link() && !e.is_dir(), "{e:?}");
    assert!(Dir::open(&home.join("j")).is_err(), "not followed");
    assert!(home.open_dir("j").is_err(), "not followed");
    assert!(home.open_file("j", false).is_err());
    let through = Dir::open_following(&home.join("j")).unwrap();
    assert_eq!(
        through.ident().unwrap(),
        Dir::open(&target).unwrap().ident().unwrap()
    );
    drop(through);

    let held = home.hold("j").unwrap();
    assert_eq!(Proof::of_file(&held).unwrap().kind, Kind::Link);
    rename_into(&held, &q, "j").unwrap();
    drop(held);
    assert!(q.entry_at("j").unwrap().is_link());
    assert!(home.stat_at("j").is_err());
    assert_eq!(fs::read(target.join("x.jpg")).unwrap(), b"photograph 4031");
    assert_eq!(
        fs::read(q.join("j").join("x.jpg")).unwrap(),
        b"photograph 4031"
    );
}

/// P8: through handles there is no MAX_PATH.
#[test]
fn a_photo_deeper_than_max_path_moves_and_returns() {
    let (_tmp, home, q) = setup();
    let mut dir = home.try_clone().unwrap();
    while dir.path().as_os_str().len() < 400 {
        dir = dir
            .mkdir("a-rather-long-folder-name-of-forty-chars")
            .unwrap();
    }
    use std::io::Write;
    let mut f = dir.create_new("deep.jpg", 0o600).unwrap();
    f.write_all(b"photograph 4031").unwrap();
    drop(f);
    let deep = dir.join("deep.jpg");
    assert!(deep.as_os_str().len() > 400);
    let held = dir.hold("deep.jpg").unwrap();
    let before = Proof::of_file(&held).unwrap();
    rename_into(&held, &q, "deep.jpg").unwrap();
    assert!(dir.stat_at("deep.jpg").is_err());
    rename_into(&held, &dir, "deep.jpg").unwrap();
    assert_eq!(before.check_file(&held), crate::proof::Verdict::Same);
    drop(held);
    assert_eq!(fs::read(&deep).unwrap(), b"photograph 4031");
    assert!(dir.current_path().unwrap().as_os_str().len() > 400);
}

/// P11: a quarantine on another volume is refused by the rename itself,
/// and nothing is copied. The CI runner keeps its temporary folder on C:
/// and the build on D:; elsewhere, a folder on a second volume can be
/// named in `PC_TEST_OTHER_VOLUME`. Without one this skips, except on CI.
#[test]
fn a_quarantine_on_another_volume_is_refused_and_nothing_is_copied() {
    let (_tmp, home, _q) = setup();
    let other_root = std::env::var_os("PC_TEST_OTHER_VOLUME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            let exe = std::env::current_exe().ok()?;
            Some(exe.parent()?.to_path_buf())
        })
        .unwrap();
    let other = tempfile::tempdir_in(&other_root).unwrap();
    let there = Dir::open(other.path()).unwrap();
    if there.ident().unwrap().0 == home.ident().unwrap().0 {
        assert!(
            std::env::var_os("CI").is_none(),
            "CI must have a second volume for this test"
        );
        eprintln!("skipped: {} is on the same volume", other_root.display());
        return;
    }
    fs::write(home.join("a.jpg"), b"photograph 4031").unwrap();
    let held = home.hold("a.jpg").unwrap();
    let e = rename_into(&held, &there, "a.jpg").unwrap_err();
    assert_eq!(e.raw_os_error(), Some(ERROR_NOT_SAME_DEVICE), "{e}");
    drop(held);
    assert_eq!(read(&home, "a.jpg"), b"photograph 4031");
    assert!(there.names().unwrap().is_empty(), "nothing was copied");
}

/// T2: names Windows cannot open again by path are neither created nor
/// accepted as a destination.
#[test]
fn names_windows_cannot_spell_back_are_refused() {
    let (_tmp, home, q) = setup();
    fs::write(home.join("a.jpg"), b"photograph 4031").unwrap();
    let refused = [
        "dot.jpg.",
        "space.jpg ",
        "CON",
        "con.jpg",
        "Nul.txt",
        "COM1.jpg",
        "lpt9",
        "a:b.jpg",
        "a*.jpg",
        "a?.jpg",
        "a\"b",
        "a<b",
        "a>b",
        "a|b",
        "tab\t.jpg",
        r"sub\x.jpg",
        "sub/x.jpg",
        "..",
    ];
    let held = home.hold("a.jpg").unwrap();
    for name in refused {
        assert!(windows::check_name(name).is_err(), "{name:?}");
        let e = q.create_new(name, 0o600).unwrap_err();
        assert_eq!(e.kind(), ErrorKind::InvalidInput, "{name:?}: {e}");
        let e = rename_into(&held, &q, name).unwrap_err();
        assert_eq!(e.kind(), ErrorKind::InvalidInput, "{name:?}: {e}");
    }
    drop(held);
    assert!(q.names().unwrap().is_empty());
    for fine in [
        "a.jpg",
        "кадр 1.CR3",
        ".xmp",
        "._a.jpg",
        "con-test.jpg",
        "COM10.jpg",
    ] {
        assert!(windows::check_name(fine).is_ok(), "{fine:?}");
    }
}

/// T5: in a folder marked case-sensitive, two spellings are two files,
/// and each is listed and opened exactly as spelled.
#[test]
fn a_case_sensitive_folder_keeps_each_spelling_apart() {
    let (tmp, _home, _q) = setup();
    let cs = tmp.path().join("cs");
    fs::create_dir(&cs).unwrap();
    fsutil(&[
        "file".as_ref(),
        "setCaseSensitiveInfo".as_ref(),
        cs.as_os_str(),
        "enable".as_ref(),
    ])
    .expect("marking a folder case-sensitive");
    let dir = Dir::open(&cs).unwrap();
    use std::io::Write;
    dir.create_new("a.xmp", 0)
        .unwrap()
        .write_all(b"lower")
        .unwrap();
    dir.create_new("a.XMP", 0)
        .unwrap()
        .write_all(b"upper")
        .unwrap();
    let mut names: Vec<_> = dir.names().unwrap();
    names.sort();
    assert_eq!(names, ["a.XMP", "a.xmp"].map(std::ffi::OsString::from));
    assert_ne!(
        dir.stat_at("a.xmp").unwrap().0,
        dir.stat_at("a.XMP").unwrap().0
    );
    assert!(dir.stat_at("A.xmp").is_err(), "no third spelling");
    let mut s = String::new();
    use std::io::Read;
    dir.open_file("a.XMP", false)
        .unwrap()
        .read_to_string(&mut s)
        .unwrap();
    assert_eq!(s, "upper");
}

/// P1/P9: the system's own path for a held object comes back in the
/// spelling a walk records, and follows a rename.
#[test]
fn the_current_path_is_given_without_the_verbatim_prefix() {
    let (tmp, home, _q) = setup();
    let now = home.current_path().unwrap();
    assert!(
        !now.to_string_lossy().starts_with(r"\\?\"),
        "{}",
        now.display()
    );
    assert!(home.is_at(&now));
    drop(home);
    let sub = Dir::open(&tmp.path().join("quarantine")).unwrap();
    fs::rename(tmp.path().join("quarantine"), tmp.path().join("renamed")).unwrap();
    assert!(sub.current_path().unwrap().ends_with("renamed"));
    assert!(sub.is_at(&tmp.path().join("renamed")));
    assert!(!sub.is_at(&tmp.path().join("quarantine")));
    use crate::winfs::without_verbatim as wv;
    assert_eq!(wv(r"\\?\C:\x\y".into()), Path::new(r"C:\x\y"));
    assert_eq!(
        wv(r"\\?\UNC\nas\share\x".into()),
        Path::new(r"\\nas\share\x")
    );
    assert_eq!(
        wv(r"\\?\Volume{1234}\x".into()),
        Path::new(r"\\?\Volume{1234}\x")
    );
}

/// An object's identity does not depend on how its name is spelled; a
/// recorded path in another case still leads to the held folder (P7c).
#[test]
fn identity_is_compared_not_spelling() {
    let (tmp, home, _q) = setup();
    assert!(home.is_at(&tmp.path().join("HOME")));
    fs::write(home.join("a.jpg"), b"frame").unwrap();
    assert_eq!(
        home.stat_at("A.JPG").unwrap().0,
        home.stat_at("a.jpg").unwrap().0
    );
}

/// F2 and §5: a volume that is not NTFS gives no identity and no evidence.
/// Needs a FAT or exFAT folder named in `PC_TEST_FAT_DIR` (the VM attaches
/// a small exFAT VHD for it); not available on the CI runner.
#[test]
#[ignore = "needs PC_TEST_FAT_DIR on a FAT/exFAT volume (run in the VM)"]
fn a_volume_that_is_not_ntfs_gives_no_identity() {
    let root = std::env::var_os("PC_TEST_FAT_DIR").expect("PC_TEST_FAT_DIR");
    let tmp = tempfile::tempdir_in(root).unwrap();
    fs::write(tmp.path().join("a.jpg"), b"frame").unwrap();
    let dir = Dir::open(tmp.path()).unwrap();
    let e = dir.ident().unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Unsupported, "{e}");
    assert!(e.to_string().contains("not supported"), "{e}");
    assert!(dir.stat_at("a.jpg").is_err());
    assert_eq!(Proof::at(&dir, "a.jpg"), None);
    let f = fs::File::open(tmp.path().join("a.jpg")).unwrap();
    assert_eq!(Proof::of_file(&f), None);
}
