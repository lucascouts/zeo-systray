//! Which icon the tray shows, and where it is found.
//!
//! The item has a *base* icon that says whose tray this is, and an *overlay*
//! that says what is happening. Keeping those separate is what lets the badge
//! follow the user's theme while the brand stays put — and it is why the
//! status no longer has to be spelled out by swapping the whole icon.
//!
//! The base is configurable because this is a matter of taste, not of
//! correctness: the bundled mark is the default, `dev.zed.Zed-Nightly` is one
//! `ZEO_SYSTRAY_ICON` away, and so is anything else in the user's theme.

use crate::state::SessionStatus;

/// Icon name for the bundled mark, installed as
/// `hicolor/scalable/apps/zeo-systray.svg`.
pub const BUNDLED: &str = "zeo-systray";

/// Environment overrides, both read once at startup.
const ICON_ENV: &str = "ZEO_SYSTRAY_ICON";
const ICON_PATH_ENV: &str = "ZEO_SYSTRAY_ICON_PATH";

#[derive(Debug, Clone)]
pub struct IconSet {
    /// Base icon name — the identity of the tray.
    pub base: String,
    /// Extra directory prepended to the theme search path. Lets the daemon run
    /// from a build tree, before anything is installed system-wide.
    pub theme_path: String,
}

impl IconSet {
    pub fn from_env() -> Self {
        Self {
            base: std::env::var(ICON_ENV).unwrap_or_else(|_| BUNDLED.to_string()),
            theme_path: std::env::var(ICON_PATH_ENV).unwrap_or_default(),
        }
    }
}

impl SessionStatus {
    /// Small badge drawn over the base icon.
    ///
    /// `Running` deliberately has none: a badge on every single turn is visual
    /// noise, and the states worth glancing at are the ones that are not
    /// "working normally".
    pub fn overlay_icon_name(self) -> &'static str {
        match self {
            Self::Running => "",
            Self::Done => "",
            Self::Background => "system-run",
            Self::Failed => "dialog-error",
            Self::NeedsYou => "dialog-question",
        }
    }
}
