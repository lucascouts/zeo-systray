//! The `notify` mode: what the hook actually runs.
//!
//! This runs on the critical path of every agent turn, so it does the least
//! possible work — read stdin, narrow it, send one datagram, exit. It never
//! connects, never retries and never blocks.
//!
//! It also **always exits 0**. A tray that is not running, a socket that was
//! never created, a payload shape we do not recognise: none of these are the
//! hook's problem, and a non-zero exit here would surface as a failure inside
//! the user's agent session. Failures are reported on stderr, where the hook
//! runner can show them if the user goes looking, and nowhere else.

use std::io::Read;
use std::os::unix::net::UnixDatagram;

use crate::protocol::{Event, HookInput, socket_path};

/// Cap on the hook payload we will read. `last_assistant_message` can be
/// large and we throw it away anyway; this bounds the work before parsing.
const MAX_INPUT_BYTES: u64 = 1024 * 1024;

/// Runs the notify mode. Returns nothing: every path is a success for the
/// caller, and diagnostics go to the log.
pub fn run() {
    let mut raw = String::new();
    if let Err(error) = std::io::stdin()
        .take(MAX_INPUT_BYTES)
        .read_to_string(&mut raw)
    {
        tracing::warn!(%error, "could not read hook payload from stdin");
        return;
    }

    let input: HookInput = match serde_json::from_str(&raw) {
        Ok(input) => input,
        Err(error) => {
            tracing::warn!(%error, "hook payload is not the JSON we expect");
            return;
        }
    };

    let hook_event = input.hook_event_name.clone();
    let Some(event) = input.into_event() else {
        tracing::debug!(hook_event, "event not forwarded");
        return;
    };

    if let Err(error) = send(&event) {
        // The overwhelmingly common cause is "the daemon is not running",
        // which is a normal state, not a fault.
        tracing::debug!(%error, hook_event, "could not reach the tray daemon");
    }
}

fn send(event: &Event) -> Result<(), Box<dyn std::error::Error>> {
    let path = socket_path().ok_or("XDG_RUNTIME_DIR is unset or empty")?;
    let payload = serde_json::to_vec(event)?;
    let socket = UnixDatagram::unbound()?;
    socket.send_to(&payload, &path)?;
    Ok(())
}
