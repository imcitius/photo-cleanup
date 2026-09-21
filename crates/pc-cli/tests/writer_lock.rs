//! One writer for one archive, whichever way it is being used.

use std::process::Command;

fn photo_cleanup(db: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_photo-cleanup"))
        .arg("--db")
        .arg(db)
        .args(args)
        .output()
        .expect("не запустить photo-cleanup")
}

#[test]
fn a_command_that_writes_waits_for_nobody_so_it_is_refused() {
    // The server's single-writer gate lives in its own memory, and the
    // command line cannot see it. Without a lock the system keeps, `index`
    // would happily rewrite the index while a job is moving files.
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("test.db");
    pc_db::Db::open(&db).unwrap();
    let archive = tmp.path().join("archive");
    std::fs::create_dir_all(&archive).unwrap();

    let held = pc_core::lock::take_writer(&db, "sorting by date").unwrap();

    let refused = photo_cleanup(&db, &["index", "--root", archive.to_str().unwrap()]);
    assert!(!refused.status.success(), "команда не была отклонена");
    let said = String::from_utf8_lossy(&refused.stderr);
    assert!(
        said.contains("already working") || said.contains("уже работает"),
        "отказ не объяснён: {said}"
    );
    assert!(
        said.contains("sorting by date"),
        "не сказано, кто держит: {said}"
    );

    // Reading goes on meanwhile: this is a gate on writing, not on looking.
    let reading = photo_cleanup(&db, &["status"]);
    assert!(
        reading.status.success(),
        "чтение отклонено: {}",
        String::from_utf8_lossy(&reading.stderr)
    );

    drop(held);
    let now = photo_cleanup(&db, &["index", "--root", archive.to_str().unwrap()]);
    assert!(
        now.status.success(),
        "после освобождения замка команда всё ещё отклонена: {}",
        String::from_utf8_lossy(&now.stderr)
    );
}
