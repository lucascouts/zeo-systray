//! Opening the thing a menu entry points at.
//!
//! The default target is `zed://agent?session=<id>&cwd=<dir>`, the deep link
//! that reopens an agent thread by its session id — the same id the hook already
//! gives us, so nothing has to be looked up or mapped. The directory rides along
//! so Zed can pick the window holding that project for a thread it has no
//! record of yet; for one it knows, its own record decides.
//!
//! It is configurable because the link is not universal: it needs a Zed that
//! understands `?session=`, and someone running a different editor (or an
//! unpatched Zed) wants the working directory instead. `ZEO_SYSTRAY_OPEN_CMD`
//! takes a command line with `{session}`, `{cwd}` and `{cwd_query}`
//! placeholders — the last is the directory percent-encoded for a URL query,
//! since a path with a space, `&` or `+` would otherwise end the parameter or
//! change it.
//!
//! Under systemd the opener is launched through a transient unit rather than
//! as a child, because a child inherits the daemon's sandbox and the opener is
//! the one thing the daemon runs that must not be sandboxed: `kde-open` writes
//! `~/.config/kde-openrc` (read-only under `ProtectHome`), and an editor it
//! launches would inherit `PrivateTmp` and `RestrictAddressFamilies=AF_UNIX`,
//! which is an editor with no network.

use std::process::{Command, Stdio};

const OPEN_CMD_ENV: &str = "ZEO_SYSTRAY_OPEN_CMD";
const DEFAULT_OPEN_CMD: &str = "xdg-open zed://agent?session={session}&cwd={cwd_query}";

/// Spawns the configured opener for a session.
///
/// Split on whitespace into an argv and executed directly — never through a
/// shell. The values substituted in are a session id and a path, and a path can
/// contain anything; handing that to `sh -c` would be a command injection with
/// extra steps.
pub fn session(session_id: &str, cwd: &str) {
    let template = std::env::var(OPEN_CMD_ENV).unwrap_or_else(|_| DEFAULT_OPEN_CMD.to_string());

    let cwd_query = query_encode(cwd);
    let mut parts = template.split_whitespace().map(|part| {
        part.replace("{cwd_query}", &cwd_query)
            .replace("{session}", session_id)
            .replace("{cwd}", cwd)
    });

    let Some(program) = parts.next() else {
        tracing::warn!(env = OPEN_CMD_ENV, "open command is empty");
        return;
    };
    let args: Vec<String> = parts.collect();

    let (program, args) = escape_sandbox(program, args, under_systemd());

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

/// Percent-encodes a value for a URL query, keeping `/` readable.
///
/// Everything outside RFC 3986's unreserved set is encoded, byte by byte, so a
/// non-ASCII path survives as its UTF-8. `+` is encoded too: form decoding,
/// which is what Zed's URL handler uses, reads a bare `+` as a space.
fn query_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                encoded.push(char::from(byte));
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// `INVOCATION_ID` is set by systemd for every service it starts and by
/// nothing else, so its presence is the one reliable "we are inside a unit"
/// signal. Under OpenRC it is absent and the opener is spawned directly.
fn under_systemd() -> bool {
    std::env::var_os("INVOCATION_ID").is_some()
}

/// Wraps the opener in `systemd-run --user` when running as a systemd service.
///
/// The transient unit gets the user manager's defaults, not this unit's
/// sandbox. `--collect` so a failed opener leaves no unit behind, `--quiet` so
/// systemd-run's own "Running as unit" line stays out of the journal.
fn escape_sandbox(
    program: String,
    args: Vec<String>,
    under_systemd: bool,
) -> (String, Vec<String>) {
    if !under_systemd {
        return (program, args);
    }
    let mut wrapped: Vec<String> = ["--user", "--collect", "--quiet", "--"]
        .into_iter()
        .map(String::from)
        .collect();
    wrapped.push(program);
    wrapped.extend(args);
    ("systemd-run".to_string(), wrapped)
}

#[cfg(test)]
mod tests {
    use super::{escape_sandbox, query_encode};

    #[test]
    fn outside_systemd_the_opener_runs_as_given() {
        let (program, args) = escape_sandbox("xdg-open".into(), vec!["zed://x".into()], false);
        assert_eq!(program, "xdg-open");
        assert_eq!(args, vec!["zed://x"]);
    }

    /// The `--` matters: without it an opener whose first token starts with a
    /// dash would be parsed as a systemd-run option.
    #[test]
    fn under_systemd_the_opener_is_wrapped_in_a_transient_unit() {
        let (program, args) = escape_sandbox("xdg-open".into(), vec!["zed://x".into()], true);
        assert_eq!(program, "systemd-run");
        assert_eq!(
            args,
            vec![
                "--user",
                "--collect",
                "--quiet",
                "--",
                "xdg-open",
                "zed://x"
            ]
        );
    }

    /// The substitution is textual, so this pins the shape of the default link
    /// rather than trusting it to stay right by inspection.
    #[test]
    fn the_default_command_builds_the_expected_deep_link() {
        let built = super::DEFAULT_OPEN_CMD
            .replace("{session}", "abc-123")
            .replace("{cwd_query}", &query_encode("/home/me/project"));
        assert_eq!(
            built,
            "xdg-open zed://agent?session=abc-123&cwd=/home/me/project"
        );
    }

    /// Each of these would end or change the query parameter if left bare:
    /// a space splits nothing here but is not valid in a URL, `&` starts the
    /// next parameter, `+` decodes as a space and `%` as an escape.
    #[test]
    fn the_directory_is_encoded_for_a_query() {
        assert_eq!(
            query_encode("/home/me/my project&co+%/ção"),
            "/home/me/my%20project%26co%2B%25/%C3%A7%C3%A3o"
        );
    }
}
