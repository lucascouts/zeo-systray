//! Plan usage limits: the five-hour window, the weekly one, and the per-model
//! weekly buckets (Fable, today).
//!
//! No hook carries these numbers, so they arrive by two other roads, and both
//! end as the same [`UsageReport`] datagram on the tray socket:
//!
//! - **The status line.** Claude Code pipes `rate_limits` to the `statusLine`
//!   command after every API response. `zeo-systray statusline` forwards it.
//!   This covers every session that draws a terminal UI — a terminal, and the
//!   TUI adapter, which runs the real `claude` in a PTY. It carries the
//!   five-hour and weekly windows only; the per-model buckets are not in it.
//! - **The ACP adapter's shared sample.** `claude-agent-acp-plus` writes the
//!   structured `/usage` report to `$XDG_RUNTIME_DIR/claude-acp-quota-*.json`
//!   at the end of every turn and on its idle poll. The daemon watches that
//!   file. It is the only road the per-model buckets travel.
//!
//! Neither road needs this program to hold a credential or open a network
//! socket, which is why the unit can keep `RestrictAddressFamilies=AF_UNIX`.
//!
//! The limits are a fact about an account, so every source reports the same
//! windows, at slightly different instants. [`UsageTracker`] latches the
//! highest step it has announced per window instance, which is what keeps two
//! sources disagreeing by a point from announcing the same step twice.

use std::collections::HashMap;
use std::io::Read;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::protocol::socket_path;

/// Key of the five-hour window. Everything else is weekly.
const FIVE_HOUR: &str = "five_hour";
const SEVEN_DAY: &str = "seven_day";

/// Notification step, in percentage points. The five-hour window burns fast
/// enough that 10 points can pass in one long turn; the weekly ones do not.
const FIVE_HOUR_STEP: f64 = 5.0;
const WEEKLY_STEP: f64 = 10.0;

/// How much later a `resets_at` must be to count as a new window rather than
/// the same one reported with jitter. A new window only starts after the old
/// one resets and lasts at least five hours, so an hour is far from both.
const NEW_WINDOW_AFTER_SECS: u64 = 3_600;

/// One limit window, in the shape both roads are narrowed to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageWindow {
    /// `five_hour`, `seven_day`, or `model:<name>` for a per-model bucket.
    pub key: String,
    /// What the user reads: `5-hour`, `Weekly`, `Fable`.
    pub label: String,
    /// Percentage used, 0-100.
    pub percent: f64,
    /// Unix seconds when the window resets.
    pub resets_at: u64,
}

/// The datagram. A single field, so it can never be mistaken for an `Event`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageReport {
    pub usage: Vec<UsageWindow>,
}

/// A step crossed, worth a desktop notification.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageAlert {
    pub window: UsageWindow,
    /// The step reached, e.g. 45 for the five-hour window at 47%.
    pub step: u32,
}

#[derive(Debug, Clone)]
struct Tracked {
    latest: UsageWindow,
    /// Highest step already announced for this window instance.
    announced: u32,
}

#[derive(Debug, Default)]
pub struct UsageTracker {
    windows: HashMap<String, Tracked>,
}

impl UsageTracker {
    /// Folds one report in and returns the steps it crossed.
    ///
    /// The first sample of a window the daemon has never seen sets the
    /// baseline silently: at startup that is a window already in progress, and
    /// announcing "40%" at login would be news about the past. A window seen to
    /// *reset* has no such excuse, so its first sample is judged from zero.
    pub fn observe(&mut self, report: UsageReport, now: u64) -> Vec<UsageAlert> {
        let mut alerts = Vec::new();
        for window in report.usage {
            // A window whose reset has passed is a sample from before it: the
            // source is stale, and the number describes nothing current.
            if window.resets_at <= now || !window.percent.is_finite() {
                continue;
            }
            let step = step_reached(&window);

            let Some(tracked) = self.windows.get_mut(&window.key) else {
                self.windows.insert(
                    window.key.clone(),
                    Tracked {
                        latest: window,
                        announced: step,
                    },
                );
                continue;
            };

            let known = tracked.latest.resets_at;
            if window.resets_at > known + NEW_WINDOW_AFTER_SECS || known <= now {
                tracked.announced = 0;
            } else if window.resets_at + NEW_WINDOW_AFTER_SECS < known {
                // An earlier reset than the one we track: a source still
                // describing the previous window. Newer knowledge wins.
                continue;
            }

            if step > tracked.announced {
                tracked.announced = step;
                alerts.push(UsageAlert {
                    window: window.clone(),
                    step,
                });
            }
            tracked.latest = window;
        }
        alerts
    }

    /// Current windows, five-hour first, then weekly, then per-model by name.
    /// Expired ones are left out: after a reset the old number is wrong.
    pub fn current(&self, now: u64) -> Vec<&UsageWindow> {
        let mut windows: Vec<&UsageWindow> = self
            .windows
            .values()
            .map(|tracked| &tracked.latest)
            .filter(|window| window.resets_at > now)
            .collect();
        windows.sort_by_key(|window| {
            let rank = match window.key.as_str() {
                FIVE_HOUR => 0,
                SEVEN_DAY => 1,
                _ => 2,
            };
            (rank, window.key.clone())
        });
        windows
    }
}

fn step_reached(window: &UsageWindow) -> u32 {
    let step = if window.key == FIVE_HOUR {
        FIVE_HOUR_STEP
    } else {
        WEEKLY_STEP
    };
    // Percent is 0-100 and the cast saturates, so a malformed 1e9 is 100%-ish
    // noise rather than a panic. Capped at 100: overage can report past it.
    let percent = window.percent.clamp(0.0, 100.0);
    ((percent / step).floor() * step) as u32
}

/// `2h 13m`, `3d 4h`, `12m` — until a reset. Relative, for the same reason the
/// history is: a clock time would need the local UTC offset.
pub fn until(resets_at: u64, now: u64) -> String {
    let secs = resets_at.saturating_sub(now);
    let (days, hours, minutes) = (secs / 86_400, secs % 86_400 / 3_600, secs % 3_600 / 60);
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Narrows the status line payload. Only `rate_limits` is read: the rest of it
/// holds the working directory, the transcript path and the model, none of
/// which the tray needs from this road.
pub fn from_statusline(payload: &Value) -> UsageReport {
    let limits = &payload["rate_limits"];
    let usage = [(FIVE_HOUR, "5-hour"), (SEVEN_DAY, "Weekly")]
        .into_iter()
        .filter_map(|(key, label)| {
            let window = &limits[key];
            Some(UsageWindow {
                key: key.into(),
                label: label.into(),
                percent: window["used_percentage"].as_f64()?,
                resets_at: window["resets_at"].as_u64()?,
            })
        })
        .collect();
    UsageReport { usage }
}

/// Narrows the adapter's shared sample. Its `utilization` is already 0-100 and
/// its `resets_at` an ISO 8601 instant; `model_scoped` holds Fable and friends.
pub fn from_quota_cache(sample: &Value) -> UsageReport {
    let limits = &sample["rate_limits"];
    let window = |key: String, label: String, raw: &Value| {
        Some(UsageWindow {
            key,
            label,
            percent: raw["utilization"].as_f64()?,
            resets_at: parse_iso8601(raw["resets_at"].as_str()?)?,
        })
    };

    let mut usage: Vec<UsageWindow> = [(FIVE_HOUR, "5-hour"), (SEVEN_DAY, "Weekly")]
        .into_iter()
        .filter_map(|(key, label)| window(key.into(), label.into(), &limits[key]))
        .collect();

    if let Some(models) = limits["model_scoped"].as_array() {
        usage.extend(models.iter().filter_map(|model| {
            let name = model["display_name"].as_str()?;
            window(format!("model:{name}"), name.to_string(), model)
        }));
    }
    UsageReport { usage }
}

/// Parses `YYYY-MM-DDTHH:MM:SS[.fff](Z|±HH:MM)` into Unix seconds. That is all
/// the adapter writes (`Date.toISOString`), and a date crate for one format is
/// not worth the dependency.
pub fn parse_iso8601(text: &str) -> Option<u64> {
    let (date, time) = text.split_once('T')?;
    let mut date = date.splitn(3, '-').map(str::parse::<i64>);
    let (year, month, day) = (date.next()?.ok()?, date.next()?.ok()?, date.next()?.ok()?);

    let (clock, offset) = if let Some(clock) = time.strip_suffix('Z') {
        (clock, 0)
    } else {
        let at = time.rfind(['+', '-'])?;
        let (clock, zone) = time.split_at(at);
        let sign = if zone.starts_with('-') { -1 } else { 1 };
        let (hours, minutes) = zone[1..].split_once(':')?;
        (
            clock,
            sign * (hours.parse::<i64>().ok()? * 3_600 + minutes.parse::<i64>().ok()? * 60),
        )
    };
    let clock = clock.split('.').next()?;
    let mut clock = clock.splitn(3, ':').map(str::parse::<i64>);
    let (hour, minute, second) = (
        clock.next()?.ok()?,
        clock.next()?.ok()?,
        clock.next()?.ok()?,
    );

    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3_600 + minute * 60 + second - offset;
    u64::try_from(secs).ok()
}

/// Days since 1970-01-01 in the proleptic Gregorian calendar (H. Hinnant).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Cap on what either road reads. Both payloads are a few KB.
const MAX_INPUT_BYTES: u64 = 1024 * 1024;

/// The `statusline` mode. Forwards the windows and prints a short line, since
/// whatever a status line command prints is what Claude Code draws.
///
/// Like `notify` it always succeeds: this runs on every render of every
/// session, and a tray that is down must not blank anyone's status line.
/// `--quiet` prints nothing, for chaining behind another status line script.
pub fn run_statusline(quiet: bool) {
    let mut raw = String::new();
    if let Err(error) = std::io::stdin()
        .take(MAX_INPUT_BYTES)
        .read_to_string(&mut raw)
    {
        tracing::warn!(%error, "could not read status line payload from stdin");
        return;
    }
    let payload: Value = match serde_json::from_str(&raw) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::warn!(%error, "status line payload is not JSON");
            return;
        }
    };

    let report = from_statusline(&payload);
    if !quiet {
        println!("{}", statusline_text(&report));
    }
    if report.usage.is_empty() {
        // API-key sessions, and every session before its first response.
        return;
    }
    if let Err(error) = send(&report) {
        tracing::debug!(%error, "could not reach the tray daemon");
    }
}

fn statusline_text(report: &UsageReport) -> String {
    report
        .usage
        .iter()
        .map(|window| {
            let short = if window.key == FIVE_HOUR { "5h" } else { "7d" };
            format!("{short} {:.0}%", window.percent)
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

fn send(report: &UsageReport) -> Result<(), Box<dyn std::error::Error>> {
    let path = socket_path().ok_or("XDG_RUNTIME_DIR is unset or empty")?;
    let payload = serde_json::to_vec(report)?;
    UnixDatagram::unbound()?.send_to(&payload, &path)?;
    Ok(())
}

/// How often the adapter's sample is checked. A stat per tick; the file itself
/// is only read when its mtime moves.
const CACHE_POLL: Duration = Duration::from_secs(5);
const CACHE_PREFIX: &str = "claude-acp-quota-";

/// Watches the adapter's shared sample and feeds it to the daemon's own socket.
///
/// Through the socket rather than into the state directly, so one loop owns the
/// state and every source is handled by the same code path.
pub fn watch_quota_cache() {
    let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty()) else {
        return;
    };
    let dir = PathBuf::from(dir);
    let mut seen: Option<(PathBuf, SystemTime)> = None;
    loop {
        if let Some(newest) = newest_sample(&dir)
            && seen.as_ref() != Some(&newest)
        {
            match read_sample(&newest.0) {
                Ok(report) if !report.usage.is_empty() => {
                    if let Err(error) = send(&report) {
                        tracing::warn!(%error, "could not forward the quota sample");
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::debug!(%error, path = %newest.0.display(), "quota sample unreadable");
                }
            }
            seen = Some(newest);
        }
        std::thread::sleep(CACHE_POLL);
    }
}

/// The most recently written sample. One file per config scope; with more than
/// one, the freshest is the account being used right now.
fn newest_sample(dir: &Path) -> Option<(PathBuf, SystemTime)> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // `.tmp` is the adapter's write-then-rename staging file.
            name.starts_with(CACHE_PREFIX) && name.ends_with(".json")
        })
        .filter_map(|entry| Some((entry.path(), entry.metadata().ok()?.modified().ok()?)))
        .max_by_key(|(_, modified)| *modified)
}

fn read_sample(path: &Path) -> Result<UsageReport, Box<dyn std::error::Error>> {
    let mut raw = String::new();
    std::fs::File::open(path)?
        .take(MAX_INPUT_BYTES)
        .read_to_string(&mut raw)?;
    Ok(from_quota_cache(&serde_json::from_str(&raw)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    fn window(key: &str, percent: f64, resets_at: u64) -> UsageWindow {
        UsageWindow {
            key: key.into(),
            label: key.into(),
            percent,
            resets_at,
        }
    }

    fn report(windows: Vec<UsageWindow>) -> UsageReport {
        UsageReport { usage: windows }
    }

    fn steps(alerts: &[UsageAlert]) -> Vec<(String, u32)> {
        alerts
            .iter()
            .map(|alert| (alert.window.key.clone(), alert.step))
            .collect()
    }

    #[test]
    fn first_sample_sets_the_baseline_silently() {
        let mut tracker = UsageTracker::default();
        let alerts = tracker.observe(report(vec![window(FIVE_HOUR, 42.0, NOW + 100)]), NOW);
        assert!(alerts.is_empty());
    }

    #[test]
    fn five_hour_steps_every_five_and_weekly_every_ten() {
        let mut tracker = UsageTracker::default();
        let reset = NOW + 10_000;
        tracker.observe(
            report(vec![
                window(FIVE_HOUR, 1.0, reset),
                window(SEVEN_DAY, 1.0, reset),
            ]),
            NOW,
        );
        let alerts = tracker.observe(
            report(vec![
                window(FIVE_HOUR, 6.0, reset),
                window(SEVEN_DAY, 6.0, reset),
            ]),
            NOW,
        );
        assert_eq!(steps(&alerts), vec![(FIVE_HOUR.into(), 5)]);

        let alerts = tracker.observe(report(vec![window(SEVEN_DAY, 11.0, reset)]), NOW);
        assert_eq!(steps(&alerts), vec![(SEVEN_DAY.into(), 10)]);
    }

    #[test]
    fn a_jump_across_several_steps_announces_once_at_the_highest() {
        let mut tracker = UsageTracker::default();
        tracker.observe(report(vec![window(FIVE_HOUR, 2.0, NOW + 100)]), NOW);
        let alerts = tracker.observe(report(vec![window(FIVE_HOUR, 23.0, NOW + 100)]), NOW);
        assert_eq!(steps(&alerts), vec![(FIVE_HOUR.into(), 20)]);
    }

    /// Measured on the adapter's sample: 15% -> 18% -> 15% inside two minutes
    /// with nothing prompted. An oscillation across a step must not re-announce.
    #[test]
    fn an_oscillation_across_a_step_announces_it_once() {
        let mut tracker = UsageTracker::default();
        tracker.observe(report(vec![window(FIVE_HOUR, 14.0, NOW + 100)]), NOW);
        let first = tracker.observe(report(vec![window(FIVE_HOUR, 15.5, NOW + 100)]), NOW);
        let down = tracker.observe(report(vec![window(FIVE_HOUR, 14.5, NOW + 100)]), NOW);
        let again = tracker.observe(report(vec![window(FIVE_HOUR, 15.5, NOW + 100)]), NOW);
        assert_eq!(first.len(), 1);
        assert!(down.is_empty());
        assert!(again.is_empty());
    }

    #[test]
    fn a_new_window_starts_counting_from_zero() {
        let mut tracker = UsageTracker::default();
        tracker.observe(report(vec![window(FIVE_HOUR, 90.0, NOW + 100)]), NOW);
        let later = NOW + 200;
        let alerts = tracker.observe(report(vec![window(FIVE_HOUR, 7.0, later + 18_000)]), later);
        assert_eq!(steps(&alerts), vec![(FIVE_HOUR.into(), 5)]);
    }

    #[test]
    fn jitter_in_resets_at_is_the_same_window() {
        let mut tracker = UsageTracker::default();
        tracker.observe(report(vec![window(FIVE_HOUR, 12.0, NOW + 5_000)]), NOW);
        let alerts = tracker.observe(report(vec![window(FIVE_HOUR, 12.0, NOW + 5_030)]), NOW);
        assert!(alerts.is_empty(), "same window, same step");
    }

    #[test]
    fn a_stale_source_describing_the_previous_window_is_ignored() {
        let mut tracker = UsageTracker::default();
        tracker.observe(report(vec![window(FIVE_HOUR, 3.0, NOW + 18_000)]), NOW);
        tracker.observe(report(vec![window(FIVE_HOUR, 80.0, NOW + 100)]), NOW);
        let current = tracker.current(NOW);
        assert_eq!(current[0].percent, 3.0);
    }

    #[test]
    fn expired_windows_are_neither_announced_nor_shown() {
        let mut tracker = UsageTracker::default();
        let alerts = tracker.observe(report(vec![window(FIVE_HOUR, 99.0, NOW - 1)]), NOW);
        assert!(alerts.is_empty());
        assert!(tracker.current(NOW).is_empty());
    }

    #[test]
    fn overage_past_one_hundred_caps_at_one_hundred() {
        let mut tracker = UsageTracker::default();
        tracker.observe(report(vec![window(FIVE_HOUR, 97.0, NOW + 100)]), NOW);
        let alerts = tracker.observe(report(vec![window(FIVE_HOUR, 130.0, NOW + 100)]), NOW);
        assert_eq!(steps(&alerts), vec![(FIVE_HOUR.into(), 100)]);
    }

    #[test]
    fn current_orders_five_hour_weekly_then_models() {
        let mut tracker = UsageTracker::default();
        tracker.observe(
            report(vec![
                window("model:Fable", 1.0, NOW + 9),
                window(SEVEN_DAY, 1.0, NOW + 9),
                window(FIVE_HOUR, 1.0, NOW + 9),
            ]),
            NOW,
        );
        let keys: Vec<_> = tracker.current(NOW).iter().map(|w| w.key.clone()).collect();
        assert_eq!(keys, vec![FIVE_HOUR, SEVEN_DAY, "model:Fable"]);
    }

    #[test]
    fn statusline_payload_is_narrowed_to_the_two_windows() {
        let payload: Value = serde_json::from_str(
            r#"{"cwd":"/home/u/secret-project","transcript_path":"/x.jsonl",
                "rate_limits":{"five_hour":{"used_percentage":45.2,"resets_at":1800000900},
                               "seven_day":{"used_percentage":18,"resets_at":1800400000}}}"#,
        )
        .expect("parses");
        let report = from_statusline(&payload);
        assert_eq!(report.usage.len(), 2);
        assert_eq!(report.usage[0].key, FIVE_HOUR);
        assert_eq!(report.usage[0].resets_at, 1_800_000_900);
        let wire = serde_json::to_string(&report).expect("serialises");
        assert!(!wire.contains("secret-project"));
        assert!(!wire.contains("transcript"));
        assert_eq!(statusline_text(&report), "5h 45% · 7d 18%");
    }

    #[test]
    fn statusline_without_rate_limits_forwards_nothing() {
        let payload: Value = serde_json::from_str(r#"{"cwd":"/tmp"}"#).expect("parses");
        assert!(from_statusline(&payload).usage.is_empty());
    }

    /// The shape the adapter writes, per `quota-cache.ts` and the SDK's
    /// `rate_limits` declaration.
    #[test]
    fn quota_cache_carries_the_per_model_buckets() {
        let sample: Value = serde_json::from_str(
            r#"{"fetchedAt":1,"rate_limits_available":true,"rate_limits":{
                "five_hour":{"utilization":12,"resets_at":"2026-09-30T02:00:00.000Z"},
                "seven_day":{"utilization":42,"resets_at":"2026-10-04T14:00:00.000Z"},
                "seven_day_opus":null,
                "model_scoped":[{"display_name":"Fable","utilization":31,
                                 "resets_at":"2026-10-04T14:00:00.000Z"}]}}"#,
        )
        .expect("parses");
        let report = from_quota_cache(&sample);
        let keys: Vec<_> = report.usage.iter().map(|w| w.key.as_str()).collect();
        assert_eq!(keys, vec![FIVE_HOUR, SEVEN_DAY, "model:Fable"]);
        assert_eq!(report.usage[2].label, "Fable");
        assert_eq!(report.usage[2].percent, 31.0);
    }

    #[test]
    fn iso8601_parses_what_date_to_iso_string_writes() {
        assert_eq!(parse_iso8601("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(
            parse_iso8601("2026-10-04T14:00:00.000Z"),
            Some(1_791_122_400)
        );
        assert_eq!(parse_iso8601("2026-10-04T14:00:00Z"), Some(1_791_122_400));
        assert_eq!(
            parse_iso8601("2026-10-04T11:00:00-03:00"),
            Some(1_791_122_400)
        );
        assert_eq!(parse_iso8601("2024-02-29T00:00:00Z"), Some(1_709_164_800));
        assert_eq!(parse_iso8601("not a date"), None);
        assert_eq!(parse_iso8601("2026-13-01T00:00:00Z"), None);
    }

    #[test]
    fn until_reads_as_a_duration() {
        assert_eq!(until(NOW + 7_980, NOW), "2h 13m");
        assert_eq!(until(NOW + 3 * 86_400 + 4 * 3_600, NOW), "3d 4h");
        assert_eq!(until(NOW + 720, NOW), "12m");
        assert_eq!(until(NOW - 5, NOW), "0m");
    }
}
