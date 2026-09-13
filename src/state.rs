//! What the daemon knows, and how many sessions collapse into one icon.
//!
//! Nothing here is persisted. A tray that outlives the sessions it describes
//! would show stale work after a reboot, and the transcripts are the record —
//! this is a view, not a store.

use std::collections::HashMap;

use crate::protocol::{Event, EventKind};

/// Where a single session currently stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SessionStatus {
    /// Finished, nothing left running.
    Done,
    /// Working.
    Running,
    /// Finished the turn, but left background work behind.
    Background,
    /// Ended with an error.
    Failed,
    /// Blocked on the user: a permission request, or Claude asking for input.
    NeedsYou,
}

impl SessionStatus {
    /// Freedesktop icon name for this status.
    ///
    /// These are all names from the icon naming spec rather than bundled
    /// pixmaps, so the icon follows the user's theme — including light/dark —
    /// with no work on our side.
    pub fn icon_name(self) -> &'static str {
        match self {
            Self::Done => "user-idle",
            Self::Running => "system-run",
            Self::Background => "system-run",
            Self::Failed => "dialog-error",
            Self::NeedsYou => "dialog-question",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Running => "running",
            Self::Background => "background work",
            Self::Failed => "failed",
            Self::NeedsYou => "needs you",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Session {
    pub project: String,
    /// Kept for the menu entry that will open this session's directory. Unread
    /// until that lands — see the TODO in `tray.rs`.
    #[allow(dead_code)]
    pub cwd: String,
    pub title: Option<String>,
    pub status: SessionStatus,
    /// Labels of background tasks still in flight, e.g. `shell`, `subagent`.
    pub background: Vec<String>,
}

impl Session {
    /// One menu line: what the user reads to tell sessions apart.
    pub fn menu_label(&self) -> String {
        let head = self.title.as_deref().unwrap_or(&self.project);
        if self.background.is_empty() {
            format!("{head} — {}", self.status.label())
        } else {
            format!(
                "{head} — {} ({})",
                self.status.label(),
                self.background.join(", ")
            )
        }
    }
}

#[derive(Debug, Default)]
pub struct TrayState {
    sessions: HashMap<String, Session>,
    /// Insertion order, so the menu does not reshuffle on every update the
    /// way a HashMap iteration would.
    order: Vec<String>,
}

impl TrayState {
    /// Folds one event into the state. Returns a notification to raise, if
    /// this transition is one worth interrupting the user for.
    pub fn apply(&mut self, event: Event) -> Option<Notification> {
        if event.kind == EventKind::SessionEnd {
            self.remove(&event.session_id);
            return None;
        }

        let status = match event.kind {
            EventKind::SessionStart => SessionStatus::Running,
            EventKind::Stop if !event.background.is_empty() => SessionStatus::Background,
            EventKind::Stop => SessionStatus::Done,
            EventKind::StopFailure => SessionStatus::Failed,
            EventKind::PermissionRequest | EventKind::Notification => SessionStatus::NeedsYou,
            // A subagent finishing does not mean the session did; the parent
            // is still working.
            EventKind::SubagentStop => SessionStatus::Running,
            EventKind::SessionEnd => unreachable!("handled above"),
        };

        let background: Vec<String> = event
            .background
            .iter()
            .map(|task| task.kind.clone())
            .collect();

        let session = Session {
            project: event.project.clone(),
            cwd: event.cwd.clone(),
            title: event.title.clone(),
            status,
            background,
        };

        if self
            .sessions
            .insert(event.session_id.clone(), session)
            .is_none()
        {
            self.order.push(event.session_id.clone());
        }

        notification_for(&event, status)
    }

    pub fn remove(&mut self, session_id: &str) {
        self.sessions.remove(session_id);
        self.order.retain(|id| id != session_id);
    }

    /// Drops every session that is no longer doing anything.
    pub fn clear_finished(&mut self) {
        self.sessions
            .retain(|_, session| !matches!(session.status, SessionStatus::Done));
        self.order.retain(|id| self.sessions.contains_key(id));
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    pub fn sessions(&self) -> impl Iterator<Item = &Session> {
        self.order.iter().filter_map(|id| self.sessions.get(id))
    }

    /// The single status the icon shows for every session at once.
    ///
    /// Worst-first: one session needing you outranks nine that are merely
    /// running, because that is the one with a person blocked on it. Falls
    /// back to `Done` when there is nothing to say.
    pub fn aggregate(&self) -> SessionStatus {
        self.sessions()
            .map(|session| session.status)
            .max()
            .unwrap_or(SessionStatus::Done)
    }

    pub fn summary(&self) -> String {
        if self.is_empty() {
            return "No agent sessions".into();
        }
        let running = self
            .sessions()
            .filter(|session| session.status == SessionStatus::Running)
            .count();
        let needs_you = self
            .sessions()
            .filter(|session| session.status == SessionStatus::NeedsYou)
            .count();
        let background = self
            .sessions()
            .filter(|session| session.status == SessionStatus::Background)
            .count();

        let mut parts = Vec::new();
        if needs_you > 0 {
            parts.push(format!("{needs_you} need you"));
        }
        if running > 0 {
            parts.push(format!("{running} running"));
        }
        if background > 0 {
            parts.push(format!("{background} in background"));
        }
        if parts.is_empty() {
            format!("{} session(s) idle", self.sessions().count())
        } else {
            parts.join(", ")
        }
    }
}

/// A desktop notification to raise.
pub struct Notification {
    pub summary: String,
    pub body: String,
    pub icon: &'static str,
}

/// Decides whether a transition deserves a desktop notification.
///
/// Deliberately quiet: a session merely starting or a subagent finishing is
/// not worth interrupting anyone. Notifying on every event is how a tray
/// becomes the thing people mute.
fn notification_for(event: &Event, status: SessionStatus) -> Option<Notification> {
    let label = event.title.as_deref().unwrap_or(&event.project);
    match event.kind {
        EventKind::Stop if status == SessionStatus::Background => Some(Notification {
            summary: format!("{label}: turn finished"),
            body: "Background work is still running.".into(),
            icon: status.icon_name(),
        }),
        EventKind::Stop => Some(Notification {
            summary: format!("{label}: done"),
            body: "The agent finished its turn.".into(),
            icon: status.icon_name(),
        }),
        EventKind::StopFailure => Some(Notification {
            summary: format!("{label}: failed"),
            body: "The turn ended with an error.".into(),
            icon: status.icon_name(),
        }),
        EventKind::PermissionRequest => Some(Notification {
            summary: format!("{label}: permission needed"),
            body: "The agent is waiting for your decision.".into(),
            icon: status.icon_name(),
        }),
        EventKind::Notification => Some(Notification {
            summary: label.to_string(),
            body: event
                .message
                .clone()
                .unwrap_or_else(|| "The agent needs your attention.".into()),
            icon: status.icon_name(),
        }),
        EventKind::SessionStart | EventKind::SubagentStop | EventKind::SessionEnd => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{BackgroundTask, EventKind};

    fn event(kind: EventKind, session: &str, background: Vec<&str>) -> Event {
        Event {
            kind,
            session_id: session.into(),
            cwd: format!("/home/user/{session}"),
            project: session.into(),
            title: None,
            agent_type: None,
            message: None,
            background: background
                .into_iter()
                .map(|kind| BackgroundTask {
                    kind: kind.into(),
                    status: "running".into(),
                })
                .collect(),
        }
    }

    #[test]
    fn stop_with_background_work_is_not_done() {
        let mut state = TrayState::default();
        state.apply(event(EventKind::Stop, "a", vec!["shell"]));
        assert_eq!(state.aggregate(), SessionStatus::Background);
    }

    #[test]
    fn stop_without_background_work_is_done() {
        let mut state = TrayState::default();
        state.apply(event(EventKind::Stop, "a", vec![]));
        assert_eq!(state.aggregate(), SessionStatus::Done);
    }

    #[test]
    fn one_session_needing_you_outranks_every_other_state() {
        let mut state = TrayState::default();
        state.apply(event(EventKind::SessionStart, "a", vec![]));
        state.apply(event(EventKind::Stop, "b", vec!["shell"]));
        state.apply(event(EventKind::StopFailure, "c", vec![]));
        state.apply(event(EventKind::PermissionRequest, "d", vec![]));
        assert_eq!(state.aggregate(), SessionStatus::NeedsYou);
    }

    #[test]
    fn session_end_removes_the_session() {
        let mut state = TrayState::default();
        state.apply(event(EventKind::SessionStart, "a", vec![]));
        assert!(!state.is_empty());
        state.apply(event(EventKind::SessionEnd, "a", vec![]));
        assert!(state.is_empty());
    }

    #[test]
    fn clear_finished_keeps_work_that_is_still_live() {
        let mut state = TrayState::default();
        state.apply(event(EventKind::Stop, "done", vec![]));
        state.apply(event(EventKind::Stop, "busy", vec!["subagent"]));
        state.clear_finished();
        let left: Vec<_> = state.sessions().map(|s| s.project.clone()).collect();
        assert_eq!(left, vec!["busy"]);
    }

    #[test]
    fn a_session_updates_in_place_rather_than_duplicating() {
        let mut state = TrayState::default();
        state.apply(event(EventKind::SessionStart, "a", vec![]));
        state.apply(event(EventKind::Stop, "a", vec![]));
        assert_eq!(state.sessions().count(), 1);
        assert_eq!(state.aggregate(), SessionStatus::Done);
    }

    #[test]
    fn starting_a_session_raises_no_notification() {
        let mut state = TrayState::default();
        assert!(
            state
                .apply(event(EventKind::SessionStart, "a", vec![]))
                .is_none()
        );
    }
}
