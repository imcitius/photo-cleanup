//! Separate processes exercise the real OS lock, including owner death.
use pc_desktop::instance::{Claim, Instance};
use std::io::{BufRead, Write};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[test]
#[ignore = "subprocess fixture"]
fn instance_process() {
    let dir = std::env::var_os("PC_INSTANCE_FIXTURE").unwrap();
    let dir = std::path::Path::new(&dir);
    let replacement = std::env::var_os("PC_INSTANCE_WAIT").is_some();
    match Instance::claim(dir, replacement, Duration::from_secs(10)).unwrap() {
        Claim::Activated => println!("activated"),
        Claim::Owner(owner) => {
            println!("owner");
            std::io::stdout().flush().unwrap();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut line = String::new();
                let _ = std::io::stdin().read_line(&mut line);
                let _ = tx.send(());
            });
            while rx.try_recv().is_err() {
                if owner.activated() {
                    std::fs::write(dir.join("shown"), "yes").unwrap();
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

fn child(dir: &std::path::Path, replacement: bool) -> Child {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["--ignored", "--exact", "instance_process", "--nocapture"])
        .env("PC_INSTANCE_FIXTURE", dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    if replacement {
        cmd.env("PC_INSTANCE_WAIT", "1");
    }
    cmd.spawn().unwrap()
}

fn owner_ready(child: &mut Child) {
    let mut lines = std::io::BufReader::new(child.stdout.as_mut().unwrap());
    let mut line = String::new();
    loop {
        assert_ne!(lines.read_line(&mut line).unwrap(), 0, "owner exited early");
        if line.trim() == "owner" {
            break;
        }
        line.clear();
    }
}

fn wait(child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(s) = child.try_wait().unwrap() {
            assert!(s.success());
            return;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("process did not exit");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn second_launch_activates_and_replacement_waits_for_clean_exit() {
    let tmp = tempfile::tempdir().unwrap();
    let mut first = child(tmp.path(), false);
    owner_ready(&mut first);
    let mut second = child(tmp.path(), false);
    wait(&mut second);
    assert!(tmp.path().join("shown").exists());
    assert!(first.try_wait().unwrap().is_none());
    let mut replacement = child(tmp.path(), true);
    std::thread::sleep(Duration::from_millis(100));
    assert!(replacement.try_wait().unwrap().is_none());
    first.stdin.as_mut().unwrap().write_all(b"quit\n").unwrap();
    wait(&mut first);
    owner_ready(&mut replacement);
    replacement
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"quit\n")
        .unwrap();
    wait(&mut replacement);
    assert!(tmp.path().join("desktop-instance.writer-lock").exists());
}

#[test]
fn a_killed_owner_releases_the_lock_without_deleting_its_file() {
    let tmp = tempfile::tempdir().unwrap();
    let mut first = child(tmp.path(), false);
    owner_ready(&mut first);
    first.kill().unwrap();
    first.wait().unwrap();
    assert!(tmp.path().join("desktop-instance.writer-lock").exists());
    let mut restarted = child(tmp.path(), false);
    owner_ready(&mut restarted);
    restarted
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"quit\n")
        .unwrap();
    wait(&mut restarted);
}
