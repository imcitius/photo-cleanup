// Disposable disk images for macOS tests (`hdiutil`, no root).
//
// Included with `#[path]` by every test module that needs a real volume
// (pc-desktop, pc-api, pc-apply); it has no dependencies besides std and
// `tempfile`, so it adds nothing to any lockfile.
//
// * One image at a time across the whole machine: an exclusive `flock` on a
//   file in the temp dir is held from before `hdiutil create` until after
//   the detach. It works between threads and between the test binaries of
//   different crates running at once (concurrent `hdiutil create` calls were
//   seen to hang for good), and the kernel frees it if a process dies.
// * Every `hdiutil` call has a timeout; a stuck child is killed (only the one
//   this module started) and the test fails with a clear message.
// * [`DiskImage`] detaches in `Drop`, also when the test panics. Detach is
//   its only cleanup.
// * The helper deletes nothing, ever: not the mount point, not what is in
//   it, not the image file, not its temporary directory (kept, never an
//   auto-deleting temp dir). They are left in the system temp dir, which the
//   OS cleans; the image is sparse, so a leftover is a few megabytes. A
//   detach that fails twice (normal, then `-force`), or a mount still there
//   after a reported detach, fails the test with the mount path, the device
//   and the image path.

use std::fs::File;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const LOCK_WAIT: Duration = Duration::from_secs(900);
const CREATE_TIMEOUT: Duration = Duration::from_secs(120);
const ATTACH_TIMEOUT: Duration = Duration::from_secs(120);
const DETACH_TIMEOUT: Duration = Duration::from_secs(60);

extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}
const LOCK_EX: i32 = 2;
const LOCK_NB: i32 = 4;

/// Exclusive machine-wide lock; released when dropped (or the process dies).
struct ImageLock(File);

std::thread_local! {
    // A test may hold two images at once; the thread shares one lock.
    static HELD: std::cell::RefCell<std::sync::Weak<ImageLock>> =
        const { std::cell::RefCell::new(std::sync::Weak::new()) };
}

impl ImageLock {
    fn take() -> std::sync::Arc<Self> {
        HELD.with(|held| {
            if let Some(lock) = held.borrow().upgrade() {
                return lock;
            }
            let lock = std::sync::Arc::new(Self::acquire());
            *held.borrow_mut() = std::sync::Arc::downgrade(&lock);
            lock
        })
    }

    fn acquire() -> Self {
        use std::os::fd::AsRawFd;
        let path = std::env::temp_dir().join("photo-cleanup-hdiutil-tests.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .unwrap_or_else(|e| panic!("cannot open hdiutil test lock {}: {e}", path.display()));
        let started = Instant::now();
        loop {
            // SAFETY: valid open fd; flock has no memory effects.
            if unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } == 0 {
                return Self(file);
            }
            assert!(
                started.elapsed() < LOCK_WAIT,
                "hdiutil test lock {} not free after {LOCK_WAIT:?}: another test run holds a disk image",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Runs `command` to the end or kills it after `timeout`.
fn run_timed(what: &str, command: &mut Command, timeout: Duration) -> Result<(), String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{what}: cannot start {command:?}: {e}"))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return Ok(());
                }
                let mut err = String::new();
                if let Some(mut e) = child.stderr.take() {
                    let _ = std::io::Read::read_to_string(&mut e, &mut err);
                }
                return Err(format!("{what}: {command:?} failed with {status}: {err}"));
            }
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{what}: {command:?} did not finish within {timeout:?} and was killed"
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(format!("{what}: waiting for {command:?}: {e}")),
        }
    }
}

pub struct DiskImage {
    pub mount: PathBuf,
    /// The helper's own temporary directory, kept after the test.
    dir: PathBuf,
    image: PathBuf,
    /// `attach` was started: from then on the image may be attached,
    /// whatever the filesystem looks like, and `Drop` detaches.
    attach_started: bool,
    /// The mounted device, as seen right after `attach`.
    device: Option<String>,
    tool: PathBuf,
    // Dropped after `Drop::drop`: the lock covers the detach.
    _lock: std::sync::Arc<ImageLock>,
}

impl DiskImage {
    /// `owners`: `None` attaches like a double click would (external volumes:
    /// ownership ignored), `Some(on)` passes `-owners on|off`.
    pub fn new(fs: &str, owners: Option<bool>) -> Self {
        Self::with_tool(Path::new("hdiutil"), fs, owners)
    }

    /// Like [`DiskImage::new`] with another `hdiutil` program; the guard's
    /// own tests use a fake one to drive its failure paths.
    pub fn with_tool(tool: &Path, fs: &str, owners: Option<bool>) -> Self {
        let lock = ImageLock::take();
        // `.keep()`: a plain path, never removed on drop. Canonical, so the
        // mount path is the one the system reports (`/private/var/...`).
        let dir = tempfile::Builder::new()
            .prefix("pc-hdiutil-")
            .tempdir()
            .unwrap()
            .keep()
            .canonicalize()
            .unwrap();
        // `-type SPARSE` makes hdiutil name it `*.sparseimage`.
        let image = dir.join("volume.sparseimage");
        let mount = dir.join("mnt");
        // The guard exists before anything is attached, so a failing or
        // timed-out step below still ends in `Drop` and a detach.
        let mut this = Self {
            mount: mount.clone(),
            dir,
            image: image.clone(),
            attach_started: false,
            device: None,
            tool: tool.to_path_buf(),
            _lock: lock,
        };
        let mut create = Command::new(&this.tool);
        create
            .args(["create", "-quiet", "-type", "SPARSE", "-size", "64m"])
            .args(["-fs", fs, "-volname", "PCTEST"])
            .arg(&image);
        if let Err(e) = run_timed("disk image create", &mut create, CREATE_TIMEOUT) {
            panic!("{e}");
        }
        let mut attach = Command::new(&this.tool);
        attach.args(["attach", "-quiet", "-nobrowse", "-noverify"]);
        if let Some(on) = owners {
            attach.args(["-owners", if on { "on" } else { "off" }]);
        }
        attach.arg("-mountpoint").arg(&mount).arg(&image);
        this.attach_started = true;
        if let Err(e) = run_timed("disk image attach", &mut attach, ATTACH_TIMEOUT) {
            panic!("{e}");
        }
        this.device = mounted_device(&this.mount, &this.dir);
        this
    }

    /// Detaches (normal, then `-force`). Removes nothing; on any doubt the
    /// error names the mount path, the device and the image.
    fn release(&self) -> Result<(), String> {
        if !self.attach_started {
            return Ok(());
        }
        let mut errors = Vec::new();
        for force in [false, true] {
            let mut detach = Command::new(&self.tool);
            detach.args(["detach", "-quiet"]);
            if force {
                detach.arg("-force");
            }
            detach.arg(&self.mount);
            match run_timed("disk image detach", &mut detach, DETACH_TIMEOUT) {
                Ok(()) => {
                    errors.clear();
                    break;
                }
                Err(e) => errors.push(e),
            }
        }
        if !errors.is_empty() {
            return Err(self.report(&errors.join("; ")));
        }
        // A reported detach is believed only if nothing is mounted at the
        // mount path any more (a missing path has nothing mounted on it).
        match std::fs::symlink_metadata(&self.mount) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(self.report(&format!("after detach: {e}"))),
            Ok(_) => match mounted_device(&self.mount, &self.dir) {
                None => Ok(()),
                Some(device) => Err(self.report(&format!(
                    "detach reported success, {device} still mounted"
                ))),
            },
        }
    }

    fn report(&self, why: &str) -> String {
        let device = match (&self.device, mounted_device(&self.mount, &self.dir)) {
            (Some(then), Some(now)) => format!("{then} at attach, {now} now"),
            (Some(then), None) => format!("{then} at attach, nothing mounted now"),
            (None, Some(now)) => format!("none seen at attach, {now} now"),
            (None, None) => "none (not a mount point)".to_string(),
        };
        format!(
            "disk image NOT released, nothing was removed: {why}\n  \
             mount point: {}\n  device: {device}\n  image: {}\n  \
             check `hdiutil info`, detach by hand",
            self.mount.display(),
            self.image.display(),
        )
    }
}

/// The device mounted at `path`, if `path` sits on another device than
/// `parent`; `Some("unknown ...")` when that cannot be told.
fn mounted_device(path: &Path, parent: &Path) -> Option<String> {
    match (std::fs::symlink_metadata(path), std::fs::metadata(parent)) {
        (Ok(here), Ok(up)) if here.dev() == up.dev() => None,
        (Ok(here), Ok(_)) => Some(device_name(here.dev())),
        (Err(e), _) if e.kind() == std::io::ErrorKind::NotFound => None,
        (Err(e), _) | (_, Err(e)) => Some(format!("unknown ({e})")),
    }
}

/// `/dev/diskNsM` for a device number, for the failure report.
fn device_name(dev: u64) -> String {
    extern "C" {
        fn devname(dev: i32, kind: u16) -> *const std::ffi::c_char;
    }
    const S_IFBLK: u16 = 0o060000;
    // SAFETY: devname only reads its arguments and returns a pointer to a
    // static buffer (or null), copied out at once.
    let name = unsafe { devname(dev as i32, S_IFBLK) };
    if name.is_null() {
        return format!("st_dev {dev:#x}");
    }
    // SAFETY: non-null, NUL-terminated per devname(3).
    let name = unsafe { std::ffi::CStr::from_ptr(name) };
    format!("/dev/{} (st_dev {dev:#x})", name.to_string_lossy())
}

impl Drop for DiskImage {
    fn drop(&mut self) {
        if let Err(report) = self.release() {
            // Loud: the test fails. During a panic only report, a second
            // panic would abort the whole test binary.
            if std::thread::panicking() {
                eprintln!("{report}");
            } else {
                panic!("{report}");
            }
        }
    }
}
