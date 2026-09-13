//! What the daemon knows, and how many sessions collapse into one icon.
//!
//! Nothing here is persisted. A tray that outlives the sessions it describes
//! would show stale work after a reboot, and the transcripts are the record —
//! this is a view, not a store.

use std::collections::HashMap;
use std::time::SystemTime;

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
    pub session_id: String,
    pub project: String,
    /// Passed to the opener when the row is clicked.
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

/// One line in the notification history.
///
/// Kept separate from `Session` because they answer different questions: a
/// session says what is true now, an event says what happened. Collapsing the
/// two would lose every event but the last per session, which is most of them.
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub at: SystemTime,
    pub label: String,
    pub summary: String,
    pub session_id: String,
    pub cwd: String,
}

impl HistoryEntry {
    /// `project — what happened (3m ago)`.
    ///
    /// Relative rather than a clock time on purpose: a wall-clock label would
    /// need the local UTC offset, which none of this program's dependencies can
    /// supply and which is not worth a dependency. "3m ago" is also the more
    /// useful phrasing for a list that only ever holds the last few events.
    pub fn menu_label(&self, now: SystemTime) -> String {
        let elapsed = now
            .duration_since(self.at)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let ago = if elapsed < 60 {
            "just now".to_string()
        } else if elapsed < 3_600 {
            format!("{}m ago", elapsed / 60)
        } else {
            format!("{}h ago", elapsed / 3_600)
        };
        format!("{} — {} ({ago})", self.label, self.summary)
    }
}

/// How many events the history keeps. Enough to cover a working stretch,
/// short enough that the submenu stays readable without scrolling.
const HISTORY_LIMIT: usize = 20;

#[derive(Debug, Default)]
pub struct TrayState {
    sessions: HashMap<String, Session>,
    /// Insertion order, so the menu does not reshuffle on every update the
    /// way a HashMap iteration would.
    order: Vec<String>,
    /// Most recent first.
    history: Vec<HistoryEntry>,
    /// Suppresses desktop notifications while leaving the icon and the history
    /// live. The escape hatch for a busy afternoon, so the answer to "too many
    /// popups" is not uninstalling.
    pub muted: bool,
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
            session_id: event.session_id.clone(),
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

        let notification = notification_for(&event, status);
        if let Some(notification) = notification.as_ref() {
            self.history.insert(
                0,
                HistoryEntry {
                    at: SystemTime::now(),
                    label: event.title.clone().unwrap_or_else(|| event.project.clone()),
                    summary: notification.history_summary.clone(),
                    session_id: event.session_id.clone(),
                    cwd: event.cwd.clone(),
                },
            );
            self.history.truncate(HISTORY_LIMIT);
        }
        // Muting silences the popup, never the record: the history is how you
        // find out what you missed while it was off.
        notification.filter(|_| !self.muted)
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

    pub fn history(&self) -> &[HistoryEntry] {
        &self.history
    }

    pub fn clear_history(&mut self) {
        self.history.clear();
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
    /// Critical notifications are not dismissed on a timer by the desktop.
    /// Reserved for the one case where the agent is stopped until a person
    /// acts -- a popup that vanishes while nobody is looking is how a turn
    /// sits blocked for an hour.
    pub critical: bool,
    /// The same event, phrased for a one-line history row.
    pub history_summary: String,
    pub session_id: String,
    pub cwd: String,
}

/// Decides whether a transition deserves a desktop notification.
///
/// Deliberately quiet: a session merely starting or a subagent finishing is
/// not worth interrupting anyone. Notifying on every event is how a tray
/// becomes the thing people mute.
fn notification_for(event: &Event, status: SessionStatus) -> Option<Notification> {
    let label = event.title.as_deref().unwrap_or(&event.project);
    let build = |summary: String, body: &str, history: &str, critical: bool| {
        Some(Notification {
            summary,
            body: body.to_string(),
            icon: status.icon_name(),
            critical,
            history_summary: history.to_string(),
            session_id: event.session_id.clone(),
            cwd: event.cwd.clone(),
        })
    };

    match event.kind {
        EventKind::Stop if status == SessionStatus::Background => build(
            format!("{label}: turn finished"),
            "Background work is still running.",
            "finished, background work running",
            false,
        ),
        EventKind::Stop => build(
            format!("{label}: done"),
            "The agent finished its turn.",
            "done",
            false,
        ),
        EventKind::StopFailure => build(
            format!("{label}: failed"),
            "The turn ended with an error.",
            "failed",
            false,
        ),
        EventKind::PermissionRequest => build(
            format!("{label}: permission needed"),
            "The agent is waiting for your decision.",
            "needs a permission decision",
            true,
        ),
        EventKind::Notification => build(
            label.to_string(),
            event
                .message
                .as_deref()
                .unwrap_or("The agent needs your attention."),
            "needs your attention",
            true,
        ),
        EventKind::SessionStart | EventKind::SubagentStop | EventKind::SessionEnd => None,
    }
}

#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;
    use crate::protocol::BackgroundTask;
    use crate::protocol::EventKind;

    pub(crate) fn event(kind: EventKind, session: &str, background: Vec<&str>) -> Event {
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
}

#[cfg(test)]
mod tests {
    use super::tests_support::event;
    use super::*;
    use crate::protocol::EventKind;

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

#[cfg(test)]
mod history_tests {
    use super::tests_support::event;
    use super::*;
    use crate::protocol::EventKind;

    #[test]
    fn every_notified_event_lands_in_the_history() {
        let mut state = TrayState::default();
        state.apply(event(EventKind::Stop, "a", vec![]));
        state.apply(event(EventKind::PermissionRequest, "b", vec![]));
        assert_eq!(state.history().len(), 2);
        // Most recent first.
        assert_eq!(state.history()[0].session_id, "b");
    }

    #[test]
    fn events_that_do_not_notify_leave_no_history() {
        let mut state = TrayState::default();
        state.apply(event(EventKind::SessionStart, "a", vec![]));
        assert!(state.history().is_empty());
    }

    /// Muting is about popups, not about the record. Losing the history while
    /// muted would defeat the reason someone mutes: catching up later.
    #[test]
    fn muting_suppresses_the_popup_but_still_records() {
        let mut state = TrayState {
            muted: true,
            ..Default::default()
        };
        let notification = state.apply(event(EventKind::Stop, "a", vec![]));
        assert!(notification.is_none());
        assert_eq!(state.history().len(), 1);
    }

    #[test]
    fn the_history_is_capped() {
        let mut state = TrayState::default();
        for i in 0..(HISTORY_LIMIT + 5) {
            state.apply(event(EventKind::Stop, &format!("s{i}"), vec![]));
        }
        assert_eq!(state.history().len(), HISTORY_LIMIT);
    }

    #[test]
    fn a_permission_request_is_critical_and_a_finished_turn_is_not() {
        let mut state = TrayState::default();
        let critical = state.apply(event(EventKind::PermissionRequest, "a", vec![]));
        let ordinary = state.apply(event(EventKind::Stop, "b", vec![]));
        assert!(critical.expect("notified").critical);
        assert!(!ordinary.expect("notified").critical);
    }

    #[test]
    fn history_labels_read_as_relative_time() {
        let mut state = TrayState::default();
        state.apply(event(EventKind::Stop, "proj", vec![]));
        let entry = &state.history()[0];
        assert!(entry.menu_label(SystemTime::now()).ends_with("(just now)"));
        let later = SystemTime::now() + std::time::Duration::from_secs(7_200);
        assert!(entry.menu_label(later).ends_with("(2h ago)"));
    }
}
