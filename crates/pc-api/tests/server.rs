//! The server as a program that embeds it sees it: a real socket on a port
//! the system chose, a start-up that fails with an error rather than a
//! message on a console, and a stop that waits for a job to reach a file
//! boundary.

use pc_api::{start, ServerConfig, Shutdown, ShutdownError};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn config(dir: &Path) -> ServerConfig {
    ServerConfig {
        db_path: dir.join("photo-cleanup.db"),
        thumbs: dir.join("thumbs"),
        quarantine: None,
        bind: "127.0.0.1:0".parse().unwrap(),
    }
}

/// One request over a fresh connection, the way a window would make it.
async fn http(addr: SocketAddr, method: &str, path: &str, body: &str) -> (u16, String) {
    http_with_headers(addr, method, path, &format!("Host: {addr}"), body).await
}

/// Like [`http`], but `headers` replaces the default `Host: {addr}` line
/// entirely, so a test can send a foreign or missing `Host`, or add
/// `X-Forwarded-Host`, against a real listener bound by [`start`].
async fn http_with_headers(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &str,
    body: &str,
) -> (u16, String) {
    let request = format!(
        "{method} {path} HTTP/1.1\r\n{headers}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    tokio::task::spawn_blocking(move || {
        let mut s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        s.write_all(request.as_bytes()).unwrap();
        let mut out = Vec::new();
        s.read_to_end(&mut out).unwrap();
        let text = String::from_utf8_lossy(&out).into_owned();
        let status = text
            .split_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let body = text
            .split_once("\r\n\r\n")
            .map(|(_, b)| b.to_string())
            .unwrap_or_default();
        (status, body)
    })
    .await
    .unwrap()
}

fn job_state(db: &Path, id: i64) -> String {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.query_row("SELECT state FROM jobs WHERE id=?1", [id], |r| r.get(0))
        .unwrap()
}

/// Enough real JPEGs that an index takes long enough to be caught running.
fn archive(root: &Path) -> PathBuf {
    // Roots are refused when they pass through a symlink, and the temporary
    // directory on macOS lives under one (`/var`).
    let archive = root.canonicalize().unwrap().join("archive");
    std::fs::create_dir_all(&archive).unwrap();
    let img = image::RgbImage::from_fn(320, 240, |x, y| {
        image::Rgb([
            ((x * 7 + y * 3) % 256) as u8,
            ((y * 5) % 256) as u8,
            ((x + y) % 256) as u8,
        ])
    });
    let mut encoded = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut encoded)
        .encode_image(&img)
        .unwrap();
    for n in 0..400 {
        std::fs::write(archive.join(format!("DSC{n:04}.JPG")), &encoded).unwrap();
    }
    archive
}

async fn start_index(addr: SocketAddr, archive: &Path) -> i64 {
    let body = serde_json::json!({
        "kind": "index",
        "params": {"roots": [archive], "min_size": 0, "readers_per_disk": 1, "workers": 1}
    })
    .to_string();
    let (status, v) = http(addr, "POST", "/api/jobs", &body).await;
    assert_eq!(status, 202, "{v}");
    serde_json::from_str::<serde_json::Value>(&v).unwrap()["job_id"]
        .as_i64()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_ready_server_answers_on_the_port_the_system_chose_and_then_stops() {
    let tmp = tempfile::tempdir().unwrap();
    let server = start(config(tmp.path())).await.unwrap();
    let addr = server.local_addr();
    assert!(addr.ip().is_loopback());
    assert_ne!(addr.port(), 0, "the real port, not the one asked for");
    assert_eq!(server.active_job(), None);

    let (status, body) = http(addr, "GET", "/api/status", "").await;
    assert_eq!(status, 200, "{body}");
    let (status, _) = http(addr, "GET", "/", "").await;
    assert_eq!(status, 200);

    server.shutdown(Shutdown::IfIdle).await.unwrap();
    assert!(
        TcpStream::connect(addr).is_err(),
        "a stopped server still accepts connections"
    );
    // The connection is closed: the same database opens as a new server.
    let again = start(config(tmp.path())).await.unwrap();
    again.shutdown(Shutdown::IfIdle).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_busy_address_is_an_error_returned_to_the_caller() {
    let tmp = tempfile::tempdir().unwrap();
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut cfg = config(tmp.path());
    cfg.bind = taken.local_addr().unwrap();
    let err = start(cfg).await.unwrap_err();
    assert!(
        format!("{err:#}").contains(&taken.local_addr().unwrap().to_string()),
        "{err:#}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_database_that_cannot_be_opened_is_an_error_and_binds_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    // A directory where the database should be: nothing can open it.
    let mut cfg = config(tmp.path());
    std::fs::create_dir_all(&cfg.db_path).unwrap();
    let port = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        probe.local_addr().unwrap()
    };
    cfg.bind = port;
    assert!(start(cfg).await.is_err());
    // Initialisation comes first, so a failed start leaves no socket behind.
    assert!(TcpStream::connect(port).is_err());

    // Not a database at all.
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    std::fs::write(&cfg.db_path, vec![0x5a; 8192]).unwrap();
    assert!(start(cfg).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn stopping_while_a_job_runs_refuses_or_waits_for_a_file_boundary() {
    let tmp = tempfile::tempdir().unwrap();
    let archive = archive(tmp.path());
    let cfg = config(tmp.path());
    let db = cfg.db_path.clone();
    let server = start(cfg).await.unwrap();
    let addr = server.local_addr();
    let id = start_index(addr, &archive).await;
    let job = server.active_job().expect("the job just started");
    assert_eq!((job.id, job.kind.as_str()), (id, "index"));

    // IfIdle does not touch a running job, and hands the server back.
    let server = match server.shutdown(Shutdown::IfIdle).await {
        Err(ShutdownError::Busy { server, job }) => {
            assert_eq!(job.id, id);
            server
        }
        other => panic!("expected Busy, got {other:?}"),
    };
    let (status, _) = http(addr, "GET", "/api/status", "").await;
    assert_eq!(status, 200, "the handed-back server stopped serving");

    // CancelJob: stop at the next file boundary, and wait for it.
    server.shutdown(Shutdown::CancelJob).await.unwrap();

    // Returned only after the job wrote how it ended.
    assert_eq!(job_state(&db, id), "cancelled");
    assert!(TcpStream::connect(addr).is_err());
    // It let go of the writer: a new server starts and does not call the
    // job interrupted — it ended properly.
    let again = start(config(tmp.path())).await.unwrap();
    assert_eq!(job_state(&db, id), "cancelled");
    again.shutdown(Shutdown::IfIdle).await.unwrap();
}

/// The Host guard (el-55m) has to be wired inside [`start`] itself, not only
/// in a unit test of the middleware function — a desktop window and the CLI
/// both go through `start`, and only a real listener from it proves either
/// one is actually protected. Exercises all three angles from the same
/// server: the request the desktop/browser will really send succeeds, a
/// rebound or wrong-port Host is rejected, and a spoofed
/// `X-Forwarded-Host` cannot stand in for it.
#[tokio::test(flavor = "multi_thread")]
async fn a_started_server_enforces_the_loopback_host_guard() {
    let tmp = tempfile::tempdir().unwrap();
    let server = start(config(tmp.path())).await.unwrap();
    let addr = server.local_addr();
    assert!(addr.ip().is_loopback());

    // Its own Host, at the port the system actually chose: allowed.
    let (status, body) = http(addr, "GET", "/api/status", "").await;
    assert_eq!(status, 200, "{body}");

    // A rebound domain: rejected, DNS-rebinding style.
    let (status, _) = http_with_headers(addr, "GET", "/api/status", "Host: evil.example", "").await;
    assert_eq!(status, 421);

    // The right host, wrong port: rejected.
    let (status, _) = http_with_headers(
        addr,
        "GET",
        "/api/status",
        &format!("Host: 127.0.0.1:{}", addr.port().wrapping_add(1).max(1)),
        "",
    )
    .await;
    assert_eq!(status, 421);

    // A spoofed X-Forwarded-Host claiming the real address must not let a
    // foreign Host through — the guard trusts only Host.
    let (status, _) = http_with_headers(
        addr,
        "GET",
        "/api/status",
        &format!("Host: evil.example\r\nX-Forwarded-Host: {addr}"),
        "",
    )
    .await;
    assert_eq!(status, 421);

    server.shutdown(Shutdown::IfIdle).await.unwrap();
}
