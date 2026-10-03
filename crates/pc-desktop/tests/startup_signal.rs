//! A terminal signal that arrives as soon as the desktop app serves its API
//! must still drain the work the API has accepted (el-5cvv6, B1).
//!
//! This drives the real desktop binary the way a user's `kill` or a logout
//! would: a disposable profile and data folder, the app's own HTTP listener,
//! an index job accepted through `POST /api/jobs`, then SIGTERM or SIGINT at
//! once. Before the fix the default disposition killed the process (status
//! "signal 15") and left the job `running` in the database, which the next
//! start reported as `interrupted`. Coordinated shutdown exits with 0 and
//! records the job as `cancelled` itself.
//!
//! Only programmatically generated PNG files are used; nothing is moved.
//! macOS only: the window shell is not built on Linux, and Windows has no
//! SIGTERM/SIGINT to send.
#![cfg(target_os = "macos")]

mod desktop_app;

use desktop_app::{post_index, synthetic_archive, App};
use std::time::Duration;

#[test]
fn sigterm_right_after_the_api_starts_cancels_an_accepted_job_and_exits_cleanly() {
    immediate_signal_drains_the_job(libc::SIGTERM);
}

#[test]
fn sigint_right_after_the_api_starts_cancels_an_accepted_job_and_exits_cleanly() {
    immediate_signal_drains_the_job(libc::SIGINT);
}

#[test]
fn without_signal_handlers_no_server_is_started() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let data = root.join("data");
    let mut app = App::launch_with(
        &root.join("app-data"),
        &data,
        &[("PC_DESKTOP_TEST_SIGNAL_FAILURE", "1")],
    );
    let status = app.wait(Duration::from_secs(60));
    let log = app.log();
    assert_eq!(status.code(), Some(1), "{log}");
    assert!(
        log.contains("cannot start: cannot install the quit signal handlers"),
        "{log}"
    );
    // The data folder is resolved and opened only by the launch that never
    // came: no folder, no database, so no server could have listened on it.
    assert!(!data.exists(), "{log}");
}

fn immediate_signal_drains_the_job(signal: libc::c_int) {
    let tmp = tempfile::tempdir().unwrap();
    // `/var` is a link to `/private/var`; the API refuses aliased roots.
    let root = tmp.path().canonicalize().unwrap();
    let archive = root.join("archive");
    synthetic_archive(&archive, 1500);
    let sentinel = archive.join("untouched.txt");
    std::fs::write(&sentinel, b"synthetic archive sentinel").unwrap();

    let app_data = root.join("app-data");
    let data = root.join("data");
    let mut app = App::launch(&app_data, &data);
    let base = app.api();
    let job = post_index(&base, &archive);
    // No waiting for readiness here: the signal must be safe as soon as
    // the API accepted the work.
    assert_eq!(unsafe { libc::kill(app.pid(), signal) }, 0);
    let status = app.wait(Duration::from_secs(60));

    assert!(
        status.success(),
        "signal {signal} must drain and exit 0, got {status:?}; log:\n{}",
        app.log()
    );
    let db = rusqlite::Connection::open_with_flags(
        data.join(pc_desktop::DB_FILE),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let state: String = db
        .query_row("SELECT state FROM jobs WHERE id=?1", [job], |r| r.get(0))
        .unwrap();
    assert_eq!(
        state,
        "cancelled",
        "the drain records the job itself; log:\n{}",
        app.log()
    );
    assert_eq!(
        std::fs::read(&sentinel).unwrap(),
        b"synthetic archive sentinel"
    );
}
