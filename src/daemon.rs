//! The `daemon` mode: owns the socket, the state and the tray.
//!
//! Two threads, no async runtime. `ksni` runs its own D-Bus loop and hands
//! back a `Handle`; this thread blocks on the socket and pushes updates
//! through that handle. A whole executor to coordinate two threads would be
//! dependency for its own sake.

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixDatagram;

use ksni::blocking::TrayMethods;

use crate::protocol::{Event, socket_path};
use crate::state::{Notification, TrayState};
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

    let mut buffer = vec![0_u8; MAX_DATAGRAM];
    loop {
        let received = match socket.recv(&mut buffer) {
            Ok(received) => received,
            Err(error) => {
                tracing::warn!(%error, "socket read failed");
                continue;
            }
        };

        let event: Event = match serde_json::from_slice(&buffer[..received]) {
            Ok(event) => event,
            Err(error) => {
                tracing::warn!(%error, bytes = received, "discarded malformed datagram");
                continue;
            }
        };

        tracing::info!(
            kind = ?event.kind,
            session = %event.session_id,
            project = %event.project,
            background = event.background.len(),
            "event"
        );

        let notification = handle.update(|tray: &mut AgentTray| tray.state.apply(event));

        // `update` yields None once the tray has shut down — the desktop
        // dropped us, and there is nothing left to draw on.
        let Some(notification) = notification else {
            tracing::warn!("tray is gone, stopping");
            return Ok(());
        };

        if let Some(notification) = notification {
            raise(&notification);
        }
    }
}

fn raise(notification: &Notification) {
    let mut builder = notify_rust::Notification::new();
    builder
        .summary(&notification.summary)
        .body(&notification.body)
        .icon(notification.icon)
        .appname("zeo-systray")
        // One action, and it is the one thing you want from a notification
        // about an agent: get back to the conversation it is about.
        .action("open", "Open thread");

    if notification.critical {
        // Critical is not dismissed on a timer by the desktop, which is the
        // point: the agent is stopped until someone acts.
        builder
            .urgency(notify_rust::Urgency::Critical)
            .timeout(notify_rust::Timeout::Never);
    } else {
        builder.timeout(notify_rust::Timeout::Default);
    }

    let handle = match builder.show() {
        Ok(handle) => handle,
        Err(error) => {
            tracing::warn!(%error, "could not show desktop notification");
            return;
        }
    };

    // wait_for_action blocks until the notification is acted on or closed, so
    // it cannot run on the socket loop: a single unattended popup would stop
    // every later event from being processed.
    let session_id = notification.session_id.clone();
    let cwd = notification.cwd.clone();
    std::thread::spawn(move || {
        handle.wait_for_action(|action| {
            if action == "open" || action == "default" {
                crate::open::session(&session_id, &cwd);
            }
        });
    });
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
