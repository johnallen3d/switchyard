//! When a snooze typed into the tab picker's ctrl-s prompt ends.
//!
//! - A duration: `90m`, `2h`, `1h30m`, `3d`, `1w` (spaces allowed).
//! - A clock time, the next one to come: `9am`, `5:30pm`, `17:00`.
//! - A day, at 9am unless a time follows: `tomorrow`, `today 4pm`,
//!   `fri`, `monday 2pm` (the next one; today's only while the time is
//!   still ahead).
//!
//! Times are local, through libc's localtime_r and mktime.

/// The longest snooze.
const MAX_SECS: u64 = 365 * 24 * 3600;

/// The hour a day without a time means.
const MORNING: u32 = 9;

const DAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

pub const EXAMPLES: &str = "2h, 90m, 3d, 9am, tomorrow, fri 2pm";

/// The local fields of a time.
#[derive(Clone, Copy)]
struct Local {
    year: i32,
    month: i32,
    day: i32,
    hour: i32,
    min: i32,
    /// 0 is Sunday.
    weekday: i32,
}

fn local(t: u64) -> Local {
    let secs = t as libc::time_t;
    // SAFETY: localtime_r only writes the tm it is given.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&secs, &mut tm) };
    Local { year: tm.tm_year, month: tm.tm_mon, day: tm.tm_mday, hour: tm.tm_hour, min: tm.tm_min, weekday: tm.tm_wday }
}

/// The time at `hour:min` local, `days` after `base`'s day (mktime
/// carries a day past the month's end into the next).
fn at(base: Local, days: i32, hour: u32, min: u32) -> u64 {
    // SAFETY: mktime only reads and normalizes the tm it is given.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = base.year;
    tm.tm_mon = base.month;
    tm.tm_mday = base.day + days;
    tm.tm_hour = hour as i32;
    tm.tm_min = min as i32;
    tm.tm_isdst = -1;
    (unsafe { libc::mktime(&mut tm) }).max(0) as u64
}

/// `2h`, `1h30m`, `1h 30m`: seconds, or None if it isn't a duration.
fn duration(s: &str) -> Option<u64> {
    let mut total = 0u64;
    let mut rest = s.trim();
    if rest.is_empty() {
        return None;
    }
    while !rest.is_empty() {
        let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        if digits == 0 {
            return None;
        }
        let n: u64 = rest[..digits].parse().ok()?;
        rest = rest[digits..].trim_start();
        let unit = rest.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(rest.len());
        let secs = match &rest[..unit] {
            "m" | "min" | "mins" | "minute" | "minutes" => 60,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3600,
            "d" | "day" | "days" => 86400,
            "w" | "wk" | "wks" | "week" | "weeks" => 7 * 86400,
            _ => return None,
        };
        total = total.checked_add(n.checked_mul(secs)?)?;
        rest = rest[unit..].trim_start();
    }
    Some(total)
}

/// `9`, `9am`, `9:30pm`, `17:00`: (hour, minute), or None.
fn clock(s: &str) -> Option<(u32, u32)> {
    let s = s.trim();
    let (s, half) = match s.strip_suffix("am").or_else(|| s.strip_suffix('a')) {
        Some(rest) => (rest.trim_end(), Some(false)),
        None => match s.strip_suffix("pm").or_else(|| s.strip_suffix('p')) {
            Some(rest) => (rest.trim_end(), Some(true)),
            None => (s, None),
        },
    };
    let (h, m) = match s.split_once(':') {
        Some((h, m)) if m.len() == 2 => (h, m),
        Some(_) => return None,
        None => (s, "0"),
    };
    if h.is_empty() || h.len() > 2 || !h.bytes().all(|b| b.is_ascii_digit()) || !m.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (mut hour, min): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    if min > 59 {
        return None;
    }
    match half {
        Some(pm) => {
            if hour == 0 || hour > 12 {
                return None;
            }
            hour = hour % 12 + if pm { 12 } else { 0 };
        }
        None if hour > 23 => return None,
        None => {}
    }
    Some((hour, min))
}

/// `tue`, `tuesday`: 0 for Sunday to 6.
fn weekday(s: &str) -> Option<i32> {
    let full = ["sunday", "monday", "tuesday", "wednesday", "thursday", "friday", "saturday"];
    let alt = ["sun", "mon", "tues", "wed", "thur", "fri", "sat"];
    (0..7).find(|&i| s == DAYS[i] || s == full[i] || s == alt[i] || (i == 4 && s == "thurs")).map(|i| i as i32)
}

/// When a snooze typed at `now` ends, or why it can't be read.
pub fn parse(input: &str, now: u64) -> Result<u64, String> {
    let s = input.trim().to_lowercase();
    if s.is_empty() {
        return Err(format!("Try {EXAMPLES}"));
    }
    let until = if let Some(secs) = duration(&s) {
        if secs == 0 {
            return Err("Snooze for at least a minute".into());
        }
        now.saturating_add(secs)
    } else {
        let base = local(now);
        let (day, time) = match s.split_once(char::is_whitespace) {
            Some((day, time)) => (day, Some(time.trim())),
            None => (s.as_str(), None),
        };
        let time_of = |t: Option<&str>| match t {
            None => Ok((MORNING, 0)),
            Some(t) => clock(t).ok_or_else(|| format!("Can't read the time “{t}”")),
        };
        match day {
            "today" => {
                if time.is_none() {
                    return Err("Say when today, such as today 4pm".into());
                }
                let (h, m) = time_of(time)?;
                let t = at(base, 0, h, m);
                if t <= now {
                    return Err("That time has passed today".into());
                }
                t
            }
            "tomorrow" | "tmr" | "tmrw" => {
                let (h, m) = time_of(time)?;
                at(base, 1, h, m)
            }
            _ => match weekday(day) {
                Some(wd) => {
                    let (h, m) = time_of(time)?;
                    let mut ahead = (wd - base.weekday).rem_euclid(7);
                    if ahead == 0 && at(base, 0, h, m) <= now {
                        ahead = 7;
                    }
                    at(base, ahead, h, m)
                }
                None => match clock(&s) {
                    // The next time the clock shows it.
                    Some((h, m)) => {
                        let t = at(base, 0, h, m);
                        if t > now { t } else { at(base, 1, h, m) }
                    }
                    None => return Err(format!("Can't read “{}”: try {EXAMPLES}", input.trim())),
                },
            },
        }
    };
    if until <= now {
        return Err("That time has passed".into());
    }
    if until - now > MAX_SECS {
        return Err("Snooze for a year at most".into());
    }
    Ok(until)
}

/// `9:00am` local.
fn clock_text(l: Local) -> String {
    let (h, pm) = match l.hour {
        0 => (12, false),
        1..12 => (l.hour, false),
        12 => (12, true),
        h => (h - 12, true),
    };
    format!("{h}:{:02}{}", l.min, if pm { "pm" } else { "am" })
}

/// When `until` is, as seen from `now`: `4:30pm`, `tomorrow 9:00am`,
/// `Fri 9:00am`, or `Nov 21 9:00am`.
pub fn describe(until: u64, now: u64) -> String {
    let (u, n) = (local(until), local(now));
    let days = |l: Local| at(l, 0, 12, 0);
    // Days apart, by local noons (so a DST change can't shift it).
    let apart = (days(u) as i64 - days(n) as i64 + 43200).div_euclid(86400);
    let time = clock_text(u);
    match apart {
        0 => time,
        1 => format!("tomorrow {time}"),
        2..7 => {
            let day = DAYS[u.weekday as usize];
            format!("{}{} {time}", day[..1].to_uppercase(), &day[1..])
        }
        _ => {
            const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
            format!("{} {} {time}", MONTHS[u.month as usize], u.day)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    #[test]
    fn durations_add_to_now() {
        assert_eq!(parse("2h", NOW), Ok(NOW + 7200));
        assert_eq!(parse("90m", NOW), Ok(NOW + 5400));
        assert_eq!(parse("1h30m", NOW), Ok(NOW + 5400));
        assert_eq!(parse(" 1h 30m ", NOW), Ok(NOW + 5400));
        assert_eq!(parse("3d", NOW), Ok(NOW + 3 * 86400));
        assert_eq!(parse("1w", NOW), Ok(NOW + 7 * 86400));
        assert_eq!(parse("2 hours", NOW), Ok(NOW + 7200));
    }

    #[test]
    fn bad_input_says_why() {
        assert!(parse("", NOW).unwrap_err().contains("Try"));
        assert!(parse("soon", NOW).unwrap_err().contains("Can't read “soon”"));
        assert!(parse("0m", NOW).is_err());
        assert!(parse("2x", NOW).is_err());
        assert!(parse("400d", NOW).unwrap_err().contains("year"));
        assert!(parse("25:00", NOW).is_err());
        assert!(parse("13pm", NOW).is_err());
        assert!(parse("tomorrow noonish", NOW).unwrap_err().contains("noonish"));
        assert!(parse("today", NOW).is_err());
    }

    #[test]
    fn clocks_read_12_and_24_hour_times() {
        assert_eq!(clock("9am"), Some((9, 0)));
        assert_eq!(clock("9:30pm"), Some((21, 30)));
        assert_eq!(clock("12am"), Some((0, 0)));
        assert_eq!(clock("12pm"), Some((12, 0)));
        assert_eq!(clock("17:05"), Some((17, 5)));
        assert_eq!(clock("9:3"), None);
    }

    #[test]
    fn clock_times_and_days_land_ahead_at_the_time() {
        for input in ["9am", "17:00", "tomorrow", "tomorrow 2pm", "fri", "monday 4:30pm"] {
            let until = parse(input, NOW).unwrap_or_else(|e| panic!("{input}: {e}"));
            assert!(until > NOW && until - NOW <= 8 * 86400, "{input}");
        }
        assert_eq!(local(parse("tomorrow", NOW).unwrap()).hour, 9);
        let l = local(parse("monday 4:30pm", NOW).unwrap());
        assert_eq!((l.weekday, l.hour, l.min), (1, 16, 30));
        // The next 9am is within a day.
        assert!(parse("9am", NOW).unwrap() - NOW <= 86400);
    }

    #[test]
    fn describe_names_the_day_when_not_today() {
        let tomorrow = parse("tomorrow 9am", NOW).unwrap();
        assert_eq!(describe(tomorrow, NOW), "tomorrow 9:00am");
        assert!(describe(NOW + 60, NOW).ends_with("am") || describe(NOW + 60, NOW).ends_with("pm"));
        assert!(!describe(NOW + 3 * 86400, NOW).starts_with("tomorrow"));
        assert!(describe(NOW + 30 * 86400, NOW).split(' ').count() == 3);
    }
}
