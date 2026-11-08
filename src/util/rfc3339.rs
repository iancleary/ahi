//! RFC 3339 timestamps in UTC: the current time as `YYYY-MM-DDTHH:MM:SSZ`,
//! and parsing of any RFC 3339 stamp (fractional seconds, `Z` or a numeric
//! offset) to Unix seconds. The civil-date conversions are the proleptic
//! Gregorian algorithms from Howard Hinnant's "chrono-Compatible Low-Level
//! Date Algorithms", which are exact for every representable date.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch, now.
pub(crate) fn unix_now() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(elapsed) => elapsed.as_secs() as i64,
        Err(before) => -(before.duration().as_secs() as i64),
    }
}

/// Now, as `YYYY-MM-DDTHH:MM:SSZ`.
pub(crate) fn now() -> String {
    format_utc(unix_now())
}

/// `seconds` since the epoch as an RFC 3339 UTC stamp with second precision.
pub(crate) fn format_utc(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let secs = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// Parse an RFC 3339 date-time to Unix seconds. Fractional seconds are
/// truncated; a numeric offset is applied. `None` for anything malformed,
/// including out-of-range fields and a leap second.
pub(crate) fn parse(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 20 {
        return None;
    }
    let num = |range: std::ops::Range<usize>| -> Option<i64> {
        let slice = bytes.get(range)?;
        if slice.is_empty() || !slice.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(slice).ok()?.parse().ok()
    };
    let sep = |index: usize, expected: &[u8]| -> Option<()> {
        expected.contains(bytes.get(index)?).then_some(())
    };
    let year = num(0..4)?;
    sep(4, b"-")?;
    let month = num(5..7)?;
    sep(7, b"-")?;
    let day = num(8..10)?;
    sep(10, b"Tt ")?;
    let hour = num(11..13)?;
    sep(13, b":")?;
    let minute = num(14..16)?;
    sep(16, b":")?;
    let second = num(17..19)?;
    let mut index = 19;
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        let start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if index == start {
            return None;
        }
    }
    let offset = match bytes.get(index)? {
        b'Z' | b'z' => {
            index += 1;
            0
        }
        sign @ (b'+' | b'-') => {
            let hours = num(index + 1..index + 3)?;
            sep(index + 3, b":")?;
            let minutes = num(index + 4..index + 6)?;
            index += 6;
            if hours > 23 || minutes > 59 {
                return None;
            }
            let magnitude = hours * 3600 + minutes * 60;
            if *sign == b'+' {
                magnitude
            } else {
                -magnitude
            }
        }
        _ => return None,
    };
    if index != bytes.len() {
        return None;
    }
    if !(1..=12).contains(&month)
        || day < 1
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
                29
            } else {
                28
            }
        }
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
pub(crate) fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The inverse of `days_from_civil`.
pub(crate) fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (if month <= 2 { y + 1 } else { y }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_known_instants() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_utc(1_794_095_997), "2026-11-07T23:59:57Z");
        assert_eq!(format_utc(-1), "1969-12-31T23:59:59Z");
        assert_eq!(format_utc(253_402_300_799), "9999-12-31T23:59:59Z");
    }

    #[test]
    fn parses_offsets_fractions_and_rejects_malformed() {
        assert_eq!(parse("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse("2026-11-07T23:59:57Z"), Some(1_794_095_997));
        assert_eq!(parse("2026-11-07T23:59:57.123456789Z"), Some(1_794_095_997));
        assert_eq!(parse("2026-11-07t23:59:57z"), Some(1_794_095_997));
        assert_eq!(parse("2026-11-08T01:59:57+02:00"), Some(1_794_095_997));
        assert_eq!(parse("2026-11-07T18:29:57-05:30"), Some(1_794_095_997));
        assert_eq!(parse("2000-02-29T00:00:00Z"), Some(951_782_400));
        for bad in [
            "",
            "2026-11-07",
            "2026-11-07T23:59:57",
            "2026-11-07T23:59:57.Z",
            "2026-13-07T23:59:57Z",
            "2026-02-30T00:00:00Z",
            "1900-02-29T00:00:00Z",
            "2026-11-07T24:00:00Z",
            "2026-11-07T23:60:00Z",
            "2026-11-07T23:59:60Z",
            "2026-11-07T23:59:57+24:00",
            "2026-11-07T23:59:57Zjunk",
            "2026-11-07X23:59:57Z",
            "٢٠٢٦-11-07T23:59:57Z",
        ] {
            assert_eq!(parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn every_day_over_eight_centuries_round_trips() {
        // 1600-01-01 to 2400-01-01 covers two full 400-year Gregorian cycles.
        let start = days_from_civil(1600, 1, 1);
        let end = days_from_civil(2400, 1, 1);
        let mut previous = (1599, 12, 31);
        for day in start..end {
            let civil = civil_from_days(day);
            assert_eq!(days_from_civil(civil.0, civil.1, civil.2), day, "{civil:?}");
            assert!(civil > previous, "{civil:?} after {previous:?}");
            assert!((1..=12).contains(&civil.1) && civil.2 >= 1);
            assert!(civil.2 <= days_in_month(civil.0, civil.1));
            previous = civil;
        }
        let stamp = format_utc(1_000_000_000);
        assert_eq!(parse(&stamp), Some(1_000_000_000));
    }

    #[test]
    fn now_is_well_formed_and_recent() {
        let stamp = now();
        let parsed = parse(&stamp).unwrap();
        assert!((parsed - unix_now()).abs() <= 1, "{stamp}");
        assert_eq!(stamp.len(), 20);
        assert!(stamp.ends_with('Z'));
    }
}
