use std::sync::atomic::{AtomicBool, Ordering};

/// Main-window content may bootstrap once. Document links must use commands;
/// allowing even a same-URL reload would discard unsaved editor memory.
pub(crate) struct MainNavigationGuard {
    started: AtomicBool,
    dev_url: Option<url::Url>,
}

impl MainNavigationGuard {
    pub fn new(dev_url: Option<url::Url>) -> Self {
        Self {
            started: AtomicBool::new(false),
            dev_url,
        }
    }

    pub fn allow(&self, target: &url::Url) -> bool {
        let trusted = self.dev_url.as_ref().is_some_and(|url| url == target)
            || (matches!(
                (target.scheme(), target.host_str()),
                ("http" | "https", Some("tauri.localhost")) | ("tauri", Some("localhost"))
            ) && matches!(target.path(), "/" | "/index.html")
                && target.query().is_none()
                && target.fragment().is_none());
        trusted
            && self
                .started
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_bootstrap_can_navigate_the_main_window() {
        for entry in ["http://tauri.localhost/", "http://localhost:1420/"] {
            let entry = url::Url::parse(entry).unwrap();
            let guard = MainNavigationGuard::new(Some(entry.clone()));
            assert!(!guard.allow(&url::Url::parse("https://example.com/").unwrap()));
            assert!(guard.allow(&entry));
            for next in ["", "#heading", "next.md", "../README.md", "//example.com/"] {
                assert!(!guard.allow(&entry.join(next).unwrap()));
            }
        }
    }
}
