//! Settling a file both machines changed only in its date-times.
//!
//! Claude Code rewrites timestamps such as `lastUpdated` in
//! `plugins/known_marketplaces.json` and skill manifests on every machine, so
//! those files differ between machines without anyone editing them. Such a
//! difference settles on the later date-time, with no prompt.
//!
//! The settle silently picks one side, so it must never fire on real content:
//! a value only counts as a timestamp when it is the whole value of a key
//! whose name says it is a time (`lastUpdated`, `modified`, `createdAt`,
//! `expires_at`, ...). An ISBN, an account number or a date written in prose
//! is content, and a difference in it is a real conflict.

use chrono::{DateTime, NaiveDateTime};

const ISO_DATE_TIME_SHAPE: &[u8] = b"dddd-dd-ddTdd:dd:dd";
const OFFSET_SHAPE: &[u8] = b"dd:dd";
const EPOCH_MILLISECONDS_DIGITS: usize = 13;

struct Timestamp<'a> {
    text: &'a str,
    milliseconds: i64,
}

/// The two versions with every date-time that differs set to the later one,
/// or `None` when they differ in anything else.
pub fn keep_later_timestamps(local: &[u8], remote: &[u8]) -> Option<Vec<u8>> {
    let local = std::str::from_utf8(local).ok()?;
    let remote = std::str::from_utf8(remote).ok()?;
    let mut merged = String::with_capacity(local.len());
    let mut local_position = 0;
    let mut remote_position = 0;

    while local_position < local.len() || remote_position < remote.len() {
        let local_timestamp = read_timestamp(local, local_position);
        let remote_timestamp = read_timestamp(remote, remote_position);
        if let (Some(local_timestamp), Some(remote_timestamp)) = (local_timestamp, remote_timestamp)
        {
            let remote_is_later = remote_timestamp.milliseconds > local_timestamp.milliseconds;
            let later = if remote_is_later {
                remote_timestamp.text
            } else {
                local_timestamp.text
            };
            merged.push_str(later);
            local_position += local_timestamp.text.len();
            remote_position += remote_timestamp.text.len();
            continue;
        }

        let local_character = local[local_position..].chars().next()?;
        let remote_character = remote[remote_position..].chars().next()?;
        if local_character != remote_character {
            return None;
        }
        merged.push(local_character);
        local_position += local_character.len_utf8();
        remote_position += remote_character.len_utf8();
    }

    Some(merged.into_bytes())
}

fn read_timestamp(text: &str, position: usize) -> Option<Timestamp<'_>> {
    if !is_the_value_of_a_time_key(&text[..position]) {
        return None;
    }
    let rest = &text[position..];
    let timestamp = read_iso_date_time(rest).or_else(|| read_epoch_milliseconds(rest))?;
    // The whole value, not the start of a longer word or number.
    let next_character = rest[timestamp.text.len()..].chars().next();
    if next_character.is_some_and(|character| character.is_alphanumeric()) {
        return None;
    }
    Some(timestamp)
}

/// Whether the text just before a value is `key: ` (YAML) or `"key": `
/// (JSON), optionally followed by the value's opening quote, with a key name
/// that says it holds a time.
fn is_the_value_of_a_time_key(before: &str) -> bool {
    let before = before
        .strip_suffix('"')
        .or_else(|| before.strip_suffix('\''))
        .unwrap_or(before);
    let Some(before) = before.trim_end_matches([' ', '\t']).strip_suffix(':') else {
        return false;
    };
    let before = before
        .strip_suffix('"')
        .or_else(|| before.strip_suffix('\''))
        .unwrap_or(before);
    let key_start = before
        .rfind(|character: char| {
            !(character.is_ascii_alphanumeric() || character == '_' || character == '-')
        })
        .map_or(0, |index| index + 1);
    names_a_time(&before[key_start..])
}

fn names_a_time(key: &str) -> bool {
    const TIME_WORDS: &[&str] = &[
        "time",
        "date",
        "updated",
        "modified",
        "created",
        "seen",
        "stamp",
        "epoch",
        "expires",
        "synced",
        "fetched",
        "installed",
    ];
    let lowercase = key.to_ascii_lowercase();
    !key.is_empty()
        && (TIME_WORDS.iter().any(|word| lowercase.contains(word))
            || key.ends_with("At")
            || lowercase.ends_with("_at")
            || lowercase.ends_with("-at")
            || lowercase == "at")
}

fn read_iso_date_time(text: &str) -> Option<Timestamp<'_>> {
    let bytes = text.as_bytes();
    if !matches_shape(bytes, ISO_DATE_TIME_SHAPE) {
        return None;
    }

    let mut end = ISO_DATE_TIME_SHAPE.len();
    let has_fraction = bytes.get(end) == Some(&b'.');
    if has_fraction {
        let fraction_digits = count_digits(&bytes[end + 1..]);
        if fraction_digits == 0 {
            return None;
        }
        end += 1 + fraction_digits;
    }

    let offset_sign = bytes.get(end).copied();
    let has_utc_designator = offset_sign == Some(b'Z');
    let has_numeric_offset = matches!(offset_sign, Some(b'+') | Some(b'-'))
        && matches_shape(&bytes[end + 1..], OFFSET_SHAPE);
    if has_utc_designator {
        end += 1;
    } else if has_numeric_offset {
        end += 1 + OFFSET_SHAPE.len();
    }

    let token = &text[..end];
    let milliseconds = if has_utc_designator || has_numeric_offset {
        DateTime::parse_from_rfc3339(token).ok()?.timestamp_millis()
    } else {
        NaiveDateTime::parse_from_str(token, "%Y-%m-%dT%H:%M:%S%.f")
            .ok()?
            .and_utc()
            .timestamp_millis()
    };

    Some(Timestamp {
        text: token,
        milliseconds,
    })
}

fn read_epoch_milliseconds(text: &str) -> Option<Timestamp<'_>> {
    let digits = count_digits(text.as_bytes());
    if digits != EPOCH_MILLISECONDS_DIGITS {
        return None;
    }

    let token = &text[..digits];
    Some(Timestamp {
        text: token,
        milliseconds: token.parse().ok()?,
    })
}

fn matches_shape(bytes: &[u8], shape: &[u8]) -> bool {
    if bytes.len() < shape.len() {
        return false;
    }
    shape.iter().zip(bytes).all(|(expected, actual)| {
        if *expected == b'd' {
            actual.is_ascii_digit()
        } else {
            expected == actual
        }
    })
}

fn count_digits(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_that_differ_only_in_dates_settle_on_the_later_ones() {
        let cases: &[(&str, &str, &str, Option<&str>)] = &[
            (
                "iso timestamp, remote later",
                "{\"lastUpdated\": \"2026-10-01T06:00:01.741Z\"}",
                "{\"lastUpdated\": \"2026-10-01T06:50:50.567Z\"}",
                Some("{\"lastUpdated\": \"2026-10-01T06:50:50.567Z\"}"),
            ),
            (
                "iso timestamp, local later",
                "  modified: 2026-10-01T12:24:26.923Z\n",
                "  modified: 2026-09-21T07:13:05.230Z\n",
                Some("  modified: 2026-10-01T12:24:26.923Z\n"),
            ),
            (
                "each date settles on its own",
                "{\"createdAt\": \"2026-01-02T00:00:00Z\", \"lastSeen\": 1790137212675}",
                "{\"createdAt\": \"2026-01-01T00:00:00Z\", \"lastSeen\": 1790837225831}",
                Some("{\"createdAt\": \"2026-01-02T00:00:00Z\", \"lastSeen\": 1790837225831}"),
            ),
            (
                "numeric offset compared as an instant",
                "updated_at: 2026-10-01T08:00:00+02:00",
                "updated_at: 2026-10-01T07:00:00Z",
                Some("updated_at: 2026-10-01T07:00:00Z"),
            ),
            (
                "a date-time in prose is content",
                "met at 2026-10-01T08:00:00Z",
                "met at 2026-10-02T08:00:00Z",
                None,
            ),
            (
                "an ISBN is content, not epoch milliseconds",
                "\"isbn\": \"9780134685991\"",
                "\"isbn\": \"9780201633610\"",
                None,
            ),
            (
                "an id that looks like epoch milliseconds is content",
                "{\"accountId\": 1790137212675}",
                "{\"accountId\": 1790837225831}",
                None,
            ),
            (
                "a 13-digit number after a letter is content",
                "token: a1790137212675",
                "token: a1790837225831",
                None,
            ),
            (
                "a time key followed by more digits is content",
                "\"lastUpdated\": 17901372126751",
                "\"lastUpdated\": 17908372258312",
                None,
            ),
            (
                "text differs besides the dates",
                "modified: 2026-10-01T12:24:26Z\nnew paragraph\n",
                "modified: 2026-09-21T07:13:05Z\n",
                None,
            ),
            (
                "a plain date in prose is content, not a timestamp",
                "since 2026-09-26",
                "since 2026-09-25",
                None,
            ),
            (
                "a number of another length is content",
                "count: 12345",
                "count: 12346",
                None,
            ),
            (
                "a date on one side only",
                "at 2026-10-01T07:00:00Z",
                "at never",
                None,
            ),
        ];

        for (name, local, remote, expected) in cases {
            let merged = keep_later_timestamps(local.as_bytes(), remote.as_bytes());
            let merged_text = merged.map(|bytes| String::from_utf8(bytes).unwrap());
            assert_eq!(
                merged_text.as_deref(),
                *expected,
                "{name}: {local:?} vs {remote:?}"
            );
        }
    }

    #[test]
    fn binary_content_is_never_merged() {
        assert_eq!(keep_later_timestamps(&[0xff, 0xfe], &[0xff, 0xfd]), None);
    }
}
