//! The StatusNotifierItem itself.
//!
//! `ksni` speaks the KDE/freedesktop SNI protocol over D-Bus directly, with no
//! GTK and no libappindicator in the way. That is the whole reason this is a
//! separate program rather than a patch: one small daemon, one D-Bus name, and
//! every desktop that implements the spec picks it up.

use ksni::menu::StandardItem;
use ksni::{Category, MenuItem, Status, ToolTip};

use crate::icon::IconSet;
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

    /// The base icon is the brand, not the status: an icon that changes shape
    /// every turn is hard to find in a full tray. Status rides on the overlay
    /// and on `status()` below.
    fn icon_name(&self) -> String {
        self.icons.base.clone()
    }

    /// Lets the daemon run from a build tree, before the icon is installed
    /// into hicolor. Empty in a normal installation.
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
                items.push(
                    StandardItem {
                        label: session.menu_label(),
                        icon_name: session.status.icon_name().into(),
                        // TODO: open the session's cwd in the editor. Left
                        // inert until the first version is proven on a real
                        // desktop — a menu entry that does the wrong thing is
                        // worse than one that does nothing.
                        enabled: false,
                        ..Default::default()
                    }
                    .into(),
                );
            }
        }

        items.push(MenuItem::Separator);
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
