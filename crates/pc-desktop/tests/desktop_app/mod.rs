//! The real desktop binary, started by the tests on disposable folders.
//! Every launch is headless (debug-only `PC_DESKTOP_TEST_HEADLESS`): nothing
//! appears on the operator's screen, no Dock icon, no dialog.
#![allow(dead_code)] // Each test crate uses a different part of the harness.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub const BIN: &str = env!("CARGO_BIN_EXE_photo-cleanup-desktop");

pub struct App {
    child: Child,
    app_data: PathBuf,
    log: PathBuf,
}

impl App {
    pub fn launch(app_data: &Path, data: &Path) -> Self {
        Self::launch_with(app_data, data, &[])
    }

    pub fn launch_with(app_data: &Path, data: &Path, env: &[(&str, &str)]) -> Self {
        let log = app_data.with_extension("log");
        let out = std::fs::File::create(&log).unwrap();
        let child = Command::new(BIN)
            .arg("--data-dir")
            .arg(data)
            .env("PC_DESKTOP_TEST_APP_DATA", app_data)
            .env_remove("PC_DESKTOP_REPLACEMENT")
            .env_remove("PC_DESKTOP_TEST_SIGNAL_FAILURE")
            .env_remove("PC_DESKTOP_TEST_QUIT_ANSWER")
            // Never a window, Dock icon or dialog on the operator's screen.
            .env("PC_DESKTOP_TEST_HEADLESS", "1")
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .stdout(out.try_clone().unwrap())
            .stderr(out)
            .spawn()
            .unwrap();
        Self {
            child,
            app_data: app_data.to_path_buf(),
            log,
        }
    }

    pub fn pid(&self) -> libc::pid_t {
        self.child.id() as libc::pid_t
    }

    pub fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    pub fn running(&mut self) -> bool {
        self.child.try_wait().unwrap().is_none()
    }

    /// Waits until `needle` has been logged `count` times in all.
    pub fn wait_for_log(&mut self, needle: &str, count: usize, limit: Duration) {
        let deadline = Instant::now() + limit;
        while self.log().matches(needle).count() < count {
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!(
                    "the app exited ({status:?}) before {needle:?}\n{}",
                    self.log()
                );
            }
            assert!(
                Instant::now() < deadline,
                "{needle:?} x{count} was not logged\n{}",
                self.log()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Whether the single-instance lock and the database writer lock are
    /// still held by someone (an exclusive non-blocking `flock` fails).
    pub fn locks_held(&self, data: &Path) -> [bool; 2] {
        [
            self.app_data.join("desktop-instance.writer-lock"),
            data.join(format!("{}.writer-lock", pc_desktop::DB_FILE)),
        ]
        .map(|path| held(&path))
    }

    /// The app's own API listener: one of its listening sockets that is not
    /// the single-instance endpoint, and answers `/api/status`.
    pub fn api(&mut self) -> String {
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!("the app exited during start-up: {status:?}\n{}", self.log());
            }
            let instance = std::fs::read_to_string(self.app_data.join("desktop-instance.port"))
                .unwrap_or_default();
            let listing = Command::new("/usr/sbin/lsof")
                .args(["-nP", "-a", "-p", &self.pid().to_string()])
                .args(["-iTCP", "-sTCP:LISTEN", "-Fn"])
                .output()
                .unwrap();
            for line in String::from_utf8_lossy(&listing.stdout).lines() {
                let Some(port) = line.strip_prefix('n').and_then(|n| n.rsplit(':').next()) else {
                    continue;
                };
                if port == instance.trim() {
                    continue;
                }
                let base = format!("127.0.0.1:{port}");
                if request(&base, "GET", "/api/status", None).is_ok() {
                    return base;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("the API did not start\n{}", self.log());
    }

    pub fn wait(&mut self, limit: Duration) -> std::process::ExitStatus {
        let deadline = Instant::now() + limit;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                panic!("the app did not exit after the signal\n{}", self.log());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

pub fn post_index(base: &str, archive: &Path) -> i64 {
    let body = serde_json::json!({
        "kind": "index",
        "params": {
            "roots": [archive],
            "workers": 1,
            "readers_per_disk": 1,
            "reindex": true,
            "min_size": 0,
        }
    });
    let reply = request(base, "POST", "/api/jobs", Some(&body)).unwrap();
    reply["job_id"]
        .as_i64()
        .unwrap_or_else(|| panic!("no job id in {reply}"))
}

/// HTTP/1.0 keeps the reply unchunked: a status line, headers, the body.
pub fn request(
    base: &str,
    method: &str,
    path: &str,
    body: Option<&serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let mut stream = TcpStream::connect(base).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    write!(
        stream,
        "{method} {path} HTTP/1.0\r\nHost: {base}\r\nOrigin: http://{base}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .map_err(|e| e.to_string())?;
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).map_err(|e| e.to_string())?;
    let reply = String::from_utf8_lossy(&reply);
    let (head, body) = reply.split_once("\r\n\r\n").ok_or("no reply")?;
    if !head.starts_with("HTTP/1.0 2") && !head.starts_with("HTTP/1.1 2") {
        return Err(format!("{head}\n{body}"));
    }
    serde_json::from_str(body).map_err(|e| format!("{e}: {body}"))
}

/// Valid, distinct PNG files without an encoder: stored (uncompressed)
/// deflate blocks inside a zlib stream.
pub fn synthetic_archive(dir: &Path, count: usize) {
    std::fs::create_dir_all(dir).unwrap();
    let (width, height) = (96u32, 64u32);
    for n in 0..count {
        let mut raw = Vec::new();
        for y in 0..height {
            raw.push(0); // filter: none
            for x in 0..width {
                let v = (x * 7 + y * 13 + n as u32 * 31) as u8;
                raw.extend_from_slice(&[v, v.wrapping_mul(3), (n % 251) as u8]);
            }
        }
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        chunk(&mut png, b"IHDR", &ihdr);
        chunk(&mut png, b"IDAT", &zlib_stored(&raw));
        chunk(&mut png, b"IEND", &[]);
        std::fs::write(dir.join(format!("{n:05}.png")), png).unwrap();
    }
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = data.chunks(65535).collect();
    for (i, block) in blocks.iter().enumerate() {
        out.push(u8::from(i + 1 == blocks.len()));
        let len = block.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    out.extend_from_slice(&((b << 16) | a).to_be_bytes());
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn held(path: &Path) -> bool {
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::AsRawFd;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let fd = file.as_raw_fd();
    if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        unsafe { libc::flock(fd, libc::LOCK_UN) };
        false
    } else {
        true
    }
}

pub fn job_state(base: &str, job: i64) -> String {
    let reply = request(base, "GET", &format!("/api/jobs/{job}"), None).unwrap();
    reply["state"].as_str().unwrap_or_default().to_string()
}

/// The job as the database records it after the process has gone.
pub fn recorded_state(data: &Path, job: i64) -> String {
    let db = rusqlite::Connection::open_with_flags(
        data.join(pc_desktop::DB_FILE),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    db.query_row("SELECT state FROM jobs WHERE id=?1", [job], |r| r.get(0))
        .unwrap()
}

/// Waits until the accepted job is running and has done some files.
pub fn wait_running(base: &str, job: i64) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let reply = request(base, "GET", &format!("/api/jobs/{job}"), None).unwrap();
        let state = reply["state"].as_str().unwrap_or_default();
        let done = reply["progress"]["done"].as_u64().unwrap_or(0);
        if state == "running" && done > 0 {
            return;
        }
        assert!(matches!(state, "queued" | "running"), "{reply}");
        assert!(Instant::now() < deadline, "the job did not start: {reply}");
        std::thread::sleep(Duration::from_millis(50));
    }
}
