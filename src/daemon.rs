//! The `daemon` mode: owns the socket, the state and the tray.
//!
//! Four threads, no async runtime. `ksni` runs its own D-Bus loop and hands
//! back a `Handle`; this thread blocks on the socket and pushes updates
//! through that handle; a third watches the ACP adapter's quota sample and
//! feeds it back through the same socket; a fourth routes clicks on
//! notifications (see `actions`). A whole executor to coordinate them would be
//! dependency for its own sake.

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixDatagram;

use ksni::blocking::TrayMethods;

use crate::actions::Actions;
use crate::protocol::{Datagram, Event, socket_path};
use crate::state::TrayState;
use crate::tray::AgentTray;

/// Largest datagram we will accept. Events are small; anything near this is a
/// bug or someone probing the socket.
const MAX_DATAGRAM: usize = 64 * 1024;

pub fn run(demo: bool) -> Result<(), Box<dyn std::error::Error>> {
    let path = socket_path().ok_or("XDG_RUNTIME_DIR is unset or empty")?;

    // A socket file left behind by a killed daemon would make bind() fail.
    // Removing it is safe because the path is inside our own runtime dir.
    if path.exists() {
        std::fs::remove_file(&path)?;
    }

    let socket = UnixDatagram::bind(&path)?;
    // The runtime dir is already 0700, but the socket says 0600 as well: the
    // permissions should state the intent, not rely on the parent's.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    tracing::info!(path = %path.display(), "listening");

    let mut state = TrayState::default();
    if demo {
        seed_demo(&mut state);
        tracing::info!("demo state seeded");
    }

    let icons = crate::icon::IconSet::from_env();
    tracing::info!(base = %icons.base, theme_path = %icons.theme_path, "icon set");
    let handle = AgentTray::new(state, icons).spawn()?;
    tracing::info!("tray registered on the session bus");

    std::thread::spawn(crate::usage::watch_quota_cache);

    let actions = Actions::spawn()?;

    let mut buffer = vec![0_u8; MAX_DATAGRAM];
    loop {
        let received = match socket.recv(&mut buffer) {
            Ok(received) => received,
            Err(error) => {
                tracing::warn!(%error, "socket read failed");
                continue;
            }
        };

        let datagram: Datagram = match serde_json::from_slice(&buffer[..received]) {
            Ok(datagram) => datagram,
            Err(error) => {
                tracing::warn!(%error, bytes = received, "discarded malformed datagram");
                continue;
            }
        };

        let notifications = match datagram {
            Datagram::Event(event) => {
                tracing::info!(
                    kind = ?event.kind,
                    session = %event.session_id,
                    project = %event.project,
                    background = event.background.len(),
                    "event"
                );
                handle.update(|tray: &mut AgentTray| tray.state.apply(event).into_iter().collect())
            }
            Datagram::Usage(report) => {
                // Debug, not info: the status line road sends one of these on
                // every render of every session.
                tracing::debug!(windows = report.usage.len(), "usage");
                let notifications =
                    handle.update(|tray: &mut AgentTray| tray.state.apply_usage(report));
                for notification in notifications.iter().flatten() {
                    tracing::info!(summary = %notification.summary, "usage step crossed");
                }
                notifications
            }
        };

        // `update` yields None once the tray has shut down — the desktop
        // dropped us, and there is nothing left to draw on.
        let Some(notifications) = notifications else {
            tracing::warn!("tray is gone, stopping");
            return Ok(());
        };

        for notification in &notifications {
            actions.show(notification);
        }
    }
}

/// Fills the tray with plausible sessions so the icon and menu can be seen on
/// a real desktop before any hook is wired up.
fn seed_demo(state: &mut TrayState) {
    use crate::protocol::{BackgroundTask, EventKind};

    let demo_events = [
        (
            EventKind::SessionStart,
            "demo-1",
            "/home/user/Projects/zeo",
            Some("Patch the agent panel"),
            vec![],
        ),
        (
            EventKind::Stop,
            "demo-2",
            "/home/user/Projects/zeo-systray",
            Some("Wire up the tray"),
            vec![BackgroundTask {
                kind: "shell".into(),
                status: "running".into(),
            }],
        ),
        (
            EventKind::PermissionRequest,
            "demo-3",
            "/home/user/Projects/bentoo",
            Some("Bump the ebuild"),
            vec![],
        ),
    ];

    for (kind, id, cwd, title, background) in demo_events {
        let project = std::path::Path::new(cwd)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        state.apply(Event {
            kind,
            session_id: id.into(),
            cwd: cwd.into(),
            project,
            title: title.map(str::to_owned),
            agent_type: None,
            message: None,
            background,
        });
    }
}
