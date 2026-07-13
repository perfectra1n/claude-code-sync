//! Pure value logic for the config commands.
//!
//! Nothing in here prompts, prints, or touches the filesystem — which is the
//! point. These conversions used to be inlined into the `inquire` call sites in
//! `interactive.rs` and `wizard.rs`, once per mode, which meant they could only
//! be exercised with a TTY attached and so were never tested at all.

use anyhow::{bail, Context, Result};

const BYTES_PER_MB: f64 = 1024.0 * 1024.0;

/// Split a comma-separated pattern list into its trimmed, non-empty parts.
///
/// This is deliberately only the split/trim/filter core. What an *empty* input
/// means is left to the caller, because the two modes report it differently:
/// interactive says "Cleared include patterns", the wizard says "Include
/// patterns set: []".
pub(super) fn parse_patterns(input: &str) -> Vec<String> {
    input
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Parse a file-size limit expressed in megabytes.
///
/// Rejects non-positive and non-finite values. Rust's float-to-int cast
/// saturates rather than wrapping or erroring, so without this check `-5` would
/// quietly become a 0-byte limit — which makes `FilterConfig::should_include`
/// reject every file — and `1e30` would become `u64::MAX`.
pub(super) fn parse_file_size_mb(input: &str) -> Result<u64> {
    let mb: f64 = input
        .trim()
        .parse()
        .context("Invalid number. Must be a positive number.")?;

    if !mb.is_finite() {
        bail!("Max file size must be a finite number of megabytes");
    }
    if mb <= 0.0 {
        bail!("Max file size must be greater than 0 MB (got {mb})");
    }

    Ok((mb * BYTES_PER_MB) as u64)
}

/// Render a byte count as megabytes with one decimal place.
pub(super) fn format_size_mb(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / BYTES_PER_MB)
}

/// The "current value" text for the optional age filter.
pub(super) fn format_age_days(days: Option<u32>) -> String {
    days.map_or_else(|| "Not set".to_string(), |d| d.to_string())
}

/// The "current value" text for a pattern list, with a caller-chosen label for
/// the empty case (the two modes word it differently).
pub(super) fn format_patterns(patterns: &[String], when_empty: &str) -> String {
    if patterns.is_empty() {
        when_empty.to_string()
    } else {
        patterns.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_patterns_splits_and_trims() {
        assert_eq!(
            parse_patterns("*work*, /home/user/x , *test*"),
            vec!["*work*", "/home/user/x", "*test*"]
        );
    }

    #[test]
    fn test_parse_patterns_drops_empty_segments() {
        // Trailing commas and stray whitespace are what people actually type.
        assert_eq!(parse_patterns("a,,b,"), vec!["a", "b"]);
        assert_eq!(parse_patterns("  ,  ,  "), Vec::<String>::new());
    }

    #[test]
    fn test_parse_patterns_empty_input_yields_empty_list() {
        assert_eq!(parse_patterns(""), Vec::<String>::new());
    }

    #[test]
    fn test_parse_file_size_accepts_positive_values() {
        assert_eq!(parse_file_size_mb("10").unwrap(), 10 * 1024 * 1024);
        assert_eq!(parse_file_size_mb("0.5").unwrap(), 512 * 1024);
        assert_eq!(parse_file_size_mb("  2  ").unwrap(), 2 * 1024 * 1024);
    }

    #[test]
    fn test_parse_file_size_rejects_negative() {
        // The bug this function exists to prevent: `as u64` saturates, so a
        // negative input used to land as a 0-byte limit, silently filtering out
        // every single file instead of erroring.
        let err = parse_file_size_mb("-5").unwrap_err().to_string();
        assert!(err.contains("greater than 0"), "unexpected error: {err}");
    }

    #[test]
    fn test_parse_file_size_rejects_zero() {
        assert!(parse_file_size_mb("0").is_err());
    }

    #[test]
    fn test_parse_file_size_rejects_non_finite() {
        // `"inf".parse::<f64>()` succeeds, and `inf as u64` saturates to u64::MAX.
        assert!(parse_file_size_mb("inf").is_err());
        assert!(parse_file_size_mb("NaN").is_err());
    }

    #[test]
    fn test_parse_file_size_rejects_garbage() {
        assert!(parse_file_size_mb("ten").is_err());
        assert!(parse_file_size_mb("").is_err());
    }

    #[test]
    fn test_format_size_mb_round_trips_with_parse() {
        let bytes = parse_file_size_mb("12.5").unwrap();
        assert_eq!(format_size_mb(bytes), "12.5");
    }

    #[test]
    fn test_format_age_days() {
        assert_eq!(format_age_days(Some(30)), "30");
        assert_eq!(format_age_days(None), "Not set");
    }

    #[test]
    fn test_format_patterns_uses_caller_label_when_empty() {
        assert_eq!(format_patterns(&[], "None"), "None");
        assert_eq!(
            format_patterns(&["a".to_string(), "b".to_string()], "None"),
            "a, b"
        );
    }
}
