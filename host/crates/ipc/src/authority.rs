//! Window authority.
//!
//! Only the `main` window may call the general API, and only while its current URL is the app's
//! own origin (or the dev server in debug builds). Auxiliary windows get short allowlists;
//! none exist yet, so every non-main call is refused.

use url::Url;

pub const MAIN_WINDOW: &str = "main";

/// The dev server origin (Vite), honoured in debug builds only.
pub const DEV_SERVER: &str = "http://127.0.0.1:1420";

/// Who is calling `api`, taken from the transport (Tauri), never from the arguments.
#[derive(Clone, Debug)]
pub struct Caller {
    pub window_label: String,
    /// The calling webview's current URL.
    pub url: String,
}

/// True when `url` is the app's own page: `tauri://localhost`, `http(s)://tauri.localhost` (the
/// Windows form), or in debug builds the dev server. No port (except the dev server's), no user
/// name or password.
pub fn is_trusted_origin(url: &str, debug: bool) -> bool {
    let Ok(u) = Url::parse(url) else {
        return false;
    };
    if !u.username().is_empty() || u.password().is_some() {
        return false;
    }
    let host = u.host_str().unwrap_or("");
    match u.scheme() {
        "tauri" => host == "localhost" && u.port().is_none(),
        "http" | "https" if host == "tauri.localhost" => u.port().is_none(),
        "http" if debug => host == "127.0.0.1" && u.port() == Some(1420),
        _ => false,
    }
}

/// Navigation allowlist for the main window: the app origin, `about:srcdoc`, and the
/// dev server in debug builds. A future interactive-app scheme would join this list.
pub fn main_window_may_navigate(url: &str, debug: bool) -> bool {
    url == "about:srcdoc" || is_trusted_origin(url, debug)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_origins_are_trusted() {
        assert!(is_trusted_origin("tauri://localhost/", false));
        assert!(is_trusted_origin(
            "http://tauri.localhost/index.html",
            false
        ));
        assert!(is_trusted_origin("https://tauri.localhost/", false));
    }

    #[test]
    fn dev_server_only_in_debug() {
        assert!(is_trusted_origin("http://127.0.0.1:1420/", true));
        assert!(!is_trusted_origin("http://127.0.0.1:1420/", false));
        assert!(!is_trusted_origin("http://127.0.0.1:1421/", true));
        assert!(!is_trusted_origin("http://localhost:1420/", true));
    }

    #[test]
    fn everything_else_is_refused() {
        for url in [
            "https://example.com/",
            "http://tauri.localhost:8080/",
            "tauri://localhost:1/",
            "tauri://evil/",
            "http://user:pw@tauri.localhost/",
            "about:srcdoc",
            "file:///C:/x.html",
            "not a url",
        ] {
            assert!(!is_trusted_origin(url, true), "{url} must not be trusted");
        }
    }

    #[test]
    fn navigation_allows_srcdoc_but_not_the_web() {
        assert!(main_window_may_navigate("about:srcdoc", false));
        assert!(main_window_may_navigate("http://tauri.localhost/", false));
        assert!(!main_window_may_navigate("https://example.com/", false));
    }
}
