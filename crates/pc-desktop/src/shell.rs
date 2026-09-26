//! The window: Tauri on Windows and macOS.
//!
//! Start-up, in this order, and nothing is shown until it is decided what to
//! show:
//!
//! 1. The data directory is resolved and its database opened
//!    (`pc_desktop::resolve` → `prepare`).
//! 2. `pc-api` starts on `127.0.0.1:0`. It returns once it is ready —
//!    migrated, listening — so the window never loads a page that races the
//!    server.
//! 3. The choice of data directory is confirmed (`confirm_started`).
//! 4. The window opens the shared interface from that server.
//!
//! Any failure on the way replaces step 4 with the bundled error page. There
//! is no console to print to.
//!
//! The shell carries out no business operation: everything that touches the
//! archive goes through the HTTP API, with its preview token and writer lock,
//! exactly as in a browser. Its own commands (`pc_desktop::INTERFACE_COMMANDS`)
//! are the native folder dialog, showing the data folder, and changing it.
//! They are granted at run time, once the port is known, to the window `main`
//! showing exactly `http://127.0.0.1:<port>` — no wildcard, so no other page
//! on any other local server reaches them. The bundled error page gets only
//! "back to the previous folder" and "try again".

mod lifecycle;

use pc_api::{Server, ServerConfig, Shutdown, ShutdownError};
use pc_desktop::{
    confirm_started, copy_data, interface_origin, is_bundled_page, is_server_page, parse_args,
    permission, prepare, preview_move, resolve, revert_to_previous, server_url, switch_to_existing,
    ChangeAction, DataLayout, DesktopInfo, MovePreview, Source, StartupError, StartupReport,
    SystemDirs, ERROR_PAGE, ERROR_PAGE_COMMANDS, INTERFACE_COMMANDS, SERVER_BIND, WINDOW_LABEL,
    WINDOW_MIN_SIZE, WINDOW_SIZE, WINDOW_TITLE,
};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::ipc::CapabilityBuilder;
use tauri::webview::NewWindowResponse;
use tauri::{AppHandle, Manager, State, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

/// What this launch runs on. Managed from the start, filled in as start-up
/// gets that far.
struct Desktop {
    /// The system directories, once known. The error page's "back to the
    /// previous folder" needs them even when nothing else started.
    dirs: Mutex<Option<SystemDirs>>,
    /// The data folder in use, and how it was chosen.
    data: Mutex<Option<(Source, DataLayout)>>,
    server: Mutex<Option<Server>>,
    /// The server's address. Shared with the window's navigation rule, which
    /// must follow the server if it has to be restarted on another port.
    addr: Arc<Mutex<Option<SocketAddr>>>,
    /// One data-folder change at a time; a second click is refused, not
    /// queued behind a server that is already stopping.
    changing: tauri::async_runtime::Mutex<()>,
    lifecycle: lifecycle::Lifecycle,
}

impl Desktop {
    fn new() -> Self {
        Self {
            dirs: Mutex::new(None),
            data: Mutex::new(None),
            server: Mutex::new(None),
            addr: Arc::new(Mutex::new(None)),
            changing: tauri::async_runtime::Mutex::new(()),
            lifecycle: lifecycle::Lifecycle::default(),
        }
    }

    fn current(&self) -> Result<(SystemDirs, Source, DataLayout), String> {
        let dirs = lock(&self.dirs).clone();
        let data = lock(&self.data).clone();
        match (dirs, data) {
            (Some(d), Some((s, l))) => Ok((d, s, l)),
            _ => Err("the data folder is not open".into()),
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn run() {
    let args = parse_args(std::env::args_os().skip(1));
    let app = tauri::Builder::default()
        .manage(Desktop::new())
        .invoke_handler(tauri::generate_handler![
            desktop_info,
            pick_folder,
            reveal_data_dir,
            preview_data_dir_change,
            change_data_dir,
            revert_data_dir,
            restart_app,
        ])
        .setup(move |app| {
            let handle = app.handle();
            if !lifecycle::initialize(handle)? {
                return Ok(());
            }
            let started = match &args {
                Ok(data_dir) => launch(handle, data_dir.as_deref()),
                Err(e) => {
                    let e = e.clone();
                    Err(StartupReport::other("arguments", move || e.clone()))
                }
            };
            match started {
                Ok(server) => {
                    let addr = server.local_addr();
                    let state = handle.state::<Desktop>();
                    *lock(&state.server) = Some(server);
                    *lock(&state.addr) = Some(addr);
                    grant_interface(handle, addr)?;
                    open_interface(handle, addr, state.addr.clone())?;
                }
                Err(report) => {
                    grant_error_page(handle)?;
                    open_error(handle, &report)?;
                }
            }
            lifecycle::ready(handle)?;
            Ok(())
        })
        .on_window_event(lifecycle::window_event)
        .build(tauri::generate_context!());
    let app = match app {
        Ok(app) => app,
        // No window could be made at all — on Windows most likely a missing
        // WebView2 runtime. Nothing else is left to show it with.
        Err(e) => {
            eprintln!("photo-cleanup-desktop: cannot start the window: {e}");
            std::process::exit(1);
        }
    };
    app.run(|handle, event| {
        lifecycle::event(handle, event);
    });
}

/// Steps 1–3: data directory, server, confirmation.
fn launch(app: &AppHandle, data_dir: Option<&Path>) -> Result<Server, StartupReport> {
    let state = app.state::<Desktop>();
    let dirs = lock(&state.dirs)
        .clone()
        .expect("instance initialized before launch");
    let resolved = resolve(&dirs, data_dir).map_err(|e| StartupReport::from_startup(&e))?;
    // A database left beside the executable by the batch-file distribution
    // (`resolved.legacy_dir`) is not picked up here. First launch uses the
    // system directory; the settings screen offers the old folder as "switch
    // to the existing database" (`desktop_info().legacy_dir`).
    let previous = resolved.previous.clone();
    let prepared = prepare(&dirs, &resolved).map_err(|e| StartupReport::from_startup(&e))?;
    let server = tauri::async_runtime::block_on(start_server(&prepared.layout, SERVER_BIND));
    let server = server.map_err(|e| {
        let db = prepared.layout.db.display().to_string();
        let mut report = StartupReport::other("server", move || {
            pc_core::tf!(
                "сервер не запустился на базе {0}: {1}",
                "the server did not start on the database {0}: {1}",
                db,
                e
            )
        });
        // A folder just moved to that does not start: the error page offers
        // the one it came from.
        if let Some(p) = previous {
            report.detail["previous"] = serde_json::to_value(p).unwrap_or_default();
        }
        report
    })?;
    if let Err(e) = confirm_started(&dirs, &prepared) {
        // Nothing has run yet; the server is stopped before the report so the
        // database is not left open behind an error page.
        let _ = tauri::async_runtime::block_on(server.shutdown(Shutdown::CancelJob));
        return Err(StartupReport::from_startup(&e));
    }
    *lock(&state.data) = Some((prepared.source, prepared.layout.clone()));
    Ok(server)
}

async fn start_server(layout: &DataLayout, bind: SocketAddr) -> Result<Server, String> {
    let config = ServerConfig {
        db_path: layout.db.clone(),
        thumbs: layout.thumbs.clone(),
        // Each file's own filesystem, as in the command line: a move must stay
        // a rename on the same device.
        quarantine: None,
        bind,
    };
    pc_api::start(config).await.map_err(|e| format!("{e:#}"))
}

/// The interface's commands, for exactly this server's origin in `main`.
fn grant_interface(app: &AppHandle, addr: SocketAddr) -> tauri::Result<()> {
    app.add_capability(interface_capability(addr))
}

fn interface_capability(addr: SocketAddr) -> CapabilityBuilder {
    let mut cap = CapabilityBuilder::new(format!("interface-{}", addr.port()))
        .window(WINDOW_LABEL)
        .remote(interface_origin(addr))
        // Not for the bundled pages: the error page has its own, smaller set.
        .local(false);
    for c in INTERFACE_COMMANDS {
        cap = cap.permission(permission(c));
    }
    cap
}

/// The error page's commands, for the bundled origin only.
fn grant_error_page(app: &AppHandle) -> tauri::Result<()> {
    app.add_capability(error_capability())
}

fn error_capability() -> CapabilityBuilder {
    let mut cap = CapabilityBuilder::new("error-page")
        .window(WINDOW_LABEL)
        .local(true);
    for c in ERROR_PAGE_COMMANDS {
        cap = cap.permission(permission(c));
    }
    cap
}

/// Step 4: the shared interface, from this launch's server only.
fn open_interface(
    app: &AppHandle,
    addr: SocketAddr,
    allowed: Arc<Mutex<Option<SocketAddr>>>,
) -> tauri::Result<()> {
    let url = Url::parse(&server_url(addr)).map_err(|e| tauri::Error::Anyhow(e.into()))?;
    WebviewWindowBuilder::new(app, WINDOW_LABEL, WebviewUrl::External(url))
        .title(WINDOW_TITLE)
        .inner_size(WINDOW_SIZE.0, WINDOW_SIZE.1)
        .min_inner_size(WINDOW_MIN_SIZE.0, WINDOW_MIN_SIZE.1)
        .center()
        // Everything else — another port, an external site, a `file:` URL —
        // is refused, not opened elsewhere. The interface links only within
        // itself (`#…`); an external link would be a new need, to be opened
        // in the system browser through a vetted opener, not a shell command
        // with a URL in it. The address is read at each navigation: after a
        // failed data-folder change the server may be back on another port.
        .on_navigation(move |url| {
            let Some(addr) = *lock(&allowed) else {
                return false;
            };
            is_server_page(
                url.scheme(),
                url.host_str(),
                url.port_or_known_default(),
                addr,
            )
        })
        .on_new_window(|_, _| NewWindowResponse::Deny)
        .build()?;
    Ok(())
}

/// The bundled error page, with the report injected before it loads.
fn open_error(app: &AppHandle, report: &StartupReport) -> tauri::Result<()> {
    // Invisible on Windows (no console), but whoever started the app from a
    // terminal — or a smoke test — reads the same report there.
    eprintln!("photo-cleanup-desktop: {}", report.en);
    WebviewWindowBuilder::new(app, WINDOW_LABEL, WebviewUrl::App(ERROR_PAGE.into()))
        .title(WINDOW_TITLE)
        .inner_size(760.0, 560.0)
        .center()
        .initialization_script(report.init_script())
        .on_navigation(|url| is_bundled_page(url.scheme(), url.host_str()))
        .on_new_window(|_, _| NewWindowResponse::Deny)
        .build()?;
    Ok(())
}

/// The app is exiting: stop the server the way `serve` does on Ctrl+C.
///
/// A running job is asked to stop at the next file boundary and waited for,
/// with no time limit, so a move and its journal row are never separated.
/// This is also a last-resort drain on event-loop exit.
fn stop(app: &AppHandle) {
    let Some(state) = app.try_state::<Desktop>() else {
        return;
    };
    let server = lock(&state.server).take();
    if let Some(server) = server {
        if let Err(e) = tauri::async_runtime::block_on(server.shutdown(Shutdown::CancelJob)) {
            eprintln!("photo-cleanup-desktop: stopping the server: {e}");
        }
    }
}

// ---- Commands ------------------------------------------------------------
//
// Errors are returned as text in the interface's language (`pc_core::lang`
// follows the settings). They are shown, never parsed.

#[tauri::command]
async fn desktop_info(state: State<'_, Desktop>) -> Result<DesktopInfo, String> {
    let (dirs, source, layout) = state.current()?;
    // The cache is walked for its size: off the async threads.
    tauri::async_runtime::spawn_blocking(move || pc_desktop::desktop_info(&dirs, source, &layout))
        .await
        .map_err(|e| e.to_string())
}

/// The native folder dialog. `None` is a cancel: nothing changes.
#[tauri::command]
async fn pick_folder(
    window: WebviewWindow,
    initial: Option<String>,
    title: Option<String>,
) -> Result<Option<String>, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let parent = window.clone();
    // The dialog is created on the main thread (macOS requires it) and
    // awaited here, so the window keeps painting while it is open.
    window
        .run_on_main_thread(move || {
            let mut d = rfd::AsyncFileDialog::new().set_parent(&parent);
            if let Some(t) = title.filter(|t| !t.is_empty()) {
                d = d.set_title(t.chars().take(200).collect::<String>());
            }
            if let Some(dir) = initial.map(PathBuf::from).filter(|p| p.is_dir()) {
                d = d.set_directory(dir);
            }
            let _ = tx.send(d.pick_folder());
        })
        .map_err(|e| e.to_string())?;
    let picking = tauri::async_runtime::spawn_blocking(move || rx.recv())
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    Ok(picking
        .await
        .map(|f| f.path().to_string_lossy().into_owned()))
}

/// Show the data folder in Finder or Explorer. No argument: it can open
/// nothing else.
#[tauri::command]
async fn reveal_data_dir(state: State<'_, Desktop>) -> Result<(), String> {
    let (_, _, layout) = state.current()?;
    // The path is ours (from the bootstrap), passed as one argument with no
    // shell in between.
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("/usr/bin/open");
    #[cfg(windows)]
    let mut cmd = std::process::Command::new("explorer.exe");
    cmd.arg(&layout.dir)
        .spawn()
        .map(drop)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn preview_data_dir_change(
    state: State<'_, Desktop>,
    target: String,
) -> Result<MovePreview, String> {
    let (_, source, layout) = state.current()?;
    tauri::async_runtime::spawn_blocking(move || preview_move(&layout, source, Path::new(&target)))
        .await
        .map_err(|e| e.to_string())
}

/// Change the data folder: stop the server, copy and verify (or switch to
/// the database already there), record the choice, restart.
///
/// Returns only on failure, after the server is back on the old folder; on
/// success the app restarts and the page goes with it. The old folder is
/// never deleted, and stays the chosen one until the very last step.
#[tauri::command]
async fn change_data_dir(
    app: AppHandle,
    state: State<'_, Desktop>,
    target: String,
    action: ChangeAction,
) -> Result<(), String> {
    let Ok(_one) = state.changing.try_lock() else {
        return Err(pc_core::tr!(
            "смена папки данных уже идёт",
            "the data folder is already being changed"
        )
        .into());
    };
    if state.lifecycle.quit.pending() {
        return Err(pc_core::tr!(
            "приложение готовится к выходу",
            "the application is preparing to quit"
        )
        .into());
    }
    let (dirs, source, layout) = state.current()?;
    let target = PathBuf::from(target);

    // Everything that can be refused is refused while the server still runs.
    match action {
        ChangeAction::Copy => {
            let (l, t) = (layout.clone(), target.clone());
            let p = tauri::async_runtime::spawn_blocking(move || preview_move(&l, source, &t))
                .await
                .map_err(|e| e.to_string())?;
            if !p.blockers.is_empty() {
                let list: Vec<String> = p.blockers.iter().map(ToString::to_string).collect();
                return Err(list.join("; "));
            }
        }
        ChangeAction::UseExisting => {
            if !target.join(pc_desktop::DB_FILE).is_file() {
                return Err(StartupError::DataUnavailable {
                    source: Source::Custom,
                    dir: target,
                    why: pc_desktop::Unavailable::NoDatabase,
                    previous: None,
                }
                .to_string());
            }
        }
    }

    // Stop writing. A running job is not cancelled from here: the operator
    // stops it in the interface (or waits), then asks again.
    let Some(server) = lock(&state.server).take() else {
        return Err("the server is not running".into());
    };
    let old_port = server.local_addr().port();
    match server.shutdown(Shutdown::IfIdle).await {
        Ok(()) => {}
        Err(ShutdownError::Busy { server, job }) => {
            *lock(&state.server) = Some(*server);
            return Err(pc_core::tf!(
                "идёт задача «{0}»: дождитесь её конца или остановите её, затем повторите",
                "a job is running ({0}): wait for it or stop it, then try again",
                job.kind
            ));
        }
        Err(ShutdownError::Failed(e)) => {
            let e = format!("{e:#}");
            recover(&app, &state, &layout, old_port).await;
            return Err(e);
        }
    }

    let (d, l, t) = (dirs.clone(), layout.clone(), target.clone());
    let done = tauri::async_runtime::spawn_blocking(move || match action {
        ChangeAction::Copy => copy_data(&l, source, &t, pc_core::disk::available_space)
            .and_then(|copied| copied.commit(&d, source)),
        ChangeAction::UseExisting => switch_to_existing(&d, source, &t),
    })
    .await;
    match done {
        Ok(Ok(())) => {
            // The new folder is recorded with the old one as `previous`; the
            // next launch confirms it, or offers the old one back.
            match pc_desktop::restart_or_restore(&dirs, spawn_replacement) {
                Ok(()) => {
                    lifecycle::finish_exit(&app, 0);
                    Ok(())
                }
                Err(e) => {
                    recover(&app, &state, &layout, old_port).await;
                    Err(e)
                }
            }
        }
        Ok(Err(e)) => {
            let e = e.to_string();
            recover(&app, &state, &layout, old_port).await;
            Err(e)
        }
        Err(e) => {
            let e = e.to_string();
            recover(&app, &state, &layout, old_port).await;
            Err(e)
        }
    }
}

/// Unlike Tauri's fire-and-forget restart, a failed spawn must leave this
/// process alive so it can restore the old server and report the failure.
fn spawn_replacement() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    spawn_executable(&exe, std::env::args_os().skip(1))
}

fn spawn_executable(
    exe: &Path,
    args: impl IntoIterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    std::process::Command::new(exe)
        .args(args)
        // The child waits for our instance lock, without activating us.
        .env("PC_DESKTOP_REPLACEMENT", "1")
        .spawn()
        .map(drop)
        .map_err(|e| format!("cannot restart {}: {e}", exe.display()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_acl_grants_only_the_exact_origin_window_and_command_set() {
        use super::*;
        use tauri::ipc::Origin;
        let mut context: tauri::Context<tauri::Wry> = tauri::generate_context!();
        let authority = context.runtime_authority_mut();
        let addr = "127.0.0.1:51234".parse().unwrap();
        authority
            .add_capability(interface_capability(addr))
            .unwrap();
        authority.add_capability(error_capability()).unwrap();
        let remote = |s: &str| Origin::Remote {
            url: Url::parse(s).unwrap(),
        };
        for command in INTERFACE_COMMANDS {
            assert!(
                authority
                    .resolve_access(command, "main", "main", &remote("http://127.0.0.1:51234/"))
                    .is_some(),
                "{command}"
            );
            for url in [
                "http://127.0.0.1:51235/",
                "http://localhost:51234/",
                "https://127.0.0.1:51234/",
                "https://example.com/",
            ] {
                assert!(
                    authority
                        .resolve_access(command, "main", "main", &remote(url))
                        .is_none(),
                    "{command} {url}"
                );
            }
            assert!(authority
                .resolve_access(
                    command,
                    "other",
                    "other",
                    &remote("http://127.0.0.1:51234/")
                )
                .is_none());
            assert!(authority
                .resolve_access(command, "main", "main", &Origin::Local)
                .is_none());
        }
        for command in ERROR_PAGE_COMMANDS {
            assert!(authority
                .resolve_access(command, "main", "main", &Origin::Local)
                .is_some());
            assert!(authority
                .resolve_access(command, "main", "main", &remote("http://127.0.0.1:51234/"))
                .is_none());
        }
        assert!(authority
            .resolve_access(
                "plugin:fs|read_file",
                "main",
                "main",
                &remote("http://127.0.0.1:51234/")
            )
            .is_none());
    }

    #[test]
    fn a_missing_restart_binary_is_reported_without_exiting() {
        let tmp = tempfile::tempdir().unwrap();
        let error = super::spawn_executable(&tmp.path().join("missing"), []).unwrap_err();
        assert!(error.contains("cannot restart"));
    }
}

/// Bring the server back on the old folder after a change that did not
/// happen. Same port if it is still free, so the page stays where it is;
/// otherwise a new one, a capability for it, and the window follows. If the
/// server cannot start at all, the app restarts — the bootstrap still names
/// the old folder, so that is what it opens.
async fn recover(app: &AppHandle, state: &Desktop, layout: &DataLayout, old_port: u16) {
    let same = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), old_port);
    let server = match start_server(layout, same).await {
        Ok(s) => Ok(s),
        Err(_) => start_server(layout, SERVER_BIND).await,
    };
    let server = match server {
        Ok(s) => s,
        Err(e) => {
            eprintln!("photo-cleanup-desktop: restarting the server: {e}");
            if let Err(e) = restart_app(app.clone()) {
                eprintln!("photo-cleanup-desktop: {e}");
            }
            return;
        }
    };
    let addr = server.local_addr();
    *lock(&state.server) = Some(server);
    *lock(&state.addr) = Some(addr);
    if addr.port() != old_port {
        let moved = grant_interface(app, addr).is_ok()
            && Url::parse(&server_url(addr))
                .ok()
                .and_then(|url| {
                    app.get_webview_window(WINDOW_LABEL)
                        .map(|w| w.navigate(url).is_ok())
                })
                .unwrap_or(false);
        if !moved {
            if let Err(e) = restart_app(app.clone()) {
                eprintln!("photo-cleanup-desktop: {e}");
            }
        }
    }
}

/// The error page's "back to the previous folder": the one a failed move
/// came from. Records it and restarts.
#[tauri::command]
async fn revert_data_dir(app: AppHandle, state: State<'_, Desktop>) -> Result<(), String> {
    let Some(dirs) = lock(&state.dirs).clone() else {
        return Err("the app folder is not known".into());
    };
    tauri::async_runtime::spawn_blocking(move || {
        let r = revert_to_previous(&dirs)?;
        prepare(&dirs, &r).map(drop)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e: StartupError| e.to_string())?;
    restart_app(app)
}

/// The error page's "try again".
#[tauri::command]
fn restart_app(app: AppHandle) -> Result<(), String> {
    spawn_replacement()?;
    lifecycle::finish_exit(&app, 0);
    Ok(())
}
