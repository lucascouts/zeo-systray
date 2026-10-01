//! Routing a click on a notification back to the session it is about.
//!
//! A notification id is only unique for the lifetime of the server that
//! issued it. Plasma's counter starts over when `plasmashell` restarts, so the
//! id of a notification raised before the restart is handed out again after
//! it. Measured 2026-09-30: `plasmashell` restarted twice under one daemon,
//! and from then on a single click opened two, then three sessions at the same
//! microsecond -- the clicked one plus every older notification still waiting
//! under the recycled id. Those older ones were threads that were archived or
//! belonged to projects not loaded, which is what made it look like a wrong
//! thread rather than an extra one.
//!
//! `notify-rust`'s `wait_for_action` cannot tell the two apart: it matches the
//! id alone and keeps one thread per notification blocked until a signal
//! arrives -- forever, for a popup that expires into the history without a
//! `NotificationClosed`. So this keeps one listener for the whole daemon and
//! keys each pending action on the id *and* the unique bus name of the server
//! that issued it. A signal from a different server is ignored, and when the
//! server's name loses its owner every action it issued is dropped.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use futures_lite::StreamExt;
use zbus::fdo::DBusProxy;
use zbus::message::Type as MessageType;
use zbus::names::BusName;
use zbus::{Connection, MatchRule, MessageStream};

const SERVER_NAME: &str = "org.freedesktop.Notifications";
const SERVER_INTERFACE: &str = "org.freedesktop.Notifications";

/// Where a click on a notification should go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub session_id: String,
    pub cwd: String,
}

#[derive(Debug)]
struct Pending {
    /// Unique bus name (`:1.234`) of the server that issued the id.
    server: String,
    target: Target,
}

/// The pending actions, and the only place that decides whether a signal
/// belongs to one of them. Pure so the collision can be tested without a bus.
#[derive(Debug, Default)]
struct Registry {
    pending: HashMap<u32, Pending>,
}

impl Registry {
    fn register(&mut self, id: u32, server: String, target: Target) {
        self.pending.insert(id, Pending { server, target });
    }

    /// An action invoked on `id` by `sender`. Returns where to go, if the
    /// signal comes from the server that issued the id and asks to open.
    fn action(&mut self, sender: &str, id: u32, action: &str) -> Option<Target> {
        if self.pending.get(&id)?.server != sender {
            return None;
        }
        let pending = self.pending.remove(&id)?;
        matches!(action, "open" | "default").then_some(pending.target)
    }

    fn closed(&mut self, sender: &str, id: u32) {
        if self
            .pending
            .get(&id)
            .is_some_and(|pending| pending.server == sender)
        {
            self.pending.remove(&id);
        }
    }

    /// The server `gone` left the bus. Its ids mean nothing any more and the
    /// next server will reuse them. Returns how many were dropped.
    fn server_gone(&mut self, gone: &str) -> usize {
        let before = self.pending.len();
        self.pending.retain(|_, pending| pending.server != gone);
        before - self.pending.len()
    }
}

/// What woke the listener.
enum Wake {
    Signal(zbus::Result<zbus::Message>),
    OwnerChanged(zbus::fdo::NameOwnerChanged),
}

/// Handle to the listener, cheap to clone.
#[derive(Clone)]
pub struct Actions {
    connection: Connection,
    registry: Arc<Mutex<Registry>>,
}

impl Actions {
    /// Connects to the session bus and starts the listener thread.
    pub fn spawn() -> zbus::Result<Self> {
        let connection = futures_lite::future::block_on(Connection::session())?;
        let actions = Self {
            connection,
            registry: Arc::default(),
        };
        let listener = actions.clone();
        std::thread::spawn(move || {
            if let Err(error) = futures_lite::future::block_on(listener.listen()) {
                tracing::warn!(%error, "notification action listener stopped");
            }
        });
        Ok(actions)
    }

    /// Remembers where a click on notification `id` should go.
    pub fn register(&self, id: u32, target: Target) {
        let server = match futures_lite::future::block_on(self.server_owner()) {
            Ok(server) => server,
            Err(error) => {
                tracing::warn!(%error, id, "could not identify the notification server");
                return;
            }
        };
        tracing::debug!(id, server = %server, session = %target.session_id, "notification registered");
        self.lock().register(id, server, target);
    }

    async fn server_owner(&self) -> zbus::Result<String> {
        let proxy = DBusProxy::new(&self.connection).await?;
        let owner = proxy
            .get_name_owner(BusName::try_from(SERVER_NAME)?)
            .await?;
        Ok(owner.to_string())
    }

    async fn listen(&self) -> zbus::Result<()> {
        let proxy = DBusProxy::new(&self.connection).await?;
        for member in ["ActionInvoked", "NotificationClosed"] {
            let rule = MatchRule::builder()
                .msg_type(MessageType::Signal)
                .interface(SERVER_INTERFACE)?
                .member(member)?
                .build();
            proxy.add_match_rule(rule).await?;
        }
        let mut owner_changes = proxy
            .receive_name_owner_changed_with_args(&[(0, SERVER_NAME)])
            .await?;

        let mut signals = MessageStream::from(&self.connection);
        loop {
            let next =
                futures_lite::future::or(async { signals.next().await.map(Wake::Signal) }, async {
                    owner_changes.next().await.map(Wake::OwnerChanged)
                })
                .await;
            match next {
                Some(Wake::Signal(Ok(message))) => self.on_signal(&message),
                Some(Wake::Signal(Err(error))) => {
                    tracing::debug!(%error, "unreadable message on the bus");
                }
                Some(Wake::OwnerChanged(change)) => {
                    // Nested rather than a let-chain: those need Rust 1.88,
                    // and the declared rust-version is 1.85.
                    let old = change
                        .args()
                        .ok()
                        .and_then(|args| args.old_owner().as_ref().map(ToString::to_string));
                    if let Some(old) = old {
                        let dropped = self.lock().server_gone(&old);
                        tracing::info!(server = %old, dropped, "notification server left the bus");
                    }
                }
                // The connection is gone; spinning on it would burn a core.
                None => return Ok(()),
            }
        }
    }

    fn on_signal(&self, message: &zbus::Message) {
        let header = message.header();
        if header.message_type() != MessageType::Signal
            || header.interface().map(|name| name.as_str()) != Some(SERVER_INTERFACE)
        {
            return;
        }
        let Some(sender) = header.sender() else {
            return;
        };
        match header.member().map(|name| name.as_str()) {
            Some("ActionInvoked") => {
                let Ok((id, action)) = message.body().deserialize::<(u32, String)>() else {
                    return;
                };
                let target = self.lock().action(sender.as_str(), id, &action);
                tracing::debug!(id, sender = %sender, action = %action, matched = target.is_some(), "notification action");
                if let Some(target) = target {
                    crate::open::session(&target.session_id, &target.cwd);
                }
            }
            Some("NotificationClosed") => {
                if let Ok((id, _reason)) = message.body().deserialize::<(u32, u32)>() {
                    self.lock().closed(sender.as_str(), id);
                }
            }
            _ => {}
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Registry> {
        // The registry holds no invariant a panic could break halfway.
        self.registry.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(session: &str) -> Target {
        Target {
            session_id: session.into(),
            cwd: format!("/home/user/{session}"),
        }
    }

    /// The defect, as measured: the server restarts, hands out an id still
    /// pending from before, and one click must open only the new session.
    #[test]
    fn an_id_recycled_by_a_restarted_server_opens_only_the_new_session() {
        let mut registry = Registry::default();
        registry.register(7, ":1.10".into(), target("archived"));

        assert_eq!(registry.server_gone(":1.10"), 1);
        registry.register(7, ":1.99".into(), target("current"));

        assert_eq!(registry.action(":1.99", 7, "open"), Some(target("current")));
        assert_eq!(registry.action(":1.99", 7, "open"), None);
    }

    /// Even without the owner change -- a missed signal -- a click from a
    /// different server never reaches an action it did not issue.
    #[test]
    fn a_signal_from_another_server_is_ignored() {
        let mut registry = Registry::default();
        registry.register(7, ":1.10".into(), target("a"));

        assert_eq!(registry.action(":1.99", 7, "open"), None);
        assert_eq!(registry.action(":1.10", 7, "default"), Some(target("a")));
    }

    #[test]
    fn closing_or_another_action_releases_without_opening() {
        let mut registry = Registry::default();
        registry.register(1, ":1.10".into(), target("a"));
        registry.register(2, ":1.10".into(), target("b"));

        registry.closed(":1.10", 1);
        assert_eq!(registry.action(":1.10", 1, "open"), None);

        assert_eq!(registry.action(":1.10", 2, "dismiss"), None);
        assert!(registry.pending.is_empty());
    }
}
