//! When the next new photo is due: at fixed local times of day (so the date and forecast are current as the
//! day starts) and optionally every N minutes.

use chrono::{DateTime, Duration, Local, LocalResult, NaiveDate, NaiveDateTime, TimeZone};

/// A local date and time as a moment. One that does not exist (the clocks jumped over it) is moved an hour later,
/// to just after the jump (an hour later on the clock), so a midnight refresh is late on that one day, not lost.
fn resolve<Tz: TimeZone>(tz: &Tz, naive: NaiveDateTime) -> Option<DateTime<Tz>> {
    match tz.from_local_datetime(&naive) {
        LocalResult::Single(t) => Some(t),
        LocalResult::Ambiguous(first, _) => Some(first),
        LocalResult::None => tz.from_local_datetime(&(naive + Duration::hours(1))).earliest(),
    }
}

fn at(day: NaiveDate, minutes: u32) -> Option<DateTime<Local>> {
    resolve(&Local, day.and_hms_opt(minutes / 60, minutes % 60, 0)?)
}

/// The first moment after `now` at which a refresh is due.
pub fn next_due(now: DateTime<Local>, times: &[u32], interval_min: u32, last: Option<DateTime<Local>>) -> DateTime<Local> {
    let mut best: Option<DateTime<Local>> = None;
    let mut consider = |t: DateTime<Local>| {
        if best.is_none_or(|b| t < b) {
            best = Some(t);
        }
    };
    for add in 0..3 {
        let day = now.date_naive() + Duration::days(add);
        for &m in times {
            if let Some(t) = at(day, m).filter(|t| *t > now) {
                consider(t);
            }
        }
    }
    if interval_min > 0 {
        let base = last.unwrap_or(now) + Duration::minutes(interval_min as i64);
        consider(base.max(now + Duration::seconds(1)));
    }
    best.unwrap_or(now + Duration::hours(24))
}

/// Whether a refresh should happen right now because one was missed, for instance while the Pi was off at
/// midnight. With no history at all the answer is yes.
pub fn missed(now: DateTime<Local>, times: &[u32], interval_min: u32, last: Option<DateTime<Local>>) -> bool {
    let Some(last) = last else { return true };
    if interval_min > 0 && now - last >= Duration::minutes(interval_min as i64) {
        return true;
    }
    for add in 0..2 {
        let day = now.date_naive() - Duration::days(add);
        for &m in times {
            if let Some(t) = at(day, m) {
                if t <= now && t > last {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, mo, d, h, mi, 0).earliest().unwrap()
    }

    /// A zone in which 00:00 to 00:59 local time does not exist, like a spring-forward at midnight.
    #[derive(Clone, Copy, Debug)]
    struct GapZone;
    impl TimeZone for GapZone {
        type Offset = chrono::FixedOffset;
        fn from_offset(_: &chrono::FixedOffset) -> Self {
            GapZone
        }
        fn offset_from_local_date(&self, _: &NaiveDate) -> LocalResult<chrono::FixedOffset> {
            LocalResult::Single(chrono::FixedOffset::east_opt(0).unwrap())
        }
        fn offset_from_local_datetime(&self, l: &NaiveDateTime) -> LocalResult<chrono::FixedOffset> {
            if l.time() < chrono::NaiveTime::from_hms_opt(1, 0, 0).unwrap() {
                LocalResult::None
            } else {
                LocalResult::Single(chrono::FixedOffset::east_opt(0).unwrap())
            }
        }
        fn offset_from_utc_date(&self, _: &NaiveDate) -> chrono::FixedOffset {
            chrono::FixedOffset::east_opt(0).unwrap()
        }
        fn offset_from_utc_datetime(&self, _: &NaiveDateTime) -> chrono::FixedOffset {
            chrono::FixedOffset::east_opt(0).unwrap()
        }
    }

    #[test]
    fn a_time_that_does_not_exist_is_moved_to_after_the_jump() {
        let day = NaiveDate::from_ymd_opt(2026, 3, 29).unwrap();
        let midnight = resolve(&GapZone, day.and_hms_opt(0, 0, 0).unwrap()).unwrap();
        assert_eq!(midnight.naive_local(), day.and_hms_opt(1, 0, 0).unwrap());
        let half_past = resolve(&GapZone, day.and_hms_opt(0, 30, 0).unwrap()).unwrap();
        assert_eq!(half_past.naive_local(), day.and_hms_opt(1, 30, 0).unwrap());
        let noon = resolve(&GapZone, day.and_hms_opt(12, 0, 0).unwrap()).unwrap();
        assert_eq!(noon.naive_local(), day.and_hms_opt(12, 0, 0).unwrap());
    }

    #[test]
    fn midnight_schedule() {
        let times = [0];
        assert_eq!(next_due(t(2026, 10, 29, 21, 30), &times, 0, None), t(2026, 10, 30, 0, 0));
        assert_eq!(next_due(t(2026, 10, 30, 0, 0), &times, 0, None), t(2026, 10, 31, 0, 0));
    }

    #[test]
    fn several_times_and_interval() {
        let times = [0, 12 * 60];
        assert_eq!(next_due(t(2026, 10, 29, 9, 0), &times, 0, None), t(2026, 10, 29, 12, 0));
        let last = Some(t(2026, 10, 29, 9, 0));
        assert_eq!(next_due(t(2026, 10, 29, 9, 5), &[0], 60, last), t(2026, 10, 29, 10, 0));
        assert_eq!(next_due(t(2026, 10, 29, 23, 30), &[0], 60, Some(t(2026, 10, 29, 23, 0))), t(2026, 10, 30, 0, 0));
    }

    #[test]
    fn missed_refresh_after_power_loss() {
        let times = [0];
        let shown = Some(t(2026, 10, 28, 0, 0));
        assert!(missed(t(2026, 10, 29, 8, 0), &times, 0, shown), "midnight passed while off");
        assert!(!missed(t(2026, 10, 28, 20, 0), &times, 0, shown));
        assert!(missed(t(2026, 10, 28, 20, 0), &times, 0, None), "never shown");
        assert!(missed(t(2026, 10, 28, 3, 0), &[], 30, shown), "interval elapsed");
    }
}
