//! A native quit request with a running job waits for an answer, and the
//! headless test mode only hides that question: it never answers it
//! (el-4vtqk, B3).
//!
//! The real desktop binary gets the same `kAEQuitApplication` Apple Event
//! that the Dock's Quit, Cmd+Q from another app or a logout send. AppKit asks
//! the delegate, the delegate answers "later" and the app asks the user
//! whether to stop the job. Headless launches show no sheet, but the
//! decision must stay pending exactly as if the sheet were open: no reply to
//! AppKit, the API, the job and both writer locks untouched, and a following
//! SIGTERM/SIGINT still stops the work in the coordinated way. An answer
//! comes only from the explicit, debug-only `PC_DESKTOP_TEST_QUIT_ANSWER`
//! file, and both answers go down the same path as the dialog's buttons.
//!
//! Only programmatically generated PNG files are used; nothing is moved.
//! macOS only: Apple Events and the native termination reply exist only
//! there.
#![cfg(target_os = "macos")]

mod desktop_app;

use desktop_app::{job_state, post_index, recorded_state, synthetic_archive, wait_running, App};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

/// Enough files that the index job is still running when the test ends its
/// checks, so "the job keeps running" is observed, not assumed.
const ARCHIVE_FILES: usize = 4000;
/// How long a pending decision is watched for an unexpected reply. The
/// headless auto-answer of 9f656df replied within milliseconds.
const PENDING_WATCH: Duration = Duration::from_millis(1500);
const BUSY: &str = "desktop quit: busy job";
const REPLY: &str = "desktop native termination reply";

#[test]
fn headless_quit_without_an_answer_stays_pending_and_sigterm_stops_the_work() {
    unanswered_quit_then_signal(libc::SIGTERM);
}

#[test]
fn headless_quit_without_an_answer_stays_pending_and_sigint_stops_the_work() {
    unanswered_quit_then_signal(libc::SIGINT);
}

#[test]
fn explicit_test_answers_continue_and_stop_behave_like_the_dialog_buttons() {
    let fixture = Fixture::new();
    let answer = fixture.root.join("quit-answer");
    let mut app = App::launch_with(
        &fixture.app_data,
        &fixture.data,
        &[("PC_DESKTOP_TEST_QUIT_ANSWER", answer.to_str().unwrap())],
    );
    let (base, job) = fixture.start(&mut app);

    // Even with the mechanism configured, nothing is decided before the
    // test answers.
    let first = quit_in_background(&app);
    app.wait_for_log(BUSY, 1, Duration::from_secs(30));
    assert_pending(&mut app, &first, &fixture, &base, job);

    // Continue: AppKit gets "do not terminate" (userCanceledErr), the
    // process, the job and the locks stay.
    write_answer(&answer, "continue");
    let (status, error) = first.recv_timeout(Duration::from_secs(30)).unwrap();
    assert_eq!((status, error), (0, -128), "{}", app.log());
    assert!(
        app.log().contains(&format!("{REPLY}: false")),
        "{}",
        app.log()
    );
    std::thread::sleep(Duration::from_millis(300));
    assert!(app.running(), "{}", app.log());
    assert_eq!(job_state(&base, job), "running", "{}", app.log());
    assert_eq!(app.locks_held(&fixture.data), [true, true]);
    assert!(!answer.exists(), "an answer is used once");

    // The next quit asks again instead of reusing the old answer.
    let second = quit_in_background(&app);
    app.wait_for_log(BUSY, 2, Duration::from_secs(30));
    assert_pending(&mut app, &second, &fixture, &base, job);

    // Stop: the job is cancelled at a file boundary, then AppKit is told
    // to terminate and the process exits 0 with the locks released.
    write_answer(&answer, "stop");
    let status = app.wait(Duration::from_secs(60));
    assert!(status.success(), "{status:?}\n{}", app.log());
    assert!(
        app.log().contains(&format!("{REPLY}: true")),
        "{}",
        app.log()
    );
    let (_, error) = second.recv_timeout(Duration::from_secs(30)).unwrap();
    assert_ne!(error, -128, "Stop must not be answered as Continue");
    fixture.assert_stopped(&app, job);
}

fn unanswered_quit_then_signal(signal: libc::c_int) {
    let fixture = Fixture::new();
    let mut app = App::launch(&fixture.app_data, &fixture.data);
    let (base, job) = fixture.start(&mut app);

    let quit = quit_in_background(&app);
    app.wait_for_log(BUSY, 1, Duration::from_secs(30));
    assert_pending(&mut app, &quit, &fixture, &base, job);

    assert_eq!(unsafe { libc::kill(app.pid(), signal) }, 0);
    let status = app.wait(Duration::from_secs(60));
    assert!(
        status.success(),
        "signal {signal} must drain and exit 0, got {status:?}\n{}",
        app.log()
    );
    assert!(
        app.log().contains(&format!("{REPLY}: true")),
        "{}",
        app.log()
    );
    assert!(
        !app.log().contains(&format!("{REPLY}: false")),
        "{}",
        app.log()
    );
    let (_, error) = quit.recv_timeout(Duration::from_secs(30)).unwrap();
    assert_ne!(error, -128, "nobody answered Continue");
    fixture.assert_stopped(&app, job);
}

/// Pending exactly as with an open sheet: no reply reached the sender, the
/// process serves its API, the job runs and both writer locks are held.
fn assert_pending(
    app: &mut App,
    reply: &mpsc::Receiver<(i32, i32)>,
    fixture: &Fixture,
    base: &str,
    job: i64,
) {
    match reply.recv_timeout(PENDING_WATCH) {
        Err(mpsc::RecvTimeoutError::Timeout) => {}
        other => panic!(
            "the quit request was answered without an answer: {other:?}\n{}",
            app.log()
        ),
    }
    let log = app.log();
    let this_request = &log[log.rfind(BUSY).unwrap()..];
    assert!(!this_request.contains(REPLY), "{log}");
    assert!(app.running(), "{log}");
    assert_eq!(job_state(base, job), "running", "{log}");
    assert_eq!(app.locks_held(&fixture.data), [true, true], "{log}");
}

/// Replaced in one rename, so the app never reads half an answer.
fn write_answer(path: &Path, answer: &str) {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, answer).unwrap();
    std::fs::rename(&tmp, path).unwrap();
}

struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    archive: PathBuf,
    app_data: PathBuf,
    data: PathBuf,
}

const SENTINEL: &[u8] = b"synthetic archive sentinel";

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        // `/var` is a link to `/private/var`; the API refuses aliased roots.
        let root = tmp.path().canonicalize().unwrap();
        let archive = root.join("archive");
        synthetic_archive(&archive, ARCHIVE_FILES);
        std::fs::write(archive.join("untouched.txt"), SENTINEL).unwrap();
        Self {
            app_data: root.join("app-data"),
            data: root.join("data"),
            archive,
            root,
            _tmp: tmp,
        }
    }

    /// API up, an index job running, and the native quit delegate installed
    /// (the API starts before it, and an Apple Event before it would test
    /// Tauri's default instead).
    fn start(&self, app: &mut App) -> (String, i64) {
        let base = app.api();
        let job = post_index(&base, &self.archive);
        wait_running(&base, job);
        app.wait_for_log("desktop lifecycle ready", 1, Duration::from_secs(30));
        (base, job)
    }

    fn assert_stopped(&self, app: &App, job: i64) {
        assert_eq!(
            recorded_state(&self.data, job),
            "cancelled",
            "{}",
            app.log()
        );
        assert_eq!(app.locks_held(&self.data), [false, false]);
        assert_eq!(
            std::fs::read(self.archive.join("untouched.txt")).unwrap(),
            SENTINEL
        );
    }
}

/// Sends `kAEQuitApplication` to the app and waits for its reply on another
/// thread: `(send status, errorNumber of the reply)`. A pending decision
/// shows up as no message on the channel.
fn quit_in_background(app: &App) -> mpsc::Receiver<(i32, i32)> {
    let pid = app.pid();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(apple_event::quit(pid, Duration::from_secs(120)));
    });
    rx
}

mod apple_event {
    use std::ffi::c_void;
    use std::time::Duration;

    #[repr(C)]
    struct AEDesc {
        descriptor_type: u32,
        data_handle: *mut c_void,
    }

    impl AEDesc {
        fn null() -> Self {
            Self {
                descriptor_type: fourcc(b"null"),
                data_handle: std::ptr::null_mut(),
            }
        }
    }

    #[link(name = "CoreServices", kind = "framework")]
    extern "C" {
        fn AECreateDesc(kind: u32, data: *const c_void, size: isize, out: *mut AEDesc) -> i16;
        fn AECreateAppleEvent(
            class: u32,
            id: u32,
            target: *const AEDesc,
            return_id: i16,
            transaction: i32,
            out: *mut AEDesc,
        ) -> i16;
        fn AESendMessage(event: *const AEDesc, reply: *mut AEDesc, mode: i32, ticks: isize) -> i32;
        fn AEGetParamPtr(
            event: *const AEDesc,
            key: u32,
            kind: u32,
            actual_kind: *mut u32,
            data: *mut c_void,
            max: isize,
            size: *mut isize,
        ) -> i16;
        fn AEDisposeDesc(desc: *mut AEDesc) -> i16;
    }

    const fn fourcc(code: &[u8; 4]) -> u32 {
        u32::from_be_bytes(*code)
    }

    const AUTO_GENERATE_RETURN_ID: i16 = -1;
    const ANY_TRANSACTION_ID: i32 = 0;
    const WAIT_REPLY: i32 = 0x3;
    const NEVER_INTERACT: i32 = 0x10;

    pub fn quit(pid: libc::pid_t, timeout: Duration) -> (i32, i32) {
        let mut target = AEDesc::null();
        let mut event = AEDesc::null();
        let mut reply = AEDesc::null();
        let mut error = 0i32;
        let ticks = (timeout.as_millis() * 60 / 1000) as isize;
        let status = unsafe {
            let mut status = i32::from(AECreateDesc(
                fourcc(b"kpid"),
                (&pid as *const libc::pid_t).cast(),
                std::mem::size_of::<libc::pid_t>() as isize,
                &mut target,
            ));
            if status == 0 {
                status = i32::from(AECreateAppleEvent(
                    fourcc(b"aevt"),
                    fourcc(b"quit"),
                    &target,
                    AUTO_GENERATE_RETURN_ID,
                    ANY_TRANSACTION_ID,
                    &mut event,
                ));
            }
            if status == 0 {
                status = AESendMessage(&event, &mut reply, WAIT_REPLY | NEVER_INTERACT, ticks);
            }
            let (mut kind, mut size) = (0u32, 0isize);
            AEGetParamPtr(
                &reply,
                fourcc(b"errn"),
                fourcc(b"long"),
                &mut kind,
                (&mut error as *mut i32).cast(),
                std::mem::size_of::<i32>() as isize,
                &mut size,
            );
            AEDisposeDesc(&mut target);
            AEDisposeDesc(&mut event);
            AEDisposeDesc(&mut reply);
            status
        };
        (status, error)
    }
}
