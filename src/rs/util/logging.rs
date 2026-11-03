//! Opt-in structured logging: one JSON object per line on stderr.
//!
//! Off unless the `HERMES_LOG` environment variable is set, so default output is unchanged.

use std::io::Write as _;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use log::{LevelFilter, Log, Metadata, Record};

/// Environment variable that enables logging and selects the level.
pub const LOG_ENV: &str = "HERMES_LOG";

/// Parse `error|warn|info|debug|trace` (ASCII case-insensitive).
#[must_use]
pub fn parse_level(text: &str) -> Option<LevelFilter> {
    match text.trim().to_ascii_lowercase().as_str() {
        "error" => Some(LevelFilter::Error),
        "warn" => Some(LevelFilter::Warn),
        "info" => Some(LevelFilter::Info),
        "debug" => Some(LevelFilter::Debug),
        "trace" => Some(LevelFilter::Trace),
        _ => None,
    }
}

/// RFC 3339 UTC timestamp with millisecond precision.
#[must_use]
pub fn format_timestamp(time: SystemTime) -> String {
    let (secs, millis) = time.duration_since(UNIX_EPOCH).map_or((0, 0), |after| {
        (
            i64::try_from(after.as_secs()).unwrap_or(i64::MAX / 2),
            after.subsec_millis(),
        )
    });
    let days = secs.div_euclid(86_400);
    let rest = secs.rem_euclid(86_400);
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

/// One log line (without the trailing newline).
#[must_use]
pub fn format_line(ts: &str, level: &str, target: &str, msg: &str, bin: &str) -> String {
    serde_json::json!({
        "ts": ts,
        "level": level,
        "target": target,
        "msg": msg,
        "bin": bin,
    })
    .to_string()
}

struct JsonLogger {
    bin: String,
    max: LevelFilter,
    /// Duplicated stderr descriptor, written without `Stderr`'s lock: entry points hold that lock
    /// for the whole run, so a worker thread logging through it would block until the run ends.
    sink: Option<std::fs::File>,
}

/// Duplicate the stderr descriptor so log writes bypass the process-wide `Stderr` lock.
fn stderr_sink() -> Option<std::fs::File> {
    #[cfg(unix)]
    let owned = std::os::fd::AsFd::as_fd(&std::io::stderr()).try_clone_to_owned();
    #[cfg(windows)]
    let owned = std::os::windows::io::AsHandle::as_handle(&std::io::stderr()).try_clone_to_owned();
    owned.ok().map(std::fs::File::from)
}

impl Log for JsonLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= self.max
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let mut line = format_line(
            &format_timestamp(SystemTime::now()),
            record.level().as_str(),
            record.target(),
            &record.args().to_string(),
            &self.bin,
        );
        line.push('\n');
        // No sink: drop the record. Falling back to `Stderr` would block on the lock the entry
        // points hold for the whole run.
        if let Some(file) = self.sink.as_ref() {
            let _ = (&*file).write_all(line.as_bytes());
        }
    }

    fn flush(&self) {}
}

static LOGGER: OnceLock<JsonLogger> = OnceLock::new();

/// Level selected by an environment value; `None` means logging stays off.
///
/// Unset or empty: off. Invalid: warn level, with the second element set so the caller can warn.
#[must_use]
pub fn level_from_env(value: Option<&str>) -> Option<(LevelFilter, bool)> {
    let text = value?;
    if text.trim().is_empty() {
        return None;
    }
    Some(parse_level(text).map_or((LevelFilter::Warn, true), |level| (level, false)))
}

/// Install the JSON logger once when `HERMES_LOG` is set; otherwise do nothing.
pub fn init(bin: &str) {
    let value = std::env::var(LOG_ENV).ok();
    let Some((max, invalid)) = level_from_env(value.as_deref()) else {
        return;
    };
    if invalid {
        let _ = writeln!(
            std::io::stderr(),
            "{bin}: {LOG_ENV} must be one of error|warn|info|debug|trace; using warn"
        );
    }
    let logger = LOGGER.get_or_init(|| JsonLogger {
        bin: bin.to_owned(),
        max,
        sink: stderr_sink(),
    });
    if log::set_logger(logger).is_ok() {
        log::set_max_level(max);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn levels_parse() {
        assert_eq!(parse_level("info"), Some(LevelFilter::Info));
        assert_eq!(parse_level("TRACE"), Some(LevelFilter::Trace));
        assert_eq!(parse_level(" error "), Some(LevelFilter::Error));
        assert_eq!(parse_level("verbose"), None);
        assert_eq!(parse_level(""), None);
    }

    #[test]
    fn env_levels() {
        assert_eq!(level_from_env(None), None);
        assert_eq!(level_from_env(Some("")), None);
        assert_eq!(
            level_from_env(Some("debug")),
            Some((LevelFilter::Debug, false))
        );
        assert_eq!(
            level_from_env(Some("loud")),
            Some((LevelFilter::Warn, true))
        );
    }

    #[test]
    fn timestamps() {
        assert_eq!(format_timestamp(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        let leap = UNIX_EPOCH + Duration::new(1_709_210_096, 42_000_000);
        assert_eq!(format_timestamp(leap), "2024-02-29T12:34:56.042Z");
        let late = UNIX_EPOCH + Duration::from_millis(1_793_404_800_999);
        assert_eq!(format_timestamp(late), "2026-10-31T00:00:00.999Z");
    }

    #[test]
    fn line_escapes_and_has_keys() {
        let line = format_line("t", "INFO", "a::b", "quote \" nl \n tab \t \\ \u{1}", "bin");
        assert!(!line.contains('\n'));
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["msg"], "quote \" nl \n tab \t \\ \u{1}");
        for key in ["ts", "level", "target", "msg", "bin"] {
            assert!(value.get(key).is_some(), "{key}");
        }
    }
}
