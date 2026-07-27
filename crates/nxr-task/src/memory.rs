//! Memory quantity parsing for task resource metadata (schema v2).

use std::fmt;

/// Errors while parsing a memory quantity string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemoryParseError {
    pub message: String,
}

impl fmt::Display for MemoryParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for MemoryParseError {}

/// Parse a memory quantity such as `512MiB`, `4GiB`, or `1024`.
///
/// Bare integers are interpreted as bytes.
///
/// # Errors
///
/// Returns [`MemoryParseError`] when the string is empty or malformed.
pub fn parse_memory(raw: &str) -> Result<u64, MemoryParseError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(MemoryParseError {
            message: "memory must not be empty".to_owned(),
        });
    }

    let (digits, unit) = split_digits_unit(trimmed)?;
    let value: u64 = digits.parse().map_err(|_| MemoryParseError {
        message: format!("invalid memory quantity `{raw}`"),
    })?;
    if value == 0 {
        return Err(MemoryParseError {
            message: "memory must be greater than zero".to_owned(),
        });
    }

    let multiplier = match unit {
        "" | "B" => 1,
        "K" | "KB" | "KiB" => 1024,
        "M" | "MB" | "MiB" => 1024_u64.pow(2),
        "G" | "GB" | "GiB" => 1024_u64.pow(3),
        "T" | "TB" | "TiB" => 1024_u64.pow(4),
        other => {
            return Err(MemoryParseError {
                message: format!("unknown memory unit `{other}` in `{raw}`"),
            });
        }
    };

    value
        .checked_mul(multiplier)
        .ok_or_else(|| MemoryParseError {
            message: format!("memory quantity overflow for `{raw}`"),
        })
}

fn split_digits_unit(raw: &str) -> Result<(&str, &str), MemoryParseError> {
    let boundary = raw
        .char_indices()
        .find(|(_, ch)| !ch.is_ascii_digit())
        .map_or(raw.len(), |(index, _)| index);
    if boundary == 0 {
        return Err(MemoryParseError {
            message: format!("invalid memory quantity `{raw}`"),
        });
    }
    Ok((&raw[..boundary], &raw[boundary..]))
}

#[cfg(test)]
mod tests {
    use super::parse_memory;

    #[test]
    fn parses_common_units() {
        assert_eq!(parse_memory("512MiB").unwrap(), 512 * 1024 * 1024);
        assert_eq!(parse_memory("4GiB").unwrap(), 4 * 1024 * 1024 * 1024);
        assert_eq!(parse_memory("1024").unwrap(), 1024);
    }

    #[test]
    fn rejects_empty_and_zero() {
        assert!(parse_memory("").is_err());
        assert!(parse_memory("0").is_err());
        assert!(parse_memory("0MiB").is_err());
    }

    #[test]
    fn rejects_unknown_unit() {
        assert!(parse_memory("4XB").is_err());
    }
}
