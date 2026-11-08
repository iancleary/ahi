//! The local UTC offset at an instant, read from TZif zone files (RFC 8536,
//! versions 1 to 3) without a time-zone crate. The zone comes from `$TZ`
//! when it names a zone or a file under a zoneinfo directory, else from
//! `/etc/localtime`. Instants past the last recorded transition follow the
//! POSIX TZ rule in the file's footer, so DST keeps working for years the
//! file does not spell out. Windows has no zone files and no FFI is used, so
//! the lookup returns `None` there and callers show UTC.

use crate::util::rfc3339::{civil_from_days, days_from_civil, format_utc};

/// Largest zone file read. Real files are a few KiB; the cap bounds a
/// hostile `$TZDIR`.
const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// Directories searched for a zone name, in order, before `$TZDIR`.
const ZONEINFO_DIRS: [&str; 4] = [
    "/usr/share/zoneinfo",
    "/usr/lib/zoneinfo",
    "/usr/share/lib/zoneinfo",
    "/etc/zoneinfo",
];

/// UTC offset in seconds at `unix`, from the local zone: `$TZ` when it is a
/// zone name or an absolute/`:`-prefixed path under the zoneinfo dirs, else
/// `/etc/localtime`. `None` when nothing readable, when the file is not
/// TZif v1-v3, or on Windows (no registry/FFI access by design).
pub(crate) fn utc_offset(unix: i64) -> Option<i32> {
    if cfg!(windows) {
        return None;
    }
    let bytes = read_zone_file(std::env::var_os("TZ"), std::env::var_os("TZDIR"))?;
    offset_from_tzif(&bytes, unix)
}

/// "HH:MM" of `unix + offset`, 24-hour, using crate::util::rfc3339's civil
/// conversion.
pub(crate) fn hhmm(unix: i64, offset_seconds: i32) -> String {
    let stamp = format_utc(unix.saturating_add(i64::from(offset_seconds)));
    stamp[11..16].to_string()
}

/// The zone file's bytes, or `None` when `$TZ` names nothing usable and
/// `/etc/localtime` is unreadable.
fn read_zone_file(
    tz: Option<std::ffi::OsString>,
    tzdir: Option<std::ffi::OsString>,
) -> Option<Vec<u8>> {
    let dirs = zoneinfo_dirs(tzdir.as_deref());
    let candidates: Vec<std::path::PathBuf> = match tz.as_ref().and_then(|value| value.to_str()) {
        Some(value) if !value.is_empty() => {
            let name = value.strip_prefix(':').unwrap_or(value);
            if name.starts_with('/') {
                let path = std::path::Path::new(name);
                let inside = dirs.iter().any(|dir| path.starts_with(dir));
                let clean = !path
                    .components()
                    .any(|component| matches!(component, std::path::Component::ParentDir));
                if inside && clean {
                    vec![path.to_path_buf()]
                } else {
                    vec![]
                }
            } else if valid_zone_name(name) {
                dirs.iter().map(|dir| dir.join(name)).collect()
            } else {
                vec![]
            }
        }
        _ => vec![],
    };
    for path in candidates {
        if let Some(bytes) = read_capped(&path) {
            return Some(bytes);
        }
    }
    read_capped(std::path::Path::new("/etc/localtime"))
}

fn zoneinfo_dirs(tzdir: Option<&std::ffi::OsStr>) -> Vec<std::path::PathBuf> {
    let mut dirs: Vec<std::path::PathBuf> =
        ZONEINFO_DIRS.iter().map(std::path::PathBuf::from).collect();
    if let Some(extra) = tzdir {
        let extra = std::path::Path::new(extra);
        if extra.is_absolute() {
            dirs.push(extra.to_path_buf());
        }
    }
    dirs
}

/// A zone name as found in `$TZ`: ASCII letters, digits, `/`, `_`, `-`, `+`,
/// with no empty or `..` components, so joining it to a directory stays
/// inside that directory.
fn valid_zone_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-' | b'+'))
        && name
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// The file's bytes when it is a regular file no larger than
/// `MAX_FILE_BYTES`; the size check happens before any read.
fn read_capped(path: &std::path::Path) -> Option<Vec<u8>> {
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
        return None;
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= MAX_FILE_BYTES).then_some(bytes)
}

/// Pure: offset from TZif bytes at `unix`. Uses the v2+ 64-bit body when
/// present. The footer POSIX TZ string is applied for instants past the last
/// transition (DST rules), supporting the `Mm.w.d/time` form and the
/// `Jn`/`n` forms; a malformed footer falls back to the last ttinfo.
pub(crate) fn offset_from_tzif(bytes: &[u8], unix: i64) -> Option<i32> {
    let zone = parse_tzif(bytes)?;
    let last = match zone.transitions.last() {
        None => {
            // No transitions: the footer rule, when present and well formed,
            // describes the whole timeline; else the single ttinfo applies.
            return match zone.footer.as_deref().and_then(parse_posix_tz) {
                Some(rule) => Some(rule.offset_at(unix)),
                None => zone.types.first().copied(),
            };
        }
        Some(last) => *last,
    };
    if unix < zone.transitions[0].0 {
        // RFC 8536 section 3.2: before the first transition, the first
        // ttinfo (standard time by convention) applies.
        return zone.types.first().copied();
    }
    if unix >= last.0 {
        if let Some(rule) = zone.footer.as_deref().and_then(parse_posix_tz) {
            return Some(rule.offset_at(unix));
        }
        return zone.types.get(usize::from(last.1)).copied();
    }
    let index = zone.transitions.partition_point(|(at, _)| *at <= unix) - 1;
    zone.types
        .get(usize::from(zone.transitions[index].1))
        .copied()
}

/// The parts of a TZif file this module uses: transitions as
/// `(instant, ttinfo index)`, ttinfo UTC offsets, and the footer TZ string.
struct Zone {
    transitions: Vec<(i64, u8)>,
    types: Vec<i32>,
    footer: Option<String>,
}

/// The six header counts, in file order.
struct Counts {
    isutcnt: usize,
    isstdcnt: usize,
    leapcnt: usize,
    timecnt: usize,
    typecnt: usize,
    charcnt: usize,
}

impl Counts {
    /// Bytes of the data block that follows a header with these counts.
    fn body_len(&self, time_size: usize) -> Option<usize> {
        let leap = time_size.checked_add(4)?;
        let mut total = self.timecnt.checked_mul(time_size)?;
        for part in [
            self.timecnt,
            self.typecnt.checked_mul(6)?,
            self.charcnt,
            self.leapcnt.checked_mul(leap)?,
            self.isstdcnt,
            self.isutcnt,
        ] {
            total = total.checked_add(part)?;
        }
        Some(total)
    }
}

fn parse_tzif(bytes: &[u8]) -> Option<Zone> {
    let (version, counts) = parse_header(bytes)?;
    let v1_len = counts.body_len(4)?;
    if version == 1 {
        let body = bytes.get(44..44 + v1_len)?;
        return parse_body(body, &counts, 4, None);
    }
    // Versions 2 and 3 repeat the header and body with 64-bit times after
    // the v1 block, then end with a newline-wrapped footer.
    let second = 44 + v1_len;
    let (_, counts) = parse_header(bytes.get(second..)?)?;
    let start = second + 44;
    let v2_len = counts.body_len(8)?;
    let body = bytes.get(start..start + v2_len)?;
    let tail = bytes.get(start + v2_len..)?;
    let footer = match tail {
        [b'\n', rest @ ..] => {
            let end = rest.iter().position(|b| *b == b'\n')?;
            let text = std::str::from_utf8(&rest[..end]).ok()?;
            (!text.is_empty()).then(|| text.to_string())
        }
        [] => None,
        _ => return None,
    };
    parse_body(body, &counts, 8, footer)
}

/// Header: magic, version, 15 reserved bytes, six big-endian u32 counts.
fn parse_header(bytes: &[u8]) -> Option<(u8, Counts)> {
    let header = bytes.get(..44)?;
    if &header[..4] != b"TZif" {
        return None;
    }
    let version = match header[4] {
        0 => 1,
        b'2' => 2,
        b'3' => 3,
        _ => return None,
    };
    let count = |index: usize| -> Option<usize> {
        let at = 20 + index * 4;
        let value = u32::from_be_bytes(header[at..at + 4].try_into().ok()?);
        usize::try_from(value).ok()
    };
    let counts = Counts {
        isutcnt: count(0)?,
        isstdcnt: count(1)?,
        leapcnt: count(2)?,
        timecnt: count(3)?,
        typecnt: count(4)?,
        charcnt: count(5)?,
    };
    if counts.typecnt == 0 || counts.typecnt > 256 {
        return None;
    }
    Some((version, counts))
}

fn parse_body(
    body: &[u8],
    counts: &Counts,
    time_size: usize,
    footer: Option<String>,
) -> Option<Zone> {
    let times_end = counts.timecnt * time_size;
    let indices_end = times_end + counts.timecnt;
    let mut transitions = Vec::with_capacity(counts.timecnt);
    let mut previous = i64::MIN;
    for i in 0..counts.timecnt {
        let slice = &body[i * time_size..(i + 1) * time_size];
        let at = if time_size == 8 {
            i64::from_be_bytes(slice.try_into().ok()?)
        } else {
            i64::from(i32::from_be_bytes(slice.try_into().ok()?))
        };
        // Transitions must strictly increase for the binary search below.
        if at <= previous {
            return None;
        }
        previous = at;
        let index = body[times_end + i];
        if usize::from(index) >= counts.typecnt {
            return None;
        }
        transitions.push((at, index));
    }
    let types = (0..counts.typecnt)
        .map(|i| {
            let info = &body[indices_end + i * 6..indices_end + i * 6 + 4];
            i32::from_be_bytes([info[0], info[1], info[2], info[3]])
        })
        .collect();
    Some(Zone {
        transitions,
        types,
        footer,
    })
}

/// A POSIX TZ rule: `std offset[dst[offset][,start[/time],end[/time]]]`.
/// Offsets are UTC offsets in seconds (east positive), already sign-flipped
/// from the POSIX west-positive convention.
#[derive(Debug, PartialEq, Eq)]
struct PosixTz {
    std_offset: i32,
    dst: Option<DstRule>,
}

#[derive(Debug, PartialEq, Eq)]
struct DstRule {
    offset: i32,
    start: Transition,
    end: Transition,
}

/// A yearly transition: a date rule plus the local wall-clock seconds at
/// which it happens (in the time in force before the transition).
#[derive(Debug, PartialEq, Eq)]
struct Transition {
    date: DateRule,
    seconds: i64,
}

#[derive(Debug, PartialEq, Eq)]
enum DateRule {
    /// `Jn`: day 1-365, February 29 never counted.
    Julian(i64),
    /// `n`: day 0-365, February 29 counted in leap years.
    ZeroBased(i64),
    /// `Mm.w.d`: month 1-12, week 1-5 (5 is the last), weekday 0-6 from Sunday.
    MonthWeekDay { month: i64, week: i64, weekday: i64 },
}

impl PosixTz {
    fn offset_at(&self, unix: i64) -> i32 {
        let Some(dst) = &self.dst else {
            return self.std_offset;
        };
        // The rule year is the year in local standard time; a transition
        // never sits close enough to New Year for this to pick wrong.
        let (year, _, _) = civil_from_days((unix + i64::from(self.std_offset)).div_euclid(86_400));
        let start = dst.start.instant(year) - i64::from(self.std_offset);
        let end = dst.end.instant(year) - i64::from(dst.offset);
        let in_dst = if start < end {
            start <= unix && unix < end
        } else {
            // Southern hemisphere: DST spans the year boundary.
            !(end <= unix && unix < start)
        };
        if in_dst {
            dst.offset
        } else {
            self.std_offset
        }
    }
}

impl Transition {
    /// Seconds since the epoch of this transition's local wall-clock time
    /// in `year`, before the UTC offset is removed.
    fn instant(&self, year: i64) -> i64 {
        let day = match self.date {
            DateRule::Julian(n) => {
                let leap = days_from_civil(year + 1, 1, 1) - days_from_civil(year, 1, 1) == 366;
                let skip = i64::from(leap && n >= 60);
                days_from_civil(year, 1, 1) + n - 1 + skip
            }
            DateRule::ZeroBased(n) => days_from_civil(year, 1, 1) + n,
            DateRule::MonthWeekDay {
                month,
                week,
                weekday,
            } => {
                let first = days_from_civil(year, month, 1);
                let next = if month == 12 {
                    days_from_civil(year + 1, 1, 1)
                } else {
                    days_from_civil(year, month + 1, 1)
                };
                // 1970-01-01 was a Thursday (weekday 4).
                let first_weekday = (first + 4).rem_euclid(7);
                let mut day = first + (weekday - first_weekday).rem_euclid(7) + (week - 1) * 7;
                while day >= next {
                    day -= 7;
                }
                day
            }
        };
        day * 86_400 + self.seconds
    }
}

/// Parse a POSIX TZ string. `None` for anything malformed, including an
/// unsupported rule form, so the caller can fall back to the last ttinfo.
fn parse_posix_tz(text: &str) -> Option<PosixTz> {
    let mut rest = text;
    skip_name(&mut rest)?;
    let std_offset = -parse_offset(&mut rest, 24)?;
    if rest.is_empty() {
        return Some(PosixTz {
            std_offset,
            dst: None,
        });
    }
    skip_name(&mut rest)?;
    let dst_offset = if rest.is_empty() || rest.starts_with(',') {
        std_offset.checked_add(3600)?
    } else {
        -parse_offset(&mut rest, 24)?
    };
    if rest.is_empty() {
        // A DST name with no rule: the C library applies US rules; here the
        // rule is required so the result is never a guess.
        return None;
    }
    rest = rest.strip_prefix(',')?;
    let start = parse_transition(&mut rest)?;
    rest = rest.strip_prefix(',')?;
    let end = parse_transition(&mut rest)?;
    if !rest.is_empty() {
        return None;
    }
    Some(PosixTz {
        std_offset,
        dst: Some(DstRule {
            offset: dst_offset,
            start,
            end,
        }),
    })
}

/// A zone designation: three or more letters, or anything but `>` inside
/// angle brackets.
fn skip_name(rest: &mut &str) -> Option<()> {
    if let Some(after) = rest.strip_prefix('<') {
        let end = after.find('>')?;
        if end == 0 {
            return None;
        }
        *rest = &after[end + 1..];
        return Some(());
    }
    let len = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
    if len < 3 {
        return None;
    }
    *rest = &rest[len..];
    Some(())
}

/// `[+-]hh[:mm[:ss]]` as signed seconds, with hours limited to `max_hours`.
fn parse_offset(rest: &mut &str, max_hours: i64) -> Option<i32> {
    let negative = match rest.as_bytes().first()? {
        b'-' => {
            *rest = &rest[1..];
            true
        }
        b'+' => {
            *rest = &rest[1..];
            false
        }
        _ => false,
    };
    let hours = parse_number(rest, 3)?;
    if hours > max_hours {
        return None;
    }
    let mut total = hours * 3600;
    for scale in [60, 1] {
        if let Some(after) = rest.strip_prefix(':') {
            *rest = after;
            let part = parse_number(rest, 2)?;
            if part > 59 {
                return None;
            }
            total += part * scale;
        } else {
            break;
        }
    }
    let total = if negative { -total } else { total };
    i32::try_from(total).ok()
}

/// Up to `max_digits` leading ASCII digits.
fn parse_number(rest: &mut &str, max_digits: usize) -> Option<i64> {
    let len = rest.bytes().take_while(u8::is_ascii_digit).count();
    if len == 0 || len > max_digits {
        return None;
    }
    let value = rest[..len].parse().ok()?;
    *rest = &rest[len..];
    Some(value)
}

/// `Jn`, `n`, or `Mm.w.d`, each with an optional `/time`. The time may be
/// negative or exceed 24 hours (TZif v3), within the -167..=167 hour range.
fn parse_transition(rest: &mut &str) -> Option<Transition> {
    let date = if let Some(after) = rest.strip_prefix('J') {
        *rest = after;
        let n = parse_number(rest, 3)?;
        (1..=365).contains(&n).then_some(DateRule::Julian(n))?
    } else if let Some(after) = rest.strip_prefix('M') {
        *rest = after;
        let month = parse_number(rest, 2)?;
        *rest = rest.strip_prefix('.')?;
        let week = parse_number(rest, 1)?;
        *rest = rest.strip_prefix('.')?;
        let weekday = parse_number(rest, 1)?;
        if !(1..=12).contains(&month) || !(1..=5).contains(&week) || weekday > 6 {
            return None;
        }
        DateRule::MonthWeekDay {
            month,
            week,
            weekday,
        }
    } else {
        let n = parse_number(rest, 3)?;
        (0..=365).contains(&n).then_some(DateRule::ZeroBased(n))?
    };
    let seconds = if let Some(after) = rest.strip_prefix('/') {
        *rest = after;
        i64::from(parse_offset(rest, 167)?)
    } else {
        2 * 3600
    };
    Some(Transition { date, seconds })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::rfc3339::parse as at;

    /// A TZif v2 file: an empty-transition v1 block (readers must skip it
    /// by its counts), then the 64-bit block with `transitions` as
    /// `(instant, type index)`, `types` as UTC offsets, and `footer`.
    fn tzif_v2(transitions: &[(i64, u8)], types: &[i32], footer: &str) -> Vec<u8> {
        let mut out = Vec::new();
        header(&mut out, b'2', 0, types.len(), types.len());
        ttinfos(&mut out, types);
        out.extend(std::iter::repeat_n(0u8, types.len()));
        header(&mut out, b'2', transitions.len(), types.len(), types.len());
        for (instant, _) in transitions {
            out.extend(instant.to_be_bytes());
        }
        for (_, index) in transitions {
            out.push(*index);
        }
        ttinfos(&mut out, types);
        out.extend(std::iter::repeat_n(0u8, types.len()));
        out.push(b'\n');
        out.extend(footer.as_bytes());
        out.push(b'\n');
        out
    }

    fn header(out: &mut Vec<u8>, version: u8, timecnt: usize, typecnt: usize, charcnt: usize) {
        out.extend(b"TZif");
        out.push(version);
        out.extend([0u8; 15]);
        for count in [0, 0, 0, timecnt, typecnt, charcnt] {
            out.extend((count as u32).to_be_bytes());
        }
    }

    fn ttinfos(out: &mut Vec<u8>, types: &[i32]) {
        for (index, offset) in types.iter().enumerate() {
            out.extend(offset.to_be_bytes());
            out.push(u8::from(index > 0));
            out.push(index as u8);
        }
    }

    fn unix(stamp: &str) -> i64 {
        at(stamp).unwrap()
    }

    #[test]
    fn fixed_offset_zone_without_transitions() {
        let bytes = tzif_v2(&[], &[19_800], "<+0530>-5:30");
        assert_eq!(offset_from_tzif(&bytes, 0), Some(19_800));
        assert_eq!(
            offset_from_tzif(&bytes, unix("2026-11-07T00:00:00Z")),
            Some(19_800)
        );
        // Without a footer the single ttinfo still applies.
        let bytes = tzif_v2(&[], &[-3600], "");
        assert_eq!(offset_from_tzif(&bytes, 1_000_000), Some(-3600));
    }

    #[test]
    fn one_transition_uses_first_type_before_and_last_type_after() {
        let switch = unix("2000-01-01T00:00:00Z");
        let bytes = tzif_v2(&[(switch, 1)], &[3600, 7200], "<+02>-2");
        assert_eq!(offset_from_tzif(&bytes, switch - 1), Some(3600));
        assert_eq!(offset_from_tzif(&bytes, switch), Some(7200));
        assert_eq!(offset_from_tzif(&bytes, switch + 86_400 * 400), Some(7200));
    }

    #[test]
    fn several_transitions_pick_the_latest_at_or_before() {
        let a = unix("1990-01-01T00:00:00Z");
        let b = unix("1995-01-01T00:00:00Z");
        let c = unix("2000-01-01T00:00:00Z");
        // A malformed footer falls back to the last ttinfo past `c`.
        let bytes = tzif_v2(&[(a, 1), (b, 2), (c, 0)], &[0, 3600, 7200], "garbage");
        assert_eq!(offset_from_tzif(&bytes, a - 1), Some(0));
        assert_eq!(offset_from_tzif(&bytes, a), Some(3600));
        assert_eq!(offset_from_tzif(&bytes, b - 1), Some(3600));
        assert_eq!(offset_from_tzif(&bytes, b), Some(7200));
        assert_eq!(offset_from_tzif(&bytes, c - 1), Some(7200));
        assert_eq!(offset_from_tzif(&bytes, c), Some(0));
        assert_eq!(offset_from_tzif(&bytes, c + 1_000_000_000), Some(0));
    }

    #[test]
    fn us_eastern_footer_around_2026_transitions() {
        let last = unix("2025-11-02T06:00:00Z");
        let bytes = tzif_v2(&[(last, 0)], &[-18_000, -14_400], "EST5EDT,M3.2.0,M11.1.0");
        // Spring forward: 2026-03-08 02:00 EST == 07:00 UTC.
        assert_eq!(
            offset_from_tzif(&bytes, unix("2026-03-08T06:59:59Z")),
            Some(-18_000)
        );
        assert_eq!(
            offset_from_tzif(&bytes, unix("2026-03-08T07:00:00Z")),
            Some(-14_400)
        );
        // Fall back: 2026-11-01 02:00 EDT == 06:00 UTC.
        assert_eq!(
            offset_from_tzif(&bytes, unix("2026-11-01T05:59:59Z")),
            Some(-14_400)
        );
        assert_eq!(
            offset_from_tzif(&bytes, unix("2026-11-01T06:00:00Z")),
            Some(-18_000)
        );
        // Mid-summer and mid-winter, and a later year the file never lists.
        assert_eq!(
            offset_from_tzif(&bytes, unix("2026-07-04T12:00:00Z")),
            Some(-14_400)
        );
        assert_eq!(
            offset_from_tzif(&bytes, unix("2026-01-15T12:00:00Z")),
            Some(-18_000)
        );
        assert_eq!(
            offset_from_tzif(&bytes, unix("2031-03-09T07:00:00Z")),
            Some(-14_400)
        );
        assert_eq!(
            offset_from_tzif(&bytes, unix("2031-03-09T06:59:59Z")),
            Some(-18_000)
        );
        // Before the only transition the first ttinfo applies, not the rule.
        assert_eq!(
            offset_from_tzif(&bytes, unix("2025-07-01T00:00:00Z")),
            Some(-18_000)
        );
    }

    #[test]
    fn southern_hemisphere_rule_spans_the_new_year() {
        // Australia/Sydney: AEST-10AEDT,M10.1.0,M4.1.0/3
        let rule = parse_posix_tz("AEST-10AEDT,M10.1.0,M4.1.0/3").unwrap();
        assert_eq!(rule.offset_at(unix("2026-01-15T00:00:00Z")), 39_600);
        assert_eq!(rule.offset_at(unix("2026-07-15T00:00:00Z")), 36_000);
        // 2026-04-05 03:00 AEDT == 2026-04-04 16:00 UTC.
        assert_eq!(rule.offset_at(unix("2026-04-04T15:59:59Z")), 39_600);
        assert_eq!(rule.offset_at(unix("2026-04-04T16:00:00Z")), 36_000);
        // 2026-10-04 02:00 AEST == 2026-10-03 16:00 UTC.
        assert_eq!(rule.offset_at(unix("2026-10-03T15:59:59Z")), 36_000);
        assert_eq!(rule.offset_at(unix("2026-10-03T16:00:00Z")), 39_600);
    }

    #[test]
    fn julian_and_zero_based_date_rules() {
        // J60 is always March 1; 59 (zero-based) is Feb 29 in a leap year,
        // March 1 otherwise.
        let julian = Transition {
            date: DateRule::Julian(60),
            seconds: 0,
        };
        assert_eq!(julian.instant(2024), unix("2024-03-01T00:00:00Z"));
        assert_eq!(julian.instant(2026), unix("2026-03-01T00:00:00Z"));
        let zero = Transition {
            date: DateRule::ZeroBased(59),
            seconds: 0,
        };
        assert_eq!(zero.instant(2024), unix("2024-02-29T00:00:00Z"));
        assert_eq!(zero.instant(2026), unix("2026-03-01T00:00:00Z"));
        // A full rule in each form, applied through a zone file.
        let bytes = tzif_v2(&[(0, 0)], &[0, 3600], "GMT0BST,J90/1,J300/2");
        assert_eq!(
            offset_from_tzif(&bytes, unix("2026-03-31T00:59:59Z")),
            Some(0)
        );
        assert_eq!(
            offset_from_tzif(&bytes, unix("2026-03-31T01:00:00Z")),
            Some(3600)
        );
        assert_eq!(
            offset_from_tzif(&bytes, unix("2026-10-27T00:59:59Z")),
            Some(3600)
        );
        assert_eq!(
            offset_from_tzif(&bytes, unix("2026-10-27T01:00:00Z")),
            Some(0)
        );
        let bytes = tzif_v2(&[(0, 0)], &[0, 3600], "GMT0BST,89,299");
        assert_eq!(
            offset_from_tzif(&bytes, unix("2026-03-31T01:59:59Z")),
            Some(0)
        );
        assert_eq!(
            offset_from_tzif(&bytes, unix("2026-03-31T02:00:00Z")),
            Some(3600)
        );
    }

    #[test]
    fn month_week_day_handles_fifth_week_and_negative_or_long_times() {
        // M3.5.0 is the last Sunday of March: 2026-03-29.
        let last_sunday = Transition {
            date: DateRule::MonthWeekDay {
                month: 3,
                week: 5,
                weekday: 0,
            },
            seconds: 3600,
        };
        assert_eq!(last_sunday.instant(2026), unix("2026-03-29T01:00:00Z"));
        // America/Godthab style: M3.5.0/-2 means 22:00 the previous day.
        let rule = parse_posix_tz("<-02>2<-01>,M3.5.0/-1,M10.5.0/0").unwrap();
        let dst = rule.dst.unwrap();
        assert_eq!(dst.start.seconds, -3600);
        assert_eq!(dst.end.seconds, 0);
        // TZif v3 allows times past 24 hours.
        let rule = parse_posix_tz("IST-2IDT,M3.4.4/26,M10.5.0").unwrap();
        assert_eq!(rule.dst.unwrap().start.seconds, 26 * 3600);
    }

    #[test]
    fn posix_tz_parsing_and_rejection() {
        assert_eq!(
            parse_posix_tz("EST5"),
            Some(PosixTz {
                std_offset: -18_000,
                dst: None,
            })
        );
        assert_eq!(parse_posix_tz("<+0545>-5:45").unwrap().std_offset, 20_700);
        assert_eq!(parse_posix_tz("UTC0").unwrap().std_offset, 0);
        let eastern = parse_posix_tz("EST5EDT,M3.2.0,M11.1.0").unwrap();
        assert_eq!(eastern.std_offset, -18_000);
        assert_eq!(eastern.dst.as_ref().unwrap().offset, -14_400);
        assert_eq!(eastern.dst.as_ref().unwrap().start.seconds, 7200);
        assert_eq!(
            parse_posix_tz("CET-1CEST-2,M3.5.0,M10.5.0/3")
                .unwrap()
                .dst
                .unwrap()
                .offset,
            7200
        );
        for bad in [
            "",
            "ES5",
            "EST",
            "EST5EDT",
            "EST25",
            "EST5EDT,M3.2.0",
            "EST5EDT,M13.2.0,M11.1.0",
            "EST5EDT,M3.6.0,M11.1.0",
            "EST5EDT,M3.2.7,M11.1.0",
            "EST5EDT,J0,M11.1.0",
            "EST5EDT,J366,M11.1.0",
            "EST5EDT,366,M11.1.0",
            "EST5EDT,M3.2.0,M11.1.0,extra",
            "EST5EDT,M3.2.0/168,M11.1.0",
            "<>5",
            "EST5:60",
        ] {
            assert_eq!(parse_posix_tz(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn rejects_malformed_tzif() {
        assert_eq!(offset_from_tzif(b"", 0), None);
        assert_eq!(offset_from_tzif(b"TZif", 0), None);
        let mut bad_magic = tzif_v2(&[], &[0], "UTC0");
        bad_magic[0] = b'X';
        assert_eq!(offset_from_tzif(&bad_magic, 0), None);
        let mut bad_version = tzif_v2(&[], &[0], "UTC0");
        bad_version[4] = b'9';
        assert_eq!(offset_from_tzif(&bad_version, 0), None);
        let truncated = tzif_v2(&[(0, 0)], &[0, 3600], "UTC0");
        assert_eq!(
            offset_from_tzif(&truncated[..truncated.len() - 20], 0),
            None
        );
        // A transition pointing past the ttinfo table.
        assert_eq!(offset_from_tzif(&tzif_v2(&[(0, 5)], &[0], "UTC0"), 0), None);
        // Transitions out of order.
        assert_eq!(
            offset_from_tzif(&tzif_v2(&[(10, 0), (5, 0)], &[0], "UTC0"), 0),
            None
        );
        // Counts that would overflow the body length.
        let mut huge = tzif_v2(&[], &[0], "UTC0");
        huge[32..36].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(offset_from_tzif(&huge, 0), None);
    }

    #[test]
    fn v1_only_file_is_read_with_32_bit_times() {
        let mut out = Vec::new();
        header(&mut out, 0, 1, 2, 2);
        out.extend(1_000i32.to_be_bytes());
        out.push(1);
        ttinfos(&mut out, &[0, 3600]);
        out.extend([0u8, 0]);
        assert_eq!(offset_from_tzif(&out, 999), Some(0));
        assert_eq!(offset_from_tzif(&out, 1_000), Some(3600));
        // Trailing bytes after a v1 body are tolerated; a v2 file with
        // junk instead of a footer is not.
        out.push(0xff);
        assert_eq!(offset_from_tzif(&out, 1_000), Some(3600));
        let mut v2 = tzif_v2(&[], &[0], "UTC0");
        v2.pop();
        v2.pop();
        v2.pop();
        v2.pop();
        v2.pop();
        v2.pop();
        v2.push(b'x');
        assert_eq!(offset_from_tzif(&v2, 0), None);
    }

    #[test]
    fn hhmm_vectors() {
        assert_eq!(hhmm(0, 0), "00:00");
        assert_eq!(hhmm(0, -18_000), "19:00");
        assert_eq!(hhmm(0, 19_800), "05:30");
        assert_eq!(hhmm(unix("2026-11-07T23:59:57Z"), 0), "23:59");
        assert_eq!(hhmm(unix("2026-11-07T23:59:57Z"), 3600), "00:59");
        assert_eq!(hhmm(unix("2026-03-08T07:00:00Z"), -14_400), "03:00");
        assert_eq!(hhmm(-1, 0), "23:59");
        assert_eq!(hhmm(i64::MAX, 3600), hhmm(i64::MAX, 0));
    }

    #[test]
    fn zone_names_are_validated() {
        for good in [
            "UTC",
            "America/New_York",
            "Etc/GMT+5",
            "EST5EDT",
            "US/Pacific-New",
        ] {
            assert!(valid_zone_name(good), "{good:?}");
        }
        for bad in [
            "",
            "/etc/passwd",
            "../etc/passwd",
            "America/../../etc/passwd",
            "America//New_York",
            "America/.",
            "Europe/Zürich",
            "a b",
            "a\0b",
            "Europe/",
        ] {
            assert!(!valid_zone_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn tz_paths_outside_zoneinfo_fall_back_to_etc_localtime() {
        // These never read a real zone file: the paths do not exist, and the
        // environment is passed in rather than taken from the process.
        let missing =
            |tz: &str| read_zone_file(Some(tz.into()), Some("/nonexistent/ahi-tzdir".into()));
        let fallback = read_zone_file(None, None);
        // Whatever /etc/localtime yields, an unusable $TZ yields the same.
        for tz in [
            "/nonexistent/ahi/zone",
            ":/nonexistent/ahi/zone",
            "/usr/share/zoneinfo/../../../nonexistent",
            "Nonexistent/Ahi_Zone",
            "../Nonexistent",
            "",
        ] {
            assert_eq!(missing(tz).is_some(), fallback.is_some(), "{tz:?}");
        }
        assert!(read_capped(std::path::Path::new("/nonexistent/ahi/zone")).is_none());
    }
}
