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
//! The shell carries out no business operation. It registers no commands and
//! grants no capability: everything that touches the archive goes through the
//! HTTP API, with its preview token and writer lock, exactly as in a browser.
//! The native folder picker and the data-folder commands (el-646) will add a
//! capability for this one window and this one origin at run time, once the
//! port is known.

use pc_api::{Server, ServerConfig, Shutdown};
use pc_desktop::{
    confirm_started, is_bundled_page, is_server_page, parse_args, prepare, resolve, server_url,
    StartupReport, SystemDirs, ERROR_PAGE, SERVER_BIND, WINDOW_LABEL, WINDOW_MIN_SIZE, WINDOW_SIZE,
    WINDOW_TITLE,
};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::webview::NewWindowResponse;
use tauri::{AppHandle, Manager, RunEvent, Url, WebviewUrl, WebviewWindowBuilder};

/// The server of this launch, until the app exits.
struct Running(Mutex<Option<Server>>);

pub fn run() {
    let args = parse_args(std::env::args_os().skip(1));
    let app = tauri::Builder::default()
        .setup(move |app| {
            let handle = app.handle();
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
                    app.manage(Running(Mutex::new(Some(server))));
                    open_interface(handle, addr)?;
                }
                Err(report) => open_error(handle, &report)?,
            }
            Ok(())
        })
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
        if let RunEvent::Exit = event {
            stop(handle);
        }
    });
}

/// Steps 1–3: data directory, server, confirmation.
fn launch(app: &AppHandle, data_dir: Option<&Path>) -> Result<Server, StartupReport> {
    let local = app.path().app_local_data_dir().map_err(|e| {
        let e = e.to_string();
        StartupReport::other("app_data_dir", move || {
            pc_core::tf!(
                "не найти папку приложения в профиле пользователя: {0}",
                "cannot find the app folder in the user profile: {0}",
                e
            )
        })
    })?;
    let exe_dir: Option<PathBuf> = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let dirs = SystemDirs::new(local, exe_dir);
    let resolved = resolve(&dirs, data_dir).map_err(|e| StartupReport::from_startup(&e))?;
    // A database left beside the executable by the batch-file distribution
    // (`resolved.legacy_dir`) is not picked up here: offering it needs the
    // data-folder screen (el-646). Until then first launch uses the system
    // directory, and the old folder stays exactly as it was.
    let prepared = prepare(&dirs, &resolved).map_err(|e| StartupReport::from_startup(&e))?;
    let config = ServerConfig {
        db_path: prepared.layout.db.clone(),
        thumbs: prepared.layout.thumbs.clone(),
        // Each file's own filesystem, as in the command line: a move must stay
        // a rename on the same device.
        quarantine: None,
        bind: SERVER_BIND,
    };
    let server = tauri::async_runtime::block_on(pc_api::start(config)).map_err(|e| {
        let e = format!("{e:#}");
        let db = prepared.layout.db.display().to_string();
        StartupReport::other("server", move || {
            pc_core::tf!(
                "сервер не запустился на базе {0}: {1}",
                "the server did not start on the database {0}: {1}",
                db,
                e
            )
        })
    })?;
    if let Err(e) = confirm_started(&dirs, &prepared) {
        // Nothing has run yet; the server is stopped before the report so the
        // database is not left open behind an error page.
        let _ = tauri::async_runtime::block_on(server.shutdown(Shutdown::CancelJob));
        return Err(StartupReport::from_startup(&e));
    }
    Ok(server)
}

/// Step 4: the shared interface, from this launch's server only.
fn open_interface(app: &AppHandle, addr: SocketAddr) -> tauri::Result<()> {
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
        // with a URL in it.
        .on_navigation(move |url| {
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
        .inner_size(760.0, 520.0)
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
/// Asking the operator first, and keeping jobs running with the window
/// closed, is the lifecycle task (el-zsj).
fn stop(app: &AppHandle) {
    let Some(running) = app.try_state::<Running>() else {
        return;
    };
    let server = running.0.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(server) = server {
        if let Err(e) = tauri::async_runtime::block_on(server.shutdown(Shutdown::CancelJob)) {
            eprintln!("photo-cleanup-desktop: stopping the server: {e}");
        }
    }
}
