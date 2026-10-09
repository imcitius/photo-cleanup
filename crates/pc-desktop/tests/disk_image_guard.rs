//! The disk-image test helper (`test-support/disk_image.rs`) on its cleanup
//! paths, driven by a fake `hdiutil` (reviews el-3y605 and el-5ci5q). No real
//! image is created or attached: the fake "mounts" a plain directory in the
//! helper's own temporary directory, with a foreign payload in it.
//!
//! The contract under test: the helper deletes nothing at all. Detach is its
//! only cleanup; a detach it cannot confirm is reported loudly (mount path,
//! device, image path) and everything stays where it is. Its temporary
//! directory, the image file and the mount point are intentionally left
//! behind, and so are this file's own fixtures (the OS cleans the temp dir).
#![cfg(target_os = "macos")]

mod disk_image {
    #![allow(dead_code)]
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../test-support/disk_image.rs"
    ));
}

use disk_image::DiskImage;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

/// A fake `hdiutil` in a kept (never auto-deleted) temporary directory:
/// `create` writes a small image file, `attach` creates the mount point with
/// a payload (a file with mode 0640 and a nested sidecar), `detach` runs
/// `detach_body`. Every call is logged next to the tool.
fn fake_hdiutil(detach_body: &str) -> PathBuf {
    let dir = tempfile::Builder::new()
        .prefix("pc-fake-hdiutil-")
        .tempdir()
        .unwrap()
        .keep();
    let tool = dir.join("hdiutil");
    let script = format!(
        r#"#!/bin/sh
here=$(dirname "$0")
printf '%s\n' "$*" >> "$here/calls.log"
for a; do last=$a; done
case "$1" in
  create) printf 'own image\n' > "$last"; exit 0 ;;
  attach)
    while [ "$#" -gt 1 ]; do
      if [ "$1" = "-mountpoint" ]; then
        mkdir -p "$2/nested"
        printf 'foreign payload\n' > "$2/foreign-photo"
        chmod 640 "$2/foreign-photo"
        printf 'xmp\n' > "$2/nested/foreign-photo.xmp"
        exit 0
      fi
      shift
    done
    exit 11 ;;
  detach) {detach_body} ;;
esac
exit 12
"#
    );
    std::fs::write(&tool, script).unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    tool
}

fn calls(tool: &Path) -> Vec<String> {
    std::fs::read_to_string(tool.with_file_name("calls.log"))
        .unwrap()
        .lines()
        .map(|line| {
            // Keep the verb and the flags; paths differ per run.
            line.split(' ')
                .filter(|word| !word.starts_with('/'))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

const CREATE: &str = "create -quiet -type SPARSE -size 64m -fs APFS -volname PCTEST";
const ATTACH: &str = "attach -quiet -nobrowse -noverify -mountpoint";

struct Payload {
    file: PathBuf,
    bytes: Vec<u8>,
    mode: u32,
    modified: SystemTime,
    inode: u64,
    nested: PathBuf,
}

impl Payload {
    fn read(mount: &Path) -> Self {
        let file = mount.join("foreign-photo");
        let meta = std::fs::metadata(&file).unwrap();
        Self {
            bytes: std::fs::read(&file).unwrap(),
            mode: meta.permissions().mode() & 0o7777,
            modified: meta.modified().unwrap(),
            inode: meta.ino(),
            nested: mount.join("nested").join("foreign-photo.xmp"),
            file,
        }
    }

    /// The payload, as read before the drop, now lives under `mount`.
    fn assert_intact_under(&self, mount: &Path) {
        let file = mount.join("foreign-photo");
        let meta = std::fs::metadata(&file)
            .unwrap_or_else(|e| panic!("payload {} is gone: {e}", file.display()));
        assert_eq!(std::fs::read(&file).unwrap(), self.bytes);
        assert_eq!(self.bytes, b"foreign payload\n");
        assert_eq!(meta.permissions().mode() & 0o7777, self.mode);
        assert_eq!(self.mode, 0o640);
        assert_eq!(meta.modified().unwrap(), self.modified);
        assert_eq!(meta.ino(), self.inode);
        assert_eq!(
            std::fs::read(mount.join("nested").join("foreign-photo.xmp")).unwrap(),
            b"xmp\n"
        );
    }

    fn assert_intact(&self) {
        self.assert_intact_under(self.file.parent().unwrap());
        assert!(self.nested.is_file());
    }
}

fn panic_message(panic: Box<dyn std::any::Any + Send>) -> String {
    panic
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default()
}

/// Builds an image with the fake tool, runs `before_drop` on its mount
/// path, drops it and returns the mount path, the payload as it was before
/// the drop and the panic message of the drop, if any.
fn drop_image_after(
    tool: &Path,
    before_drop: impl FnOnce(&Path),
) -> (PathBuf, Payload, Option<String>) {
    let image = DiskImage::with_tool(tool, "APFS", None);
    let mount = image.mount.clone();
    let payload = Payload::read(&mount);
    before_drop(&mount);
    let outcome = catch_unwind(AssertUnwindSafe(move || drop(image)));
    (mount, payload, outcome.err().map(panic_message))
}

fn drop_image(tool: &Path) -> (PathBuf, Payload, Option<String>) {
    drop_image_after(tool, |_| {})
}

/// The image file the helper created, untouched.
fn assert_own_image_kept(helper_dir: &Path) {
    assert_eq!(
        std::fs::read(helper_dir.join("volume.sparseimage")).unwrap(),
        b"own image\n"
    );
}

/// The reviewer's reproducer (el-3y605): both detach attempts fail. The
/// mount point, its contents and their metadata stay; the image file stays;
/// the failure names the mount path, the device and the image.
#[test]
fn failed_detach_leaves_the_mount_and_its_contents_and_fails_loudly() {
    let tool = fake_hdiutil("exit 9");
    let (mount, payload, message) = drop_image(&tool);
    let message = message.expect("a failed detach must fail the test");
    let helper_dir = mount.parent().unwrap();
    assert!(message.contains(&mount.display().to_string()), "{message}");
    assert!(message.contains("device:"), "{message}");
    assert!(
        message.contains(&helper_dir.join("volume.sparseimage").display().to_string()),
        "{message}"
    );
    assert!(message.contains("detach"), "{message}");
    payload.assert_intact();
    assert_own_image_kept(helper_dir);
    assert_eq!(
        calls(&tool),
        [CREATE, ATTACH, "detach -quiet", "detach -quiet -force"]
    );
}

/// A detach that succeeds (the fake leaves a plain directory, not a mount
/// point): the test passes and nothing is removed — not the mount point,
/// not what is in it, not the image, not the temporary directory.
#[test]
fn a_successful_detach_removes_nothing() {
    let tool = fake_hdiutil("exit 0");
    let (mount, payload, message) = drop_image(&tool);
    assert_eq!(message, None);
    payload.assert_intact();
    assert!(mount.is_dir());
    assert_own_image_kept(mount.parent().unwrap());
    assert_eq!(calls(&tool), [CREATE, ATTACH, "detach -quiet"]);
}

/// Reproducer el-5ci5q B1: by the time detach returns, another (empty)
/// directory with its own mode and extended attribute sits at the mount
/// path. It is not the helper's: it stays, with its metadata, and so does
/// the original mount point (moved aside) and the image.
#[test]
fn a_foreign_directory_at_the_mount_path_after_detach_is_kept() {
    let tool = fake_hdiutil(
        r#"mv "$last" "$last-retained" && mkdir "$last" && chmod 750 "$last" \
      && xattr -w review.foreign metadata "$last" \
      && stat -f %i "$last" > "$here/foreign.inode"; exit 0"#,
    );
    let (mount, payload, _message) = drop_image(&tool);
    let foreign = std::fs::symlink_metadata(&mount)
        .unwrap_or_else(|e| panic!("foreign directory {} is gone: {e}", mount.display()));
    assert!(foreign.is_dir());
    assert_eq!(foreign.permissions().mode() & 0o7777, 0o750);
    let inode = std::fs::read_to_string(tool.with_file_name("foreign.inode")).unwrap();
    assert_eq!(foreign.ino().to_string(), inode.trim());
    let xattr = Command::new("xattr")
        .args(["-p", "review.foreign"])
        .arg(&mount)
        .output()
        .unwrap();
    assert!(xattr.status.success(), "extended attribute gone");
    assert_eq!(xattr.stdout, b"metadata\n");
    payload.assert_intact_under(&mount.with_file_name("mnt-retained"));
    assert_own_image_kept(mount.parent().unwrap());
}

/// Reproducer el-5ci5q B2: the mount path is gone before the drop. That
/// proves nothing about the attachment: the helper still detaches, and a
/// detach that fails (like the real tool on a missing path) is reported
/// loudly while the image and the moved-aside payload stay.
#[test]
fn a_missing_mount_path_still_detaches_and_keeps_the_image() {
    let tool = fake_hdiutil(r#"[ -d "$last" ] && exit 0; exit 1"#);
    let (mount, payload, message) = drop_image_after(&tool, |mount| {
        std::fs::rename(mount, mount.with_file_name("mnt-retained")).unwrap();
    });
    assert_eq!(
        calls(&tool),
        [CREATE, ATTACH, "detach -quiet", "detach -quiet -force"]
    );
    let message = message.expect("an unconfirmed detach must fail the test");
    assert!(message.contains(&mount.display().to_string()), "{message}");
    assert!(message.contains("volume.sparseimage"), "{message}");
    assert_own_image_kept(mount.parent().unwrap());
    payload.assert_intact_under(&mount.with_file_name("mnt-retained"));
}

/// A test that panics while it holds an image still detaches it; the
/// test's own panic is what the caller sees.
#[test]
fn a_panicking_test_still_detaches() {
    let tool = fake_hdiutil("exit 0");
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let _image = DiskImage::with_tool(&tool, "APFS", None);
        panic!("the test itself failed");
    }));
    assert_eq!(
        panic_message(outcome.unwrap_err()),
        "the test itself failed"
    );
    assert_eq!(calls(&tool), [CREATE, ATTACH, "detach -quiet"]);
}

/// The helper and this file call no removal API at all (contract after
/// el-5ci5q): no `remove_*`, no `unlink`/`rmdir`/`rm`, no auto-deleting
/// `TempDir` / `NamedTempFile`. Every `tempdir()` is kept.
#[test]
fn the_helper_and_its_tests_remove_nothing() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let sources = [
        root.join("../../test-support/disk_image.rs"),
        root.join("tests/disk_image_guard.rs"),
    ];
    // Spelled in pieces so this list does not match itself.
    let forbidden: Vec<String> = [
        ["remove", "_file"],
        ["remove", "_dir"],
        ["un", "link"],
        ["r", "mdir"],
        ["\"r", "m\""],
        ["r", "m -"],
        ["r", "m \""],
        ["Temp", "Dir"],
        ["NamedTemp", "File"],
        ["tempfile::temp", "dir("],
        ["tempfile::temp", "file("],
        ["tra", "sh"],
        ["d", "elete"],
    ]
    .iter()
    .map(|[a, b]| format!("{a}{b}"))
    .collect();
    for path in &sources {
        let text = std::fs::read_to_string(path).unwrap();
        for (number, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap();
            for word in &forbidden {
                assert!(
                    !code.contains(word.as_str()),
                    "{}:{}: `{word}` in `{line}`",
                    path.display(),
                    number + 1
                );
            }
        }
        // `Builder::tempdir()` returns an auto-deleting directory: each one
        // must be turned into a plain path with `.keep()` right away.
        let flat: String = text.split_whitespace().collect();
        let tempdirs = flat.matches(concat!(".temp", "dir()")).count();
        let kept = flat
            .matches(concat!(".temp", "dir().unwrap().keep()"))
            .count();
        assert!(
            tempdirs > 0 && kept == tempdirs,
            "{}: unkept tempdir",
            path.display()
        );
    }
}
