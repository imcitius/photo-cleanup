//! What the window may show, and what it says when there is nothing to show.
//!
//! The shell is thin on purpose: it starts `pc-api` on loopback and points a
//! webview at it. The rules it enforces are here, free of Tauri, so they are
//! tested on every OS — Linux included, where no window is ever built.
//!
//! The rule for pages is narrow. The window shows the shared interface from
//! this launch's own server, `http://127.0.0.1:<port>`, and nothing else: no
//! other port (another local server is somebody else's page), no `localhost`
//! spelling of the same address, no external site, no new windows. A page the
//! window never shows cannot reach whatever native permissions the window
//! holds — a check on navigation is simpler to trust than a check on every
//! command.

use serde::Serialize;
use std::ffi::OsString;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use crate::StartupError;

/// The only window. Capabilities added later name it, not a wildcard.
pub const WINDOW_LABEL: &str = "main";
/// The window title — the product's name, as on the icon.
pub const WINDOW_TITLE: &str = "Photo Cleanup";
/// A first window big enough for the plan table and the comparison view side
/// by side on a 1440×900 laptop screen, with room left for the menu bar.
pub const WINDOW_SIZE: (f64, f64) = (1280.0, 820.0);
/// Below this the interface's two-column layouts collapse awkwardly.
pub const WINDOW_MIN_SIZE: (f64, f64) = (960.0, 600.0);
/// The start-up error page, bundled into the binary (`ui/` in the crate).
pub const ERROR_PAGE: &str = "error.html";

/// Where the desktop server listens: loopback, a port the system chooses.
///
/// Never a LAN address. The desktop server is for this machine's window; a
/// server for other machines is `photo-cleanup serve --bind`.
pub const SERVER_BIND: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);

/// The address the window opens: the server's own origin, spelled exactly as
/// the server was bound, so the `Host` it sends passes pc-api's loopback check.
pub fn server_url(addr: SocketAddr) -> String {
    format!("http://{addr}/")
}

/// Whether a page belongs to this launch's server.
///
/// `host` is as a URL parser reports it (`127.0.0.1`, `[::1]`), `port` with
/// the scheme default filled in. Only the exact bound address and port pass:
/// `localhost` would resolve to the same socket today, but it is a name, and
/// the rule is about the socket this process owns, not about names for it.
pub fn is_server_page(
    scheme: &str,
    host: Option<&str>,
    port: Option<u16>,
    addr: SocketAddr,
) -> bool {
    if scheme != "http" || port != Some(addr.port()) {
        return false;
    }
    let Some(host) = host else { return false };
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    host.parse::<IpAddr>().is_ok_and(|ip| ip == addr.ip())
}

/// Whether a page is one bundled into the binary — the start-up error page.
///
/// Tauri serves them as `tauri://localhost` on macOS and as
/// `http(s)://tauri.localhost` on Windows.
pub fn is_bundled_page(scheme: &str, host: Option<&str>) -> bool {
    matches!(
        (scheme, host),
        ("tauri", Some("localhost")) | ("http" | "https", Some("tauri.localhost"))
    )
}

/// The command line of the desktop app: nothing, or `--data-dir <path>`.
///
/// `--data-dir` is for tests and for people who know what they are doing: it
/// picks the data directory for this launch only and records nothing
/// (DESKTOP.md, «Порядок разрешения при запуске»). Anything else is refused
/// rather than ignored, so a typo does not silently open the default archive.
/// macOS adds `-psn_…` when Finder launches an app on some versions; it
/// carries nothing for us and is skipped.
pub fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Option<PathBuf>, String> {
    let mut args = args.into_iter();
    let mut data_dir = None;
    while let Some(arg) = args.next() {
        let text = arg.to_string_lossy();
        if text == "--data-dir" {
            let Some(dir) = args.next() else {
                return Err("--data-dir needs a folder".into());
            };
            if data_dir.replace(PathBuf::from(dir)).is_some() {
                return Err("--data-dir is given twice".into());
            }
        } else if let Some(dir) = text.strip_prefix("--data-dir=") {
            if data_dir.replace(PathBuf::from(dir)).is_some() {
                return Err("--data-dir is given twice".into());
            }
        } else if text.starts_with("-psn_") {
            continue;
        } else {
            return Err(format!("unknown argument: {text}"));
        }
    }
    Ok(data_dir)
}

/// What the error page shows: the message in both languages and the details.
///
/// Both languages, because the failure comes before the settings are read —
/// the language the operator chose is in the very database that could not be
/// opened. The page puts the one matching the system first.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StartupReport {
    pub ru: String,
    pub en: String,
    /// The error as data: `kind`, paths, reasons. Shown as details and what
    /// the actions of the data-folder screen will be chosen by (el-646).
    pub detail: serde_json::Value,
}

impl StartupReport {
    /// A failure of the data directory: resolving, opening, recording it.
    pub fn from_startup(e: &StartupError) -> Self {
        let (ru, en) = in_both_languages(|| e.to_string());
        let detail = serde_json::to_value(e).unwrap_or(serde_json::Value::Null);
        Self { ru, en, detail }
    }

    /// Any other failure — the server not starting, the window not opening.
    /// `message` is called once per language.
    pub fn other(kind: &str, message: impl Fn() -> String) -> Self {
        let (ru, en) = in_both_languages(message);
        Self {
            detail: serde_json::json!({ "kind": kind }),
            ru,
            en,
        }
    }

    /// The script that hands the report to the error page before it loads.
    ///
    /// JSON is a JavaScript expression, and nothing here goes through HTML:
    /// the page puts the strings in with `textContent`, so a path with `<` in
    /// it is shown, not interpreted.
    pub fn init_script(&self) -> String {
        let json = serde_json::to_string(self).unwrap_or_else(|_| "null".into());
        format!("window.__PC_STARTUP_ERROR__ = {json};")
    }
}

/// Render a message in Russian and in English.
///
/// The language is process-wide (`pc_core::lang`), so it is switched and put
/// back. Only called when start-up has failed, before or instead of a server
/// that would read it concurrently.
fn in_both_languages(message: impl Fn() -> String) -> (String, String) {
    use pc_core::lang::{current, set, Lang};
    // Two reports rendered at once would put each other's language back.
    static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let was = current();
    set(Lang::Ru);
    let ru = message();
    set(Lang::En);
    let en = message();
    set(was);
    (ru, en)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn only_the_servers_own_origin_is_a_server_page() {
        let a = addr("127.0.0.1:51234");
        assert!(is_server_page("http", Some("127.0.0.1"), Some(51234), a));
        // Another local server — somebody else's page.
        assert!(!is_server_page("http", Some("127.0.0.1"), Some(8080), a));
        // The same socket by name is still refused: the rule is the address.
        assert!(!is_server_page("http", Some("localhost"), Some(51234), a));
        assert!(!is_server_page("https", Some("127.0.0.1"), Some(51234), a));
        assert!(!is_server_page("http", Some("example.com"), Some(51234), a));
        assert!(!is_server_page("http", Some("127.0.0.2"), Some(51234), a));
        assert!(!is_server_page("http", None, Some(51234), a));
        assert!(!is_server_page("file", None, None, a));
        assert!(!is_server_page("data", None, None, a));
    }

    #[test]
    fn an_ipv6_server_matches_its_bracketed_host() {
        let a = addr("[::1]:4000");
        assert!(is_server_page("http", Some("[::1]"), Some(4000), a));
        assert!(!is_server_page("http", Some("127.0.0.1"), Some(4000), a));
    }

    #[test]
    fn the_window_opens_the_bound_address_verbatim() {
        assert_eq!(
            server_url(addr("127.0.0.1:51234")),
            "http://127.0.0.1:51234/"
        );
        assert_eq!(server_url(addr("[::1]:4000")), "http://[::1]:4000/");
    }

    #[test]
    fn the_desktop_server_binds_loopback_on_a_system_port() {
        assert!(SERVER_BIND.ip().is_loopback());
        assert_eq!(SERVER_BIND.port(), 0);
    }

    #[test]
    fn bundled_pages_are_the_tauri_origin_on_each_os() {
        assert!(is_bundled_page("tauri", Some("localhost")));
        assert!(is_bundled_page("http", Some("tauri.localhost")));
        assert!(is_bundled_page("https", Some("tauri.localhost")));
        assert!(!is_bundled_page("http", Some("localhost")));
        assert!(!is_bundled_page(
            "https",
            Some("tauri.localhost.evil.example")
        ));
    }

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn the_command_line_takes_a_data_dir_and_nothing_else() {
        assert_eq!(parse_args(os(&[])), Ok(None));
        assert_eq!(
            parse_args(os(&["--data-dir", "/tmp/Фото архив"])),
            Ok(Some(PathBuf::from("/tmp/Фото архив")))
        );
        assert_eq!(
            parse_args(os(&["--data-dir=/tmp/x"])),
            Ok(Some(PathBuf::from("/tmp/x")))
        );
        assert_eq!(parse_args(os(&["-psn_0_12345"])), Ok(None));
        assert!(parse_args(os(&["--data-dir"])).is_err());
        assert!(parse_args(os(&["--data-dir", "/a", "--data-dir", "/b"])).is_err());
        assert!(parse_args(os(&["--datadir", "/a"])).is_err());
    }

    #[test]
    fn a_startup_report_carries_both_languages_and_the_kind() {
        let e = StartupError::DataUnavailable {
            source: crate::Source::Custom,
            dir: PathBuf::from("/Volumes/Архив/photo-cleanup"),
            why: crate::Unavailable::Missing,
            previous: None,
        };
        let r = StartupReport::from_startup(&e);
        assert!(r.ru.contains("данные недоступны"), "{}", r.ru);
        assert!(r.en.contains("data unavailable"), "{}", r.en);
        assert!(r.en.contains("/Volumes/Архив/photo-cleanup"));
        assert_eq!(r.detail["kind"], "data_unavailable");
        assert_eq!(r.detail["why"]["what"], "missing");
    }

    #[test]
    fn the_init_script_is_plain_json_even_for_hostile_paths() {
        let r = StartupReport::other("server", || "</script><b>\"x\"\u{2028}".into());
        let script = r.init_script();
        let json = script
            .strip_prefix("window.__PC_STARTUP_ERROR__ = ")
            .and_then(|s| s.strip_suffix(';'))
            .unwrap();
        let back: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(back["en"], "</script><b>\"x\"\u{2028}");
        assert_eq!(back["detail"]["kind"], "server");
    }
}
