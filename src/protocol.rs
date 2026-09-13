//! What travels between the hook and the daemon.
//!
//! Two shapes live here. [`HookInput`] is what Claude Code hands the hook on
//! stdin; [`Event`] is the much smaller thing we put on the socket.
//!
//! The gap between them is deliberate and is the privacy boundary of this
//! program: a hook payload can carry `last_assistant_message`, tool inputs and
//! transcript paths, any of which may hold a secret or personal data. None of
//! that is copied into an `Event`. Only identifiers, a project label, counters
//! and the notification text Claude itself wrote for display cross the socket.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Socket file name under `$XDG_RUNTIME_DIR`.
pub const SOCKET_NAME: &str = "zeo-systray.sock";

/// Path of the datagram socket the daemon listens on.
///
/// `$XDG_RUNTIME_DIR` is per-user and already mode 0700, which is why the
/// socket lives there rather than in `/tmp` — and why this returns `None`
/// instead of falling back somewhere world-readable.
pub fn socket_path() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")?;
    if dir.is_empty() {
        return None;
    }
    Some(Path::new(&dir).join(SOCKET_NAME))
}

/// The hook events we care about. Anything else Claude Code emits is dropped
/// by the `notify` mode before it reaches the socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventKind {
    SessionStart,
    Stop,
    StopFailure,
    PermissionRequest,
    Notification,
    SubagentStop,
    SessionEnd,
}

impl EventKind {
    /// Maps the hook's `hook_event_name` onto the subset we forward.
    pub fn from_hook_name(name: &str) -> Option<Self> {
        match name {
            "SessionStart" => Some(Self::SessionStart),
            "Stop" => Some(Self::Stop),
            "StopFailure" => Some(Self::StopFailure),
            "PermissionRequest" => Some(Self::PermissionRequest),
            "Notification" => Some(Self::Notification),
            "SubagentStop" => Some(Self::SubagentStop),
            "SessionEnd" => Some(Self::SessionEnd),
            _ => None,
        }
    }
}

/// A background task still in flight when the turn ended.
///
/// Claude Code reports these on `Stop` precisely so a hook can tell "the
/// session is done" from "the session is parked waiting on background work".
/// We keep the label, never the command line — a shell command is exactly the
/// kind of string that carries tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundTask {
    pub kind: String,
    pub status: String,
}

/// One line on the socket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub kind: EventKind,
    pub session_id: String,
    /// Working directory, kept because it is how the user recognises a
    /// session. It is a path, not content.
    pub cwd: String,
    /// Last path component of `cwd`, precomputed so the daemon does no
    /// string work in the tray callback.
    pub project: String,
    /// Session title when Claude Code supplies one (`SessionStart`).
    pub title: Option<String>,
    /// Subagent type, present only for subagent events.
    pub agent_type: Option<String>,
    /// Text Claude wrote *for display* (`Notification` only). This is the one
    /// free-text field that crosses the socket, and it exists because a
    /// notification with no text is useless.
    pub message: Option<String>,
    /// Background work still running when the turn ended.
    pub background: Vec<BackgroundTask>,
}

/// The hook payload, narrowed to the fields we read.
///
/// Serde ignores unknown fields by default, which is what we want: Claude Code
/// adds fields to these payloads over time and an unknown one must never turn
/// into a failed notification.
#[derive(Debug, Deserialize)]
pub struct HookInput {
    pub hook_event_name: String,
    pub session_id: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub session_title: Option<String>,
    #[serde(default)]
    pub agent_type: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub background_tasks: Vec<HookBackgroundTask>,
}

#[derive(Debug, Deserialize)]
pub struct HookBackgroundTask {
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub status: String,
}

impl HookInput {
    /// Narrows a hook payload into the event we are willing to forward.
    ///
    /// Returns `None` for events we do not act on, so the caller can exit
    /// without touching the socket at all.
    pub fn into_event(self) -> Option<Event> {
        let kind = EventKind::from_hook_name(&self.hook_event_name)?;
        let project = Path::new(&self.cwd)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.cwd.clone());

        Some(Event {
            kind,
            session_id: self.session_id,
            cwd: self.cwd,
            project,
            title: self.session_title,
            agent_type: self.agent_type,
            // Only a Notification carries display text. Every other event
            // drops it, so a stray `message` field can never leak through.
            message: match kind {
                EventKind::Notification => self.message,
                _ => None,
            },
            background: self
                .background_tasks
                .into_iter()
                .map(|task| BackgroundTask {
                    kind: task.kind,
                    status: task.status,
                })
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The privacy boundary, as a regression test. A hook payload carries
    /// fields that may hold secrets — `last_assistant_message` and a shell
    /// task's `command` are the two obvious ones. Neither has a home in
    /// `Event`, and this asserts it stays that way: the serialised datagram
    /// must not contain them.
    #[test]
    fn secrets_in_the_hook_payload_never_reach_the_socket() {
        let raw = r#"{
            "hook_event_name": "Stop",
            "session_id": "s1",
            "cwd": "/home/user/project",
            "transcript_path": "/home/user/.claude/projects/x.jsonl",
            "last_assistant_message": "TOKEN-sk-ant-do-not-leak",
            "background_tasks": [
                { "id": "t1", "type": "shell", "status": "running",
                  "command": "curl -H 'Authorization: Bearer LEAKME' https://x" }
            ]
        }"#;

        let input: HookInput = serde_json::from_str(raw).expect("payload parses");
        let event = input.into_event().expect("Stop is forwarded");
        let wire = serde_json::to_string(&event).expect("event serialises");

        assert!(!wire.contains("TOKEN-sk-ant-do-not-leak"));
        assert!(!wire.contains("LEAKME"));
        assert!(!wire.contains("curl"));
        assert!(!wire.contains("transcript_path"));
        // What it *does* carry: enough to name the session and count the work.
        assert!(wire.contains("/home/user/project"));
        assert_eq!(event.project, "project");
        assert_eq!(event.background.len(), 1);
        assert_eq!(event.background[0].kind, "shell");
    }

    /// Display text is allowed through for Notification only, because a
    /// notification with no text is useless. Every other event drops it.
    #[test]
    fn display_text_crosses_only_for_notification() {
        let with_message = |hook: &str| {
            let raw = format!(
                r#"{{"hook_event_name":"{hook}","session_id":"s","cwd":"/tmp/p","message":"hello"}}"#
            );
            serde_json::from_str::<HookInput>(&raw)
                .expect("parses")
                .into_event()
                .expect("forwarded")
                .message
        };

        assert_eq!(with_message("Notification").as_deref(), Some("hello"));
        assert_eq!(with_message("Stop"), None);
        assert_eq!(with_message("PermissionRequest"), None);
    }

    /// Events we do not act on are dropped before the socket is touched.
    #[test]
    fn unknown_events_are_not_forwarded() {
        let raw = r#"{"hook_event_name":"PreToolUse","session_id":"s","cwd":"/tmp/p"}"#;
        let input: HookInput = serde_json::from_str(raw).expect("parses");
        assert!(input.into_event().is_none());
    }

    /// Claude Code adds fields to these payloads over time; an unknown one
    /// must never turn into a failed notification.
    #[test]
    fn unknown_fields_do_not_break_parsing() {
        let raw = r#"{"hook_event_name":"Stop","session_id":"s","cwd":"/tmp/p",
                      "some_field_invented_next_year": {"nested": true}}"#;
        let input: HookInput = serde_json::from_str(raw).expect("parses");
        assert!(input.into_event().is_some());
    }
}
