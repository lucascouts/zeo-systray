//! A desktop tray indicator for Claude Code agent sessions.
//!
//! One binary, two modes. `notify` is what a Claude Code hook runs: it reads
//! the hook payload on stdin and forwards a narrowed event. `daemon` owns the
//! tray icon and the desktop notifications.
//!
//! The split exists because the hook runs on the critical path of every agent
//! turn. Keeping that side stateless and short is what stops a broken tray
//! from ever slowing down — or failing — an agent session.

mod daemon;
mod icon;
mod notify;
mod protocol;
mod state;
mod tray;

use std::process::ExitCode;

const USAGE: &str = "\
zeo-systray — tray indicator for Claude Code agent sessions

USAGE:
    zeo-systray daemon [--demo]   run the tray (systemd user service)
    zeo-systray notify            forward one hook payload from stdin
    zeo-systray --version

The notify mode is meant to be called from a hook in settings.json:

    { \"type\": \"command\", \"command\": \"zeo-systray notify\" }
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str);

    match mode {
        Some("daemon") => {
            init_logging();
            let demo = args.iter().any(|arg| arg == "--demo");
            match daemon::run(demo) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    tracing::error!(%error, "daemon stopped");
                    ExitCode::FAILURE
                }
            }
        }
        Some("notify") => {
            init_logging();
            notify::run();
            // Always success: see the module docs. A tray problem must never
            // become an agent problem.
            ExitCode::SUCCESS
        }
        Some("--version" | "-V") => {
            println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("--help" | "-h") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        _ => {
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// Structured logs on stderr, which is where systemd picks them up for the
/// journal. `ZEO_SYSTRAY_LOG` overrides the level without a reinstall.
fn init_logging() {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_env("ZEO_SYSTRAY_LOG")
        .unwrap_or_else(|_| EnvFilter::new("zeo_systray=info"));

    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
