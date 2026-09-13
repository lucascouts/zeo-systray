# zeo-systray

A desktop tray indicator for **Claude Code** agent sessions. It shows what your
agents are doing — running, waiting on you, finished, or parked on background
work — without you having to keep the window in front of you.

It is an unofficial project, not affiliated with or endorsed by Anthropic.

> Despite the name, this does **not** require the `zeo` editor, or any editor.
> It is fed by Claude Code hooks, so it covers every session: your editor's
> agent panel and the ones you start in a terminal, equally.

## How it works

```
~/.claude/settings.json hook  →  zeo-systray notify   (reads the hook payload on stdin)
                                      ↓ datagram, $XDG_RUNTIME_DIR/zeo-systray.sock
                                 zeo-systray daemon   (tray icon + desktop notifications)
```

One binary, two modes. The `notify` mode runs on the critical path of every
agent turn, so it is deliberately trivial: read stdin, send one datagram, exit.
It **always exits 0** — a tray that is not running must never surface as a
failure inside an agent session.

The daemon keeps state in memory only. Nothing is written to disk; the
transcripts are already the record.

## What the icon means

The icon has two layers. The **base** is the identity — it never changes shape,
so you can find the tray at a glance. A small **overlay badge** carries the
status:

| State | Badge | Meaning |
|---|---|---|
| Needs you | `dialog-question` | a permission request, or Claude asking for input |
| Failed | `dialog-error` | the turn ended with an error |
| Background | `system-run` | the turn finished, but background work is still running |
| Running | *(none)* | the agent is working |
| Idle | *(none)* | nothing in flight |

Running and idle carry no badge on purpose: a marker on every single turn is
noise, and what deserves a glance is the states that are *not* business as usual.

With several sessions open the badge shows the **worst** state, because that is
the one with a person blocked on it. The tooltip breaks down the counts, and
`NeedsAttention` is reserved for the one state where someone is actually stuck —
it is what makes Plasma highlight the item.

Badges are freedesktop theme names, so they follow your theme, light or dark.

### Choosing the base icon

The bundled mark is the default. Two environment variables change it, both read
once at startup:

| Variable | Effect |
|---|---|
| `ZEO_SYSTRAY_ICON` | any icon name in your theme, e.g. `dev.zed.Zed-Nightly` |
| `ZEO_SYSTRAY_ICON_PATH` | extra directory searched for icons — lets the daemon run from a build tree before anything is installed |

A caveat worth knowing before you switch: an application icon with an opaque
rounded background tends to swallow the overlay badge. Measured on Plasma with
`dev.zed.Zed-Nightly` — the base renders fine, the badge does not show. The
bundled mark is transparent SVG, so the badge lands cleanly on its corner.

## Install

```sh
cargo build --release
install -Dm755 target/release/zeo-systray ~/.local/bin/zeo-systray
install -Dm644 assets/zeo-systray.svg ~/.local/share/icons/hicolor/scalable/apps/zeo-systray.svg
install -Dm644 contrib/zeo-systray.service ~/.config/systemd/user/zeo-systray.service
systemctl --user enable --now zeo-systray.service
```

Then add the hooks. See `contrib/hooks.example.json` — **append** to the arrays
already in your `~/.claude/settings.json` rather than replacing them, or you
will drop whatever else you run on those events.

To see the icon before installing or wiring anything up, straight from the
build tree:

```sh
ZEO_SYSTRAY_ICON_PATH="$PWD/assets/theme" cargo run -- daemon --demo
```

## Desktop support

It speaks [StatusNotifierItem](https://www.freedesktop.org/wiki/Specifications/StatusNotifierItem/)
over D-Bus directly — no GTK, no libappindicator.

| Desktop | Status |
|---|---|
| KDE Plasma | works out of the box; SNI is theirs |
| XFCE, MATE, Cinnamon, Budgie, LXQt | supported |
| Wayland | irrelevant — SNI is D-Bus, not a display protocol |
| **GNOME** | **needs an extension**: [AppIndicator Support](https://extensions.gnome.org/extension/615/appindicator-support/) or [Status Tray](https://extensions.gnome.org/extension/9164/status-tray/) |

GNOME removed built-in tray support, so *every* tray application needs that
extension. There is nothing this program can do about it.

## What crosses the socket, and what does not

A hook payload can carry `last_assistant_message`, tool inputs, a shell task's
full command line and the transcript path — any of which may hold a token or
personal data. **None of that is forwarded.** The datagram carries only:

- the session id and working directory,
- a project label derived from that directory,
- the session title when Claude Code supplies one,
- counters and type labels for background tasks (`shell`, `subagent`, …) —
  never the command itself,
- on `Notification` only, the display text Claude itself wrote, because a
  notification with no text is useless.

This boundary is enforced by tests (`src/protocol.rs`), not only by review.

The socket lives in `$XDG_RUNTIME_DIR` — per-user, mode 0600, never TCP.

## Known rough edges

- Killing the daemon leaves the socket file behind. Harmless: the next start
  removes a stale socket before binding.
- Session entries in the menu are inert. Opening a session's directory is the
  obvious next step and is marked `TODO` in `src/tray.rs`.
- Hooks are a Claude Code mechanism, so sessions from other agents (Codex,
  Gemini, an editor's own built-in agent) are not covered.

## Development

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo audit
```
