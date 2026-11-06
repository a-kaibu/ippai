//! Calendar time (C11 7.27): struct tm from seconds since 1970 and back (gmtime/localtime,
//! mktime's normalization) and strftime with the MS CRT's "C" locale.
//!
//! Ranges (MS docs): gmtime/localtime fail before 1970-01-01 00:00:00 UTC (localtime allows
//! the 12 hours west of it) and after 3000-12-31 23:59:59 (the 32-bit forms after 2038-01-19
//! 03:14:07); mktime returns -1 outside the same range.
//!
//! strftime: the C99 conversions plus MS's `#` flag (no leading zeros; %#c and %#x the long
//! date), the "C" locale's names and formats (%c = "%m/%d/%y %H:%M:%S", %x = "%m/%d/%y",
//! %X = "%H:%M:%S"); an invalid tm field or conversion makes it fail (the UCRT's EINVAL).

/// struct tm's fields in order: sec, min, hour, mday, mon, year (since 1900), wday, yday, isdst.
pub type Tm = [i32; 9];

/// The last second _time64 functions accept: 3000-12-31 23:59:59 UTC.
pub const MAX_TIME64: i64 = 32_535_215_999;
/// localtime's earliest: 12 hours before the epoch (a zone east of UTC still has a 1970 date).
pub const MIN_LOCAL: i64 = -43_200;

pub const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
pub const MONTHS: [&str; 12] =
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/// Days from 1970-01-01 to y-m-d (proleptic Gregorian; m 1..=12).
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// The date of day `days` since 1970-01-01: (year, month 1..=12, day 1..=31).
pub fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m, d)
}

/// The broken-down time of second `t` (already shifted to the wanted zone), isdst 0.
pub fn tm_of(t: i64) -> Tm {
    let days = t.div_euclid(86400);
    let rem = t.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    let yday = days - days_from_civil(y, 1, 1);
    [
        (rem % 60) as i32,
        (rem / 60 % 60) as i32,
        (rem / 3600) as i32,
        d as i32,
        (m - 1) as i32,
        (y - 1900) as i32,
        (days + 4).rem_euclid(7) as i32,
        yday as i32,
        0,
    ]
}

/// gmtime (`bits` 32 or 64): None outside the range.
pub fn gmtime(t: i64, bits: u32) -> Option<Tm> {
    let max = if bits == 32 { i32::MAX as i64 } else { MAX_TIME64 };
    (0..=max).contains(&t).then(|| tm_of(t))
}

/// localtime with the zone's offset east of UTC in seconds (`bias`).
pub fn localtime(t: i64, bits: u32, bias: i64) -> Option<Tm> {
    let max = if bits == 32 { i32::MAX as i64 } else { MAX_TIME64 };
    (MIN_LOCAL..=max).contains(&t).then(|| tm_of(t + bias))
}

/// mktime: the time of `tm` read as local time (`bias` east of UTC), every field normalized
/// (overflowing seconds into minutes and so on; tm_wday/tm_yday recomputed). None (-1) when the
/// result is outside the range of the `bits`-bit time_t; `tm` is then left alone.
pub fn mktime(tm: &mut Tm, bits: u32, bias: i64) -> Option<i64> {
    let (sec, min, hour, mday, mon, year) = (tm[0] as i64, tm[1] as i64, tm[2] as i64, tm[3] as i64, tm[4] as i64, tm[5] as i64);
    let y = 1900 + year + mon.div_euclid(12);
    let m = mon.rem_euclid(12);
    let days = days_from_civil(y, m + 1, 1) + mday - 1;
    let t = days * 86400 + hour * 3600 + min * 60 + sec - bias;
    let max = if bits == 32 { i32::MAX as i64 } else { MAX_TIME64 };
    if !(0..=max).contains(&t) {
        return None;
    }
    // (no daylight saving time: tm_isdst comes back 0)
    *tm = tm_of(t + bias);
    Some(t)
}

/// strftime's output for `fmt`; None when a field it needs is out of range or a conversion is
/// unknown (the UCRT returns 0 with EINVAL). `zone`: (offset east of UTC in seconds, name).
pub fn strftime(fmt: &[u16], tm: &Tm, zone: (i64, &str)) -> Option<Vec<u16>> {
    let mut o = String::new();
    let mut i = 0;
    let field = |k: usize, lo: i32, hi: i32| -> Option<i32> { (lo..=hi).contains(&tm[k]).then_some(tm[k]) };
    while i < fmt.len() {
        let ch = fmt[i];
        i += 1;
        if ch != b'%' as u16 {
            o.push(char::from_u32(ch as u32).unwrap_or('\u{fffd}'));
            continue;
        }
        let mut f = *fmt.get(i)?;
        let alt = f == b'#' as u16;
        if alt {
            i += 1;
            f = *fmt.get(i)?;
        }
        // (C99's E and O modifiers: the "C" locale has no alternative forms)
        if f == b'E' as u16 || f == b'O' as u16 {
            i += 1;
            f = *fmt.get(i)?;
        }
        i += 1;
        let n2 = |v: i32| if alt { format!("{v}") } else { format!("{v:02}") };
        let sec = || field(0, 0, 60);
        let min = || field(1, 0, 59);
        let hour = || field(2, 0, 23);
        let mday = || field(3, 1, 31);
        let mon = || field(4, 0, 11);
        let year = || field(5, -1900, 8099);
        let wday = || field(6, 0, 6);
        let yday = || field(7, 0, 365);
        if f >= 0x80 {
            return None;
        }
        match f as u8 {
            b'a' => o += &DAYS[wday()? as usize][..3],
            b'A' => o += DAYS[wday()? as usize],
            b'b' | b'h' => o += &MONTHS[mon()? as usize][..3],
            b'B' => o += MONTHS[mon()? as usize],
            b'c' if alt => {
                o += &format!(
                    "{}, {} {}, {}, {}:{}:{}",
                    DAYS[wday()? as usize],
                    MONTHS[mon()? as usize],
                    mday()?,
                    year()? + 1900,
                    hour()?,
                    format!("{:02}", min()?),
                    format!("{:02}", sec()?)
                )
            }
            b'c' => {
                o += &format!(
                    "{:02}/{:02}/{:02} {:02}:{:02}:{:02}",
                    mon()? + 1,
                    mday()?,
                    (year()? + 1900).rem_euclid(100),
                    hour()?,
                    min()?,
                    sec()?
                )
            }
            b'C' => o += &n2((year()? + 1900).div_euclid(100)),
            b'd' => o += &n2(mday()?),
            b'D' => o += &format!("{:02}/{:02}/{:02}", mon()? + 1, mday()?, (year()? + 1900).rem_euclid(100)),
            b'e' => o += &if alt { format!("{}", mday()?) } else { format!("{:2}", mday()?) },
            b'F' => o += &format!("{}-{:02}-{:02}", year()? + 1900, mon()? + 1, mday()?),
            b'g' => o += &n2(iso_week(year()?, yday()?, wday()?).0.rem_euclid(100) as i32),
            b'G' => o += &format!("{}", iso_week(year()?, yday()?, wday()?).0),
            b'H' => o += &n2(hour()?),
            b'I' => o += &n2(if hour()? % 12 == 0 { 12 } else { hour()? % 12 }),
            b'j' => o += &if alt { format!("{}", yday()? + 1) } else { format!("{:03}", yday()? + 1) },
            b'm' => o += &n2(mon()? + 1),
            b'M' => o += &n2(min()?),
            b'n' => o.push('\n'),
            b'p' => o += if hour()? < 12 { "AM" } else { "PM" },
            b'r' => {
                let h = hour()?;
                o += &format!("{:02}:{:02}:{:02} {}", if h % 12 == 0 { 12 } else { h % 12 }, min()?, sec()?, if h < 12 { "AM" } else { "PM" })
            }
            b'R' => o += &format!("{:02}:{:02}", hour()?, min()?),
            b'S' => o += &n2(sec()?),
            b't' => o.push('\t'),
            b'T' => o += &format!("{:02}:{:02}:{:02}", hour()?, min()?, sec()?),
            b'u' => o += &format!("{}", if wday()? == 0 { 7 } else { wday()? }),
            b'U' => o += &n2((yday()? + 7 - wday()?) / 7),
            b'V' => o += &n2(iso_week(year()?, yday()?, wday()?).1),
            b'w' => o += &format!("{}", wday()?),
            b'W' => o += &n2((yday()? + 7 - (wday()? + 6) % 7) / 7),
            b'x' if alt => o += &format!("{}, {} {}, {}", DAYS[wday()? as usize], MONTHS[mon()? as usize], mday()?, year()? + 1900),
            b'x' => o += &format!("{:02}/{:02}/{:02}", mon()? + 1, mday()?, (year()? + 1900).rem_euclid(100)),
            b'X' => o += &format!("{:02}:{:02}:{:02}", hour()?, min()?, sec()?),
            b'y' => o += &n2((year()? + 1900).rem_euclid(100)),
            b'Y' => o += &format!("{}", year()? + 1900),
            b'z' => {
                let off = zone.0 / 60;
                o += &format!("{}{:02}{:02}", if off < 0 { '-' } else { '+' }, off.abs() / 60, off.abs() % 60)
            }
            b'Z' => o += zone.1,
            b'%' => o.push('%'),
            _ => return None,
        }
    }
    Some(o.encode_utf16().collect())
}

/// ISO 8601 week-based (year, week) of a date (tm_year, tm_yday, tm_wday).
fn iso_week(year: i32, yday: i32, wday: i32) -> (i64, i32) {
    let y = year as i64 + 1900;
    let leap = |y: i64| (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    // the week's Thursday decides its year
    let wd = (wday + 6) % 7; // Monday 0
    let thursday = yday - wd + 3;
    if thursday < 0 {
        let prev_len = if leap(y - 1) { 366 } else { 365 };
        return (y - 1, (thursday + prev_len) / 7 + 1);
    }
    let len = if leap(y) { 366 } else { 365 };
    if thursday >= len {
        return (y + 1, 1);
    }
    (y, thursday / 7 + 1)
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// civil_from_days inverts days_from_civil over the whole range time_t covers (and past it).
    #[kani::proof]
    fn civil_roundtrip() {
        let d: i64 = kani::any();
        kani::assume((-1_000_000..=12_000_000).contains(&d));
        let (y, m, day) = civil_from_days(d);
        assert!((1..=12).contains(&m) && (1..=31).contains(&day));
        assert_eq!(days_from_civil(y, m, day), d);
    }

    /// tm_of gives fields in range, and its weekday steps with the day.
    #[kani::proof]
    fn tm_fields_in_range() {
        let t: i64 = kani::any();
        kani::assume((MIN_LOCAL..=MAX_TIME64 + 86_400).contains(&t));
        let tm = tm_of(t);
        assert!((0..60).contains(&tm[0]) && (0..60).contains(&tm[1]) && (0..24).contains(&tm[2]));
        assert!((1..=31).contains(&tm[3]) && (0..12).contains(&tm[4]) && (0..7).contains(&tm[6]) && (0..366).contains(&tm[7]));
        let next = tm_of(t + 86_400);
        assert_eq!(next[6], (tm[6] + 1) % 7);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(f: &str, tm: &Tm) -> String {
        let w: Vec<u16> = f.encode_utf16().collect();
        String::from_utf16(&strftime(&w, tm, (9 * 3600, "Tokyo Standard Time")).unwrap()).unwrap()
    }

    #[test]
    fn conversions() {
        // 2006-01-02 15:04:05 (a Monday)
        let tm = tm_of(1136214245);
        assert_eq!(tm, [5, 4, 15, 2, 0, 106, 1, 1, 0]);
        assert_eq!(s("%c|%#c|%x|%#x|%X", &tm), "01/02/06 15:04:05|Monday, January 2, 2006, 15:04:05|01/02/06|Monday, January 2, 2006|15:04:05");
        assert_eq!(s("%#d %#H %j %#j %I %p %z %Z %%", &tm), "2 15 002 2 03 PM +0900 Tokyo Standard Time %");
        assert_eq!(s("%G-W%V-%u %g", &tm), "2006-W01-1 06");
        assert!(strftime(&[b'%' as u16, b'Q' as u16], &tm, (0, "")).is_none());
        let mut bad = tm;
        bad[4] = 12;
        assert!(strftime(&[b'%' as u16, b'b' as u16], &bad, (0, "")).is_none());
    }

    #[test]
    fn mktime_normalizes() {
        let mut tm: Tm = [0, 0, 0, 32, 0, 70, 0, 0, 0]; // January 32nd 1970
        assert_eq!(mktime(&mut tm, 64, 0), Some(31 * 86400));
        assert_eq!(tm, [0, 0, 0, 1, 1, 70, 0, 31, 0]);
        let mut tm: Tm = [0, 0, 0, 1, 0, 69, 0, 0, 0];
        assert_eq!(mktime(&mut tm, 64, 0), None);
        let mut tm: Tm = [7, 14, 3, 19, 0, 138, 0, 0, 0];
        assert_eq!(mktime(&mut tm, 32, 0), Some(i32::MAX as i64));
        tm[0] += 1;
        assert_eq!(mktime(&mut tm, 32, 0), None);
        assert!(gmtime(-1, 64).is_none() && localtime(-1, 64, 9 * 3600).is_some());
    }
}
