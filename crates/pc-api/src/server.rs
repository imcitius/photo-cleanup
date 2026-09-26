//! A server that can be started and stopped by the program that owns it.
//!
//! The command line runs until Ctrl+C and nobody needs a handle. A desktop
//! shell does: it has to know the port the system chose before it can open a
//! window, show a start-up failure in that window rather than on a console
//! nobody sees, and stop the server when the operator quits — without
//! cutting a running job off halfway through a file.
//!
//! Everything a server needs before its first request — migrations, the
//! language from the settings, marking a dead process's jobs interrupted —
//! happens here and only here. `serve` is a thin wrapper over it, so the
//! command line and the desktop cannot drift apart.

use crate::{router, security, AppState};
use anyhow::{Context, Result};
use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{atomic::Ordering, Arc};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

/// Where the server keeps its data and where it listens.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub db_path: PathBuf,
    pub thumbs: PathBuf,
    /// `None` parks moved files at the root of each file's own filesystem.
    pub quarantine: Option<PathBuf>,
    /// Port 0 lets the system choose; [`Server::local_addr`] tells which.
    pub bind: SocketAddr,
}

/// A running server. Dropping it without [`Server::shutdown`] leaves the
/// server running in the background until the runtime itself goes away.
pub struct Server {
    state: Arc<AppState>,
    addr: SocketAddr,
    stop: oneshot::Sender<()>,
    http: JoinHandle<std::io::Result<()>>,
}

/// The job a server is carrying out at the moment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveJob {
    pub id: i64,
    pub kind: String,
}

/// How to treat a job that is still running when the server is asked to stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shutdown {
    /// Refuse, and hand the server back untouched, if a job is running.
    IfIdle,
    /// Ask the job to stop at the next file boundary and wait for it — with
    /// no time limit — before stopping.
    CancelJob,
}

pub enum ShutdownError {
    /// [`Shutdown::IfIdle`] found a job running. The server keeps serving
    /// and is handed back, so the caller can ask the operator and try again.
    Busy { server: Box<Server>, job: ActiveJob },
    /// Stopping itself went wrong. The server is gone either way.
    Failed(anyhow::Error),
}

impl fmt::Debug for Server {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Server").field("addr", &self.addr).finish()
    }
}

impl fmt::Debug for ShutdownError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy { job, .. } => f.debug_struct("Busy").field("job", job).finish(),
            Self::Failed(e) => f.debug_tuple("Failed").field(e).finish(),
        }
    }
}

impl fmt::Display for ShutdownError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy { job, .. } => f.write_str(&pc_core::tf!(
                "Выполняется задача №{0} ({1})",
                "Job #{0} ({1}) is still running",
                job.id,
                job.kind
            )),
            Self::Failed(e) => write!(f, "{e:#}"),
        }
    }
}

impl std::error::Error for ShutdownError {}

/// Start a server and return once it is ready.
///
/// Ready means the state exists — database opened and migrated, language
/// taken from the settings, abandoned jobs marked interrupted — and the
/// listener is bound. Connections made from this moment on wait in the
/// kernel's queue and are answered, so a window can be pointed at
/// [`Server::local_addr`] at once.
///
/// A database that cannot be opened and an address that cannot be bound are
/// returned as errors, not printed: whoever started the server decides where
/// the operator reads them.
pub async fn start(config: ServerConfig) -> Result<Server> {
    let ServerConfig {
        db_path,
        thumbs,
        quarantine,
        bind,
    } = config;
    let mut state = AppState::new(&db_path, &thumbs, quarantine)?;
    {
        // Before the first request, so an error during start-up is already in
        // the language the operator chose.
        let db = state.db.lock().unwrap();
        let settings = crate::service::settings_value(&state, &db)?;
        crate::service::apply_language(&settings);
    }
    state.network = !bind.ip().is_loopback();
    let state = Arc::new(state);
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| pc_core::tf!("не занять адрес {0}", "cannot bind {0}", bind))?;
    let addr = listener.local_addr()?;
    let app = router(state.clone());
    // A loopback bind is what the CLI defaults to and what the desktop shell
    // uses; only there does a same-machine attacker's page stand a chance at
    // DNS rebinding. A NAS bind on a LAN address is reachable from other
    // machines on purpose, so it is left exactly as it was. Wired here, after
    // the real port is known, so both `serve` and a desktop-started server go
    // through the same check — `serve` is a thin wrapper over this function.
    let app = if addr.ip().is_loopback() {
        app.layer(axum::middleware::from_fn(move |req, next| {
            security::require_loopback_host(addr, req, next)
        }))
    } else {
        app
    };
    let (stop, stopped) = oneshot::channel::<()>();
    let serve = axum::serve(listener, app).with_graceful_shutdown(async move {
        // A dropped sender stops the server just as a sent signal does.
        let _ = stopped.await;
    });
    let http = tokio::spawn(async move { serve.await });
    Ok(Server {
        state,
        addr,
        stop,
        http,
    })
}

impl Server {
    /// The address actually bound, with the port the system chose.
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// The job being carried out right now, if any.
    pub fn active_job(&self) -> Option<ActiveJob> {
        active_job(&self.state)
    }

    /// Request the same cooperative cancellation as the HTTP cancel button.
    /// Keep serving; this never aborts a worker or splits rename from journal.
    pub fn cancel_active_job(&self) -> bool {
        let active = self.state.jobs.active.lock().unwrap();
        if let Some((_, control)) = &*active {
            control.cancel.store(true, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    /// Stop the server.
    ///
    /// In this order, and the order is the point:
    ///
    /// 1. New jobs and hand-made changes are refused with 503. Whatever had
    ///    already passed the mutation gate is let finish first, so nothing
    ///    starts after this step.
    /// 2. A running job either refuses the shutdown ([`Shutdown::IfIdle`]) or
    ///    is asked to stop ([`Shutdown::CancelJob`]). It stops at the next
    ///    file boundary — `Control::check` sits between files, never inside
    ///    a move — so a `rename` and the journal row that records it are
    ///    never separated. The wait has no time limit: a deadline here would
    ///    be exactly the half-finished move the journal exists to prevent.
    /// 3. The job's thread is awaited until it has let go of the writer lock
    ///    and written its terminal state to `jobs`.
    /// 4. HTTP stops gracefully and the last reference to the state, with its
    ///    SQLite connection, is dropped.
    ///
    /// If the process is killed instead, the existing rule applies: the next
    /// start marks the job interrupted, and a half-done journal entry is left
    /// for `journal-reconcile`. Nothing on disk is ever redone automatically.
    pub async fn shutdown(self, mode: Shutdown) -> Result<(), ShutdownError> {
        let state = self.state.clone();
        state.closing.store(true, Ordering::SeqCst);
        // Every writer checks `closing` under this gate. Taking it once after
        // raising the flag waits out a change or job start that was already
        // inside; after that, no writer can get in. A blocking lock, so off
        // the async threads.
        let gate = state.clone();
        tokio::task::spawn_blocking(move || drop(gate.mutation.lock().unwrap()))
            .await
            .map_err(|e| ShutdownError::Failed(e.into()))?;
        pause_with_the_gate_closed().await;

        if let Some(job) = active_job(&state) {
            match mode {
                Shutdown::IfIdle => {
                    state.closing.store(false, Ordering::SeqCst);
                    return Err(ShutdownError::Busy {
                        server: Box::new(self),
                        job,
                    });
                }
                Shutdown::CancelJob => {
                    if let Some((_, control)) = &*state.jobs.active.lock().unwrap() {
                        control.cancel.store(true, Ordering::Relaxed);
                    }
                }
            }
        }
        // Awaited even when no job looked active: one that has just let go of
        // the writer may not yet have written how it ended.
        let worker = state.jobs.worker.lock().unwrap().take();
        if let Some(worker) = worker {
            // A panic inside the job is already caught and recorded by the
            // job itself; the handle can only fail if the runtime is going
            // away, and then there is nothing left to wait for.
            let _ = worker.handle.await;
        }

        let Server {
            state: own,
            stop,
            http,
            ..
        } = self;
        let _ = stop.send(());
        let served = http.await;
        drop(own);
        // The router's copy went with the HTTP task. A copy can still be
        // held briefly by a request handler that is unwinding; it closes the
        // connection when it lets go, which is all that matters.
        let _ = Arc::try_unwrap(state);
        match served {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(ShutdownError::Failed(e.into())),
            Err(e) => Err(ShutdownError::Failed(e.into())),
        }
    }
}

/// Test-only: hold the moment between closing the gate and stopping, which
/// in real life lasts as long as a job takes to reach a file boundary, so a
/// test can knock on the closed door without racing the job.
#[cfg(test)]
pub(crate) static SHUTDOWN_PAUSE_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

async fn pause_with_the_gate_closed() {
    #[cfg(test)]
    {
        let ms = SHUTDOWN_PAUSE_MS.load(Ordering::Relaxed);
        if ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
        }
    }
}

fn active_job(state: &AppState) -> Option<ActiveJob> {
    let id = state
        .jobs
        .active
        .lock()
        .unwrap()
        .as_ref()
        .map(|(id, _)| *id)?;
    let kind = state
        .jobs
        .worker
        .lock()
        .unwrap()
        .as_ref()
        .filter(|w| w.id == id)
        .map(|w| w.kind.clone())
        .unwrap_or_default();
    Some(ActiveJob { id, kind })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    fn http(addr: SocketAddr, method: &str, path: &str, body: &str) -> u16 {
        let mut s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        write!(
            s,
            "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out.split_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .unwrap_or(0)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_stopping_server_still_reads_but_takes_no_new_job_or_change() {
        let tmp = tempfile::tempdir().unwrap();
        let server = start(ServerConfig {
            db_path: tmp.path().join("photo-cleanup.db"),
            thumbs: tmp.path().join("thumbs"),
            quarantine: None,
            bind: "127.0.0.1:0".parse().unwrap(),
        })
        .await
        .unwrap();
        let addr = server.local_addr();
        SHUTDOWN_PAUSE_MS.store(1500, Ordering::Relaxed);
        let stopping = tokio::spawn(server.shutdown(Shutdown::CancelJob));
        // The flag is raised before the first await of the shutdown.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let codes = tokio::task::spawn_blocking(move || {
            [
                http(addr, "GET", "/api/status", ""),
                http(addr, "POST", "/api/jobs", r#"{"kind":"scan","params":{}}"#),
                http(addr, "PUT", "/api/settings", r#"{"theme":"dark"}"#),
            ]
        })
        .await
        .unwrap();
        SHUTDOWN_PAUSE_MS.store(0, Ordering::Relaxed);
        stopping.await.unwrap().unwrap();
        assert_eq!(codes, [200, 503, 503]);
        assert!(TcpStream::connect(addr).is_err());
    }
}
