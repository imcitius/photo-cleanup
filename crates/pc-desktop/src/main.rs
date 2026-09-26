// No console window behind the app on Windows: it is started by double-click,
// and every start-up failure is shown in the window instead.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(any(windows, target_os = "macos"))]
mod shell;

#[cfg(any(windows, target_os = "macos"))]
fn main() {
    shell::run();
}

// Linux gets the command line and the Docker image, not a window: building
// the shell there would pull webkit2gtk into every check (DESKTOP.md).
#[cfg(not(any(windows, target_os = "macos")))]
fn main() {
    eprintln!(
        "photo-cleanup-desktop is built for Windows and macOS only; \
         on this system use `photo-cleanup serve`."
    );
    std::process::exit(2);
}
