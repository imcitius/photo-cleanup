//! Native lifecycle. Closing only hides the existing webview; exit is a
//! serialized server shutdown, never a forced process exit during a job.
use super::*;
use pc_desktop::geometry::Geometry;
use pc_desktop::instance::{Claim, Instance};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::{RunEvent, WindowEvent};

mod quit;

#[derive(Default)]
pub(super) struct Lifecycle {
    pub(super) quit: quit::Requests,
    allowed: AtomicBool,
    #[cfg(target_os = "macos")]
    native_reply_pending: AtomicBool,
    listener_stop: Arc<AtomicBool>,
    listener: Mutex<Option<std::thread::JoinHandle<()>>>,
    geometry_path: Mutex<Option<PathBuf>>,
    normal: Mutex<Option<Geometry>>,
}

pub(super) fn initialize(app: &AppHandle) -> anyhow::Result<bool> {
    let local = app.path().app_local_data_dir()?;
    #[cfg(debug_assertions)]
    let local = match std::env::var_os("PC_DESKTOP_TEST_APP_DATA") {
        Some(path) => {
            let path = PathBuf::from(path);
            anyhow::ensure!(
                path.is_absolute(),
                "PC_DESKTOP_TEST_APP_DATA must be absolute"
            );
            path
        }
        None => local,
    };
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let dirs = SystemDirs::new(local, exe_dir);
    let state = app.state::<Desktop>();
    // Portable must not write geometry/coordination files into the profile.
    let home = if dirs.portable_marker().is_some() {
        dirs.exe_dir.as_ref().unwrap().join(pc_desktop::DATA_SUBDIR)
    } else {
        dirs.app_local_data.clone()
    };
    *lock(&state.dirs) = Some(dirs);
    *lock(&state.lifecycle.geometry_path) = Some(home.join("window.json"));
    let replacement = std::env::var_os("PC_DESKTOP_REPLACEMENT").is_some();
    let claim = Instance::claim(&home, replacement, Duration::from_secs(60));
    let instance = match claim {
        Ok(Claim::Owner(instance)) => instance,
        Ok(Claim::Activated) => {
            finish_exit(app, 0);
            return Ok(false);
        }
        Err(e) => {
            rfd::MessageDialog::new()
                .set_title(WINDOW_TITLE)
                .set_description(format!("Cannot start Photo Cleanup: {e:#}"))
                .set_level(rfd::MessageLevel::Error)
                .show();
            finish_exit(app, 1);
            return Ok(false);
        }
    };
    let stop = state.lifecycle.listener_stop.clone();
    let handle = app.clone();
    *lock(&state.lifecycle.listener) = Some(std::thread::spawn(move || {
        while !stop.load(Ordering::Acquire) {
            if instance.activated(|| {
                let state = handle.state::<Desktop>();
                !state.lifecycle.quit.pending() && !state.lifecycle.allowed.load(Ordering::Acquire)
            }) {
                let app = handle.clone();
                let _ = handle.run_on_main_thread(move || show(&app));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        // Hold ownership through server shutdown, including restarts.
        drop(instance);
    }));
    Ok(true)
}

pub(super) fn ready(app: &AppHandle) -> tauri::Result<()> {
    #[cfg(target_os = "macos")]
    macos::install(app);
    restore(app);
    #[cfg(windows)]
    tray(app)?;
    // Terminal signals are coordinated too; SIGKILL/OS forced termination
    // cannot be intercepted and follow the interrupted/journal contract.
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            if let (Ok(mut term), Ok(mut interrupt)) = (
                signal(SignalKind::terminate()),
                signal(SignalKind::interrupt()),
            ) {
                loop {
                    let signal = tokio::select! {
                        signal = term.recv() => signal,
                        signal = interrupt.recv() => signal,
                    };
                    if signal.is_none() {
                        break;
                    }
                    request_exit(&handle, true);
                }
            }
        }
        #[cfg(windows)]
        while tokio::signal::ctrl_c().await.is_ok() {
            request_exit(&handle, true);
        }
    });
    Ok(())
}

pub(super) fn show(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Call only after the server is drained (or before it was started).
/// Route restarts through the same reply in case a native quit arrived
/// while the data-folder transfer held the serialization gate.
pub(super) fn finish_exit(app: &AppHandle, code: i32) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        handle
            .state::<Desktop>()
            .lifecycle
            .allowed
            .store(true, Ordering::Release);
        #[cfg(target_os = "macos")]
        if macos::reply(&handle, true) {
            return;
        }
        handle.exit(code);
    });
}

pub(super) fn event(app: &AppHandle, event: RunEvent) {
    match event {
        RunEvent::ExitRequested { api, code, .. } => {
            #[cfg(debug_assertions)]
            eprintln!(
                "desktop exit requested: code={code:?}, allowed={}",
                app.state::<Desktop>()
                    .lifecycle
                    .allowed
                    .load(Ordering::Acquire)
            );
            #[cfg(not(debug_assertions))]
            let _ = code;
            if !app
                .state::<Desktop>()
                .lifecycle
                .allowed
                .load(Ordering::Acquire)
            {
                api.prevent_exit();
                request_exit(app, false);
            }
        }
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => show(app),
        RunEvent::Exit => {
            save(app);
            super::stop(app);
            let state = app.state::<Desktop>();
            state.lifecycle.listener_stop.store(true, Ordering::Release);
            if let Some(listener) = lock(&state.lifecycle.listener).take() {
                let _ = listener.join();
            };
        }
        _ => {}
    }
}

pub(super) fn window_event(window: &tauri::Window, event: &WindowEvent) {
    if window.label() != WINDOW_LABEL {
        return;
    }
    let app = window.app_handle();
    match event {
        WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            save(app);
            let _ = window.hide();
        }
        WindowEvent::Moved(_) | WindowEvent::Resized(_) => remember(app),
        _ => {}
    }
}

fn request_exit(app: &AppHandle, confirmed: bool) {
    let state = app.state::<Desktop>();
    let Some(mut request) = state.lifecycle.quit.request(confirmed) else {
        return;
    };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<Desktop>();
        // A folder transfer holds this through commit/spawn/recovery. Quit
        // must not observe its temporarily absent server as idle.
        let _change = state.changing.lock().await;
        let server = lock(&state.server).take();
        let result = if let Some(server) = server {
            match server.shutdown(Shutdown::IfIdle).await {
                Ok(()) => Ok(()),
                Err(ShutdownError::Busy { server, job }) => {
                    #[cfg(debug_assertions)]
                    eprintln!("desktop quit: busy job {}, confirmed={confirmed}", job.id);
                    *lock(&state.server) = Some(*server);
                    if !quit::decide(&mut request, confirm(&app, &job)).await
                        && cancel_exit(&app).await
                    {
                        return;
                    }
                    show(&app);
                    if let Some(w) = app.get_webview_window(WINDOW_LABEL) {
                        let _ = w.set_title(pc_core::tr!(
                            "Photo Cleanup — остановка работы…",
                            "Photo Cleanup — stopping work…"
                        ));
                    }
                    let server = lock(&state.server)
                        .take()
                        .expect("shutdown owns the server");
                    server.shutdown(Shutdown::CancelJob).await
                }
                Err(e) => Err(e),
            }
        } else {
            Ok(())
        };
        if let Err(e) = result {
            eprintln!("photo-cleanup-desktop: shutdown: {e}");
        }
        finish_exit(&app, 0);
    });
}

async fn cancel_exit(app: &AppHandle) -> bool {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        // Serialize the native reply with should_terminate. A new native
        // request must not accidentally receive the previous dialog's NO.
        let cancelled = handle.state::<Desktop>().lifecycle.quit.cancel();
        if cancelled {
            #[cfg(target_os = "macos")]
            macos::reply(&handle, false);
        }
        let _ = tx.send(cancelled);
    });
    rx.await.unwrap_or(true)
}

async fn confirm(app: &AppHandle, job: &pc_api::ActiveJob) -> bool {
    let text = pc_core::tf!(
        "Идёт задача №{0} ({1}). Остановить работу и выйти? Текущая операция с файлом и запись журнала завершатся. Продолжение работы оставит задачу запущенной.",
        "Job #{0} ({1}) is running. Stop work and quit? The current file operation and journal write will finish first. Continuing leaves the job running.", job.id, job.kind);
    let stop = pc_core::tr!("Остановить и выйти", "Stop and quit").to_string();
    let keep = pc_core::tr!("Продолжить работу", "Continue working").to_string();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let handle = app.clone();
    if app
        .run_on_main_thread(move || {
            show(&handle);
            let mut dialog = rfd::AsyncMessageDialog::new()
                .set_title(WINDOW_TITLE)
                .set_description(text)
                .set_level(rfd::MessageLevel::Warning)
                // The safe choice is the first/default button.
                .set_buttons(rfd::MessageButtons::OkCancelCustom(keep, stop));
            if let Some(w) = handle.get_webview_window(WINDOW_LABEL) {
                dialog = dialog.set_parent(&w);
            }
            let _ = tx.send(dialog.show());
        })
        .is_err()
    {
        return false;
    }
    let Ok(dialog) = rx.await else { return false };
    let result = dialog.await;
    #[cfg(debug_assertions)]
    eprintln!("desktop quit dialog: {result:?}");
    matches!(result, rfd::MessageDialogResult::Custom(s) if s == pc_core::tr!("Остановить и выйти", "Stop and quit"))
}

#[cfg(windows)]
fn tray(app: &AppHandle) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
    let open = MenuItem::with_id(
        app,
        "open",
        pc_core::tr!("Открыть", "Open"),
        true,
        None::<&str>,
    )?;
    let stop = MenuItem::with_id(
        app,
        "stop",
        pc_core::tr!("Остановить работу", "Stop work"),
        true,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(
        app,
        "quit",
        pc_core::tr!("Выйти", "Quit"),
        true,
        None::<&str>,
    )?;
    let menu = Menu::with_items(app, &[&open, &stop, &quit])?;
    let mut tray = TrayIconBuilder::with_id("desktop")
        .tooltip(WINDOW_TITLE)
        .menu(&menu)
        .on_menu_event(|app, e| match e.id.as_ref() {
            "open" => show(app),
            "stop" => {
                if let Some(s) = lock(&app.state::<Desktop>().server).as_ref() {
                    s.cancel_active_job();
                }
            }
            "quit" => request_exit(app, false),
            _ => {}
        })
        .on_tray_icon_event(|tray, e| {
            if matches!(
                e,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                show(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

fn remember(app: &AppHandle) {
    let Some(w) = app.get_webview_window(WINDOW_LABEL) else {
        return;
    };
    if w.is_minimized().unwrap_or(true)
        || w.is_maximized().unwrap_or(true)
        || w.is_fullscreen().unwrap_or(true)
    {
        return;
    }
    if let (Ok(pos), Ok(size)) = (w.outer_position(), w.outer_size()) {
        *lock(&app.state::<Desktop>().lifecycle.normal) = Some(Geometry {
            x: pos.x,
            y: pos.y,
            width: size.width,
            height: size.height,
            maximized: false,
        });
    }
}

fn save(app: &AppHandle) {
    remember(app);
    let state = app.state::<Desktop>();
    let Some(mut geometry) = *lock(&state.lifecycle.normal) else {
        return;
    };
    if let Some(w) = app.get_webview_window(WINDOW_LABEL) {
        geometry.maximized = w.is_maximized().unwrap_or(false);
    }
    if let Some(path) = lock(&state.lifecycle.geometry_path).as_ref() {
        if let Err(e) = geometry.save(path) {
            eprintln!("photo-cleanup-desktop: window geometry: {e}");
        }
    };
}

fn restore(app: &AppHandle) {
    let state = app.state::<Desktop>();
    let Some(w) = app.get_webview_window(WINDOW_LABEL) else {
        return;
    };
    let saved = lock(&state.lifecycle.geometry_path)
        .as_ref()
        .and_then(|p| Geometry::read(p));
    let screens: Vec<_> = w
        .primary_monitor()
        .ok()
        .flatten()
        .into_iter()
        .chain(w.available_monitors().unwrap_or_default())
        .map(|m| {
            let a = m.work_area();
            Geometry {
                x: a.position.x,
                y: a.position.y,
                width: a.size.width,
                height: a.size.height,
                maximized: false,
            }
        })
        .collect();
    let scale = w.scale_factor().unwrap_or(1.0);
    let chrome = w
        .outer_size()
        .ok()
        .zip(w.inner_size().ok())
        .map(|(outer, inner)| {
            (
                outer.width.saturating_sub(inner.width),
                outer.height.saturating_sub(inner.height),
            )
        })
        .unwrap_or((0, 0));
    let min_width = (WINDOW_MIN_SIZE.0 * scale) as u32;
    let min_height = (WINDOW_MIN_SIZE.1 * scale) as u32;
    let saved = saved.map(|g| Geometry {
        width: g.width.max(min_width.saturating_add(chrome.0)),
        height: g.height.max(min_height.saturating_add(chrome.1)),
        ..g
    });
    if let Some(g) = saved.and_then(|g| g.fit(&screens)) {
        let width = g.width.saturating_sub(chrome.0).max(1);
        let height = g.height.saturating_sub(chrome.1).max(1);
        // A disconnected/smaller screen can be below the usual minimum.
        // Let the fitted rectangle win, including the native title bar.
        let _ = w.set_min_size(Some(tauri::PhysicalSize::new(
            min_width.min(width),
            min_height.min(height),
        )));
        let _ = w.set_size(tauri::PhysicalSize::new(width, height));
        let _ = w.set_position(tauri::PhysicalPosition::new(g.x, g.y));
        *lock(&state.lifecycle.normal) = Some(g);
        if g.maximized {
            let _ = w.maximize();
        }
    } else {
        remember(app);
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, Sel};
    use objc2::{msg_send, sel};
    use std::sync::OnceLock;

    static APP: OnceLock<AppHandle> = OnceLock::new();

    extern "C-unwind" fn should_terminate(_: &AnyObject, _: Sel, _: &AnyObject) -> usize {
        if let Some(app) = APP.get() {
            if app
                .state::<Desktop>()
                .lifecycle
                .allowed
                .load(Ordering::Acquire)
            {
                return 1; // NSTerminateNow
            }
            app.state::<Desktop>()
                .lifecycle
                .native_reply_pending
                .store(true, Ordering::Release);
            request_exit(app, false);
            return 2; // NSTerminateLater: reply after confirmation and drain.
        }
        0 // No app state: fail closed.
    }

    pub(super) fn reply(app: &AppHandle, terminate: bool) -> bool {
        assert!(objc2::MainThreadMarker::new().is_some());
        if !app
            .state::<Desktop>()
            .lifecycle
            .native_reply_pending
            .swap(false, Ordering::AcqRel)
        {
            return false;
        }
        #[cfg(debug_assertions)]
        eprintln!("desktop native termination reply: {terminate}");
        // SAFETY: NSApplication is live and this runs on its main thread.
        // A positive reply invokes the inherited applicationWillTerminate
        // callback, including RunEvent::Exit and instance-lock release.
        unsafe {
            let ns: Retained<AnyObject> =
                msg_send![AnyClass::get(c"NSApplication").unwrap(), sharedApplication];
            let _: () = msg_send![&*ns, replyToApplicationShouldTerminate: terminate];
        }
        true
    }

    pub(super) fn install(app: &AppHandle) {
        assert!(objc2::MainThreadMarker::new().is_some());
        let _ = APP.set(app.clone());
        // Tao 0.35 only observes applicationWillTerminate. AppKit's native
        // Quit/Dock action therefore bypasses RunEvent::ExitRequested. Add
        // the earlier veto to this delegate instance, inheriting all Tao
        // methods (especially reopen/openURLs) and adding no ivars.
        // SAFETY: installation is on the main thread before user events;
        // NSApplication and its delegate are live retained objects. The
        // subclass has identical layout and the method has the AppKit ABI:
        // NSUInteger (NSApplicationTerminateReply), self, SEL, NSApplication*.
        unsafe {
            let ns: Retained<AnyObject> =
                msg_send![AnyClass::get(c"NSApplication").unwrap(), sharedApplication];
            let delegate: Retained<AnyObject> = msg_send![&*ns, delegate];
            let mut subclass =
                ClassBuilder::new(c"PhotoCleanupLifecycleDelegate", delegate.class())
                    .expect("lifecycle delegate installed once");
            subclass.add_method(
                sel!(applicationShouldTerminate:),
                should_terminate as extern "C-unwind" fn(_, _, _) -> usize,
            );
            AnyObject::set_class(&delegate, subclass.register());
        }
    }
}
