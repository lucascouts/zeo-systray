//! Opening the thing a menu entry points at.
//!
//! The default target is `zed://agent?session=<id>`, the deep link that reopens
//! an agent thread by its session id — the same id the hook already gives us, so
//! nothing has to be looked up or mapped.
//!
//! It is configurable because the link is not universal: it needs a Zed that
//! understands `?session=`, and someone running a different editor (or an
//! unpatched Zed) wants the working directory instead. `ZEO_SYSTRAY_OPEN_CMD`
//! takes a command line with `{session}` and `{cwd}` placeholders.

use std::process::{Command, Stdio};

const OPEN_CMD_ENV: &str = "ZEO_SYSTRAY_OPEN_CMD";
const DEFAULT_OPEN_CMD: &str = "xdg-open zed://agent?session={session}";

/// Spawns the configured opener for a session.
///
/// Split on whitespace into an argv and executed directly — never through a
/// shell. The values substituted in are a session id and a path, and a path can
/// contain anything; handing that to `sh -c` would be a command injection with
/// extra steps.
pub fn session(session_id: &str, cwd: &str) {
    let template = std::env::var(OPEN_CMD_ENV).unwrap_or_else(|_| DEFAULT_OPEN_CMD.to_string());

    let mut parts = template
        .split_whitespace()
        .map(|part| part.replace("{session}", session_id).replace("{cwd}", cwd));

    let Some(program) = parts.next() else {
        tracing::warn!(env = OPEN_CMD_ENV, "open command is empty");
        return;
    };
    let args: Vec<String> = parts.collect();

    tracing::info!(program = %program, session = %session_id, "opening session");

    // Detached and silent: the tray must not gain a zombie child, and the
    // opener's own chatter has no business in the journal.
    match Command::new(&program)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            // Reap on a thread so the daemon never blocks on the editor.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(error) => tracing::warn!(%error, program = %program, "could not run the opener"),
    }
}

#[cfg(test)]
mod tests {
    /// The substitution is textual, so this pins the shape of the default link
    /// rather than trusting it to stay right by inspection.
    #[test]
    fn the_default_command_builds_the_expected_deep_link() {
        let built = super::DEFAULT_OPEN_CMD.replace("{session}", "abc-123");
        assert_eq!(built, "xdg-open zed://agent?session=abc-123");
    }
}
