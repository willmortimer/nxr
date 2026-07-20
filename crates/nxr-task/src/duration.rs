//! Human-friendly duration strings for task timeouts (`10m`, `5s`, …).

use std::time::Duration;

use thiserror::Error;

/// Errors while parsing a duration string.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum DurationParseError {
    /// Empty or whitespace-only input.
    #[error("duration must not be empty")]
    Empty,
    /// Unrecognized format.
    #[error("invalid duration `{value}` (expected e.g. `500ms`, `5s`, `10m`, `1h`)")]
    Invalid { value: String },
}

/// Parse a compact duration: `ms` / `s` / `m` / `h` suffix, or bare seconds.
///
/// # Errors
///
/// Returns [`DurationParseError`] when the string is empty or malformed.
pub fn parse_duration(raw: &str) -> Result<Duration, DurationParseError> {
    let value = raw.trim();
    if value.is_empty() {
        return Err(DurationParseError::Empty);
    }

    let (number, unit) = split_number_unit(value).ok_or_else(|| DurationParseError::Invalid {
        value: value.to_owned(),
    })?;
    let amount: u64 = number.parse().map_err(|_| DurationParseError::Invalid {
        value: value.to_owned(),
    })?;

    let duration = match unit {
        "" | "s" | "sec" | "secs" | "second" | "seconds" => Duration::from_secs(amount),
        "ms" | "millis" | "millisecond" | "milliseconds" => Duration::from_millis(amount),
        "m" | "min" | "mins" | "minute" | "minutes" => {
            Duration::from_secs(amount.saturating_mul(60))
        }
        "h" | "hr" | "hrs" | "hour" | "hours" => Duration::from_secs(amount.saturating_mul(3600)),
        _ => {
            return Err(DurationParseError::Invalid {
                value: value.to_owned(),
            });
        }
    };
    Ok(duration)
}

fn split_number_unit(value: &str) -> Option<(&str, &str)> {
    let split = value
        .char_indices()
        .find(|(_, ch)| !ch.is_ascii_digit())
        .map_or(value.len(), |(index, _)| index);
    if split == 0 {
        return None;
    }
    Some((&value[..split], value[split..].trim()))
}

/// Format a duration for summary tables (`1.2s`, `19.4s`, `4m12s`).
#[must_use]
pub fn format_duration(duration: Duration) -> String {
    let millis = duration.as_millis();
    if millis < 10_000 {
        let secs = duration.as_secs_f64();
        return format!("{secs:.1}s");
    }
    let total_secs = duration.as_secs();
    if total_secs < 60 {
        return format!("{total_secs}s");
    }
    let minutes = total_secs / 60;
    let secs = total_secs % 60;
    if minutes < 60 {
        return format!("{minutes}m{secs:02}s");
    }
    let hours = minutes / 60;
    let minutes = minutes % 60;
    format!("{hours}h{minutes:02}m")
}

#[cfg(test)]
mod tests {
    use super::{format_duration, parse_duration};
    use std::time::Duration;

    #[test]
    fn parses_common_suffixes() {
        assert_eq!(parse_duration("5s").unwrap(), Duration::from_secs(5));
        assert_eq!(parse_duration("10m").unwrap(), Duration::from_secs(600));
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("30").unwrap(), Duration::from_secs(30));
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("abc").is_err());
        assert!(parse_duration("5x").is_err());
    }

    #[test]
    fn formats_readable() {
        assert_eq!(format_duration(Duration::from_millis(1200)), "1.2s");
        assert_eq!(format_duration(Duration::from_secs(72)), "1m12s");
    }
}
