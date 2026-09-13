//! The StatusNotifierItem itself.
//!
//! `ksni` speaks the KDE/freedesktop SNI protocol over D-Bus directly, with no
//! GTK and no libappindicator in the way. That is the whole reason this is a
//! separate program rather than a patch: one small daemon, one D-Bus name, and
//! every desktop that implements the spec picks it up.

use std::time::SystemTime;

use ksni::menu::{CheckmarkItem, StandardItem, SubMenu};
use ksni::{Category, MenuItem, Status, ToolTip};

use crate::icon::IconSet;
use crate::open;
use crate::state::{SessionStatus, TrayState};

pub struct AgentTray {
    pub state: TrayState,
    icons: IconSet,
}

impl AgentTray {
    pub fn new(state: TrayState, icons: IconSet) -> Self {
        Self { state, icons }
    }
}

impl ksni::Tray for AgentTray {
    fn id(&self) -> String {
        env!("CARGO_PKG_NAME").into()
    }

    fn title(&self) -> String {
        "Agent sessions".into()
    }

    fn category(&self) -> Category {
        Category::ApplicationStatus
    }

    /// Left click opens the menu. There is no window to raise, so the default
    /// activate behaviour would do nothing at all.
    const MENU_ON_ACTIVATE: bool = true;

    /// Middle click clears finished sessions — the one action repeated often
    /// enough to deserve a gesture of its own.
    fn secondary_activate(&mut self, _x: i32, _y: i32) {
        self.state.clear_finished();
    }

    /// The base icon is the brand, not the status: an icon that changes shape
    /// every turn is hard to find in a full tray. Status rides on the overlay
    /// and on `status()` below.
    fn icon_name(&self) -> String {
        self.icons.base.clone()
    }

    /// Extra directory searched for icons. Normally the installed icon
    /// directory — see `IconSet::from_env` for why that is not redundant.
    fn icon_theme_path(&self) -> String {
        self.icons.theme_path.clone()
    }

    /// The badge. Empty for the states not worth flagging.
    fn overlay_icon_name(&self) -> String {
        self.state.aggregate().overlay_icon_name().into()
    }

    /// `NeedsAttention` is what makes Plasma highlight the item, so it is
    /// reserved for the one state where a person is actually blocked.
    fn status(&self) -> Status {
        match self.state.aggregate() {
            SessionStatus::NeedsYou => Status::NeedsAttention,
            _ => Status::Active,
        }
    }

    /// Keeps the brand while the host is highlighting us — swapping to a
    /// generic question mark here would lose the identity exactly when the
    /// user is scanning the tray for it.
    fn attention_icon_name(&self) -> String {
        self.icons.base.clone()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "Agent sessions".into(),
            description: self.state.summary(),
            icon_name: self.icons.base.clone(),
            icon_pixmap: Vec::new(),
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items: Vec<MenuItem<Self>> = Vec::new();

        // Live sessions. No status icon on these rows: the label already says
        // the state in words, and a theme icon that renders on some rows and
        // not others makes the quiet ones look broken.
        if self.state.is_empty() {
            items.push(
                StandardItem {
                    label: "No agent sessions".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        } else {
            for session in self.state.sessions() {
                let session_id = session.session_id.clone();
                let cwd = session.cwd.clone();
                items.push(
                    StandardItem {
                        label: session.menu_label(),
                        activate: Box::new(move |_: &mut Self| open::session(&session_id, &cwd)),
                        ..Default::default()
                    }
                    .into(),
                );
            }
        }

        // What happened, as opposed to what is true now. A submenu because the
        // two answer different questions and the top level should stay short.
        let history = self.state.history();
        if !history.is_empty() {
            let now = SystemTime::now();
            let mut entries: Vec<MenuItem<Self>> = history
                .iter()
                .map(|entry| {
                    let session_id = entry.session_id.clone();
                    let cwd = entry.cwd.clone();
                    StandardItem {
                        label: entry.menu_label(now),
                        activate: Box::new(move |_: &mut Self| open::session(&session_id, &cwd)),
                        ..Default::default()
                    }
                    .into()
                })
                .collect();
            entries.push(MenuItem::Separator);
            entries.push(
                StandardItem {
                    label: "Clear history".into(),
                    icon_name: "edit-clear-history".into(),
                    activate: Box::new(|tray: &mut Self| tray.state.clear_history()),
                    ..Default::default()
                }
                .into(),
            );

            items.push(MenuItem::Separator);
            items.push(
                SubMenu {
                    label: format!("Recent notifications ({})", history.len()),
                    submenu: entries,
                    ..Default::default()
                }
                .into(),
            );
        }

        items.push(MenuItem::Separator);
        items.push(
            CheckmarkItem {
                label: "Silence notifications".into(),
                checked: self.state.muted,
                activate: Box::new(|tray: &mut Self| tray.state.muted = !tray.state.muted),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: "Clear finished".into(),
                icon_name: "edit-clear-history".into(),
                activate: Box::new(|tray: &mut Self| tray.state.clear_finished()),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|_| std::process::exit(0)),
                ..Default::default()
            }
            .into(),
        );

        items
    }
}
