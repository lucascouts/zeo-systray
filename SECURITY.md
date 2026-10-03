# Security Policy

`zeo-systray` is a desktop tray daemon fed by Claude Code hooks. It runs as the
logged-in user, reads hook payloads on stdin, and talks to the desktop over the
session D-Bus. This document says what it is careful about and how to report a
vulnerability.

## Supported versions

Only the latest release is supported. Fixes are not backported.

## Reporting a vulnerability

Report privately via GitHub **Private vulnerability reporting** (Security tab →
_Report a vulnerability_) or by email to the maintainer (`lucascs@proton.me`).
Please do not open a public issue for a security report. Include the affected
version, a reproduction and the impact. Expect an initial acknowledgement within
a few days.

## What the design guards

- **Hook payloads stop at the socket.** A payload can carry the assistant's last
  message, tool inputs, shell commands and transcript paths. None of that crosses
  into the daemon: only identifiers, the working directory, a project label,
  background-task kinds and the notification text Claude wrote for display. A
  regression test asserts it (`secrets_in_the_hook_payload_never_reach_the_socket`
  in `src/protocol.rs`); see also "What crosses the socket" in the README.
- **The socket is the user's.** It lives in `$XDG_RUNTIME_DIR` (mode 0700) with
  mode 0600, and the daemon refuses to start rather than fall back to a
  world-readable directory when that variable is unset.
- **No shell.** The opener command is split on whitespace and executed directly;
  the session id and directory substituted into it never reach `sh -c`.
- **Clicks are bound to the server that issued them.** A notification action is
  accepted only from the D-Bus name that raised that notification, so an id
  recycled by a restarted notification server cannot open another session.
- **Sandboxed under systemd.** The shipped unit sets `ProtectHome=read-only`,
  `PrivateTmp`, `NoNewPrivileges` and `RestrictAddressFamilies=AF_UNIX`; the
  opener alone is launched outside it, through `systemd-run --user`.

## Supply chain

- `Cargo.lock` is committed; the Gentoo ebuild builds from it with every crate
  pinned by checksum in its Manifest.
- Dependabot proposes crate updates weekly with a 7-day release cooldown
  (`.github/dependabot.yml`). The cooldown covers the crates `Cargo.toml` names,
  not every transitive version.
- Releases run `cargo clippy`, the test suite, `cargo audit` and `gitleaks`
  locally before they are tagged. There is no CI in this repository, so those
  results are the maintainer's, not a public check.
