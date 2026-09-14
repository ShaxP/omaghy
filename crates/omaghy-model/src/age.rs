//! Relative age. A model concern, not a view one — `spec/30-ui.md` §1.

use time::{Duration, OffsetDateTime};

/// Coarse buckets for grouping and for deciding emphasis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AgeBucket {
    Now,
    Hours,
    Today,
    Week,
    Month,
    Older,
}

/// Compact relative age: `now` `4m` `2h` `3d` `2w` `14mo` `2y`.
///
/// Right-aligned in lists, and the only always-present sort cue — so it is
/// never the column that drops (`spec/30-ui.md` §9).
pub fn relative(then: OffsetDateTime, now: OffsetDateTime) -> String {
    let d = now - then;
    if d < Duration::ZERO {
        // Clock skew, or a server timestamp slightly ahead of us. "now" is
        // less alarming than a negative age.
        return "now".into();
    }
    let secs = d.whole_seconds();
    match secs {
        0..60 => "now".into(),
        60..3600 => format!("{}m", secs / 60),
        3600..86_400 => format!("{}h", secs / 3600),
        86_400..604_800 => format!("{}d", secs / 86_400),
        604_800..2_592_000 => format!("{}w", secs / 604_800),
        2_592_000..31_536_000 => format!("{}mo", secs / 2_592_000),
        _ => format!("{}y", secs / 31_536_000),
    }
}

pub fn bucket(then: OffsetDateTime, now: OffsetDateTime) -> AgeBucket {
    let secs = (now - then).whole_seconds().max(0);
    match secs {
        0..3600 => AgeBucket::Now,
        3600..86_400 => AgeBucket::Hours,
        86_400..172_800 => AgeBucket::Today,
        172_800..604_800 => AgeBucket::Week,
        604_800..2_592_000 => AgeBucket::Month,
        _ => AgeBucket::Older,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    const NOW: OffsetDateTime = datetime!(2026-09-10 11:00 UTC);

    fn ago(d: Duration) -> String {
        relative(NOW - d, NOW)
    }

    #[test]
    fn formats_every_scale_compactly() {
        assert_eq!(ago(Duration::seconds(5)), "now");
        assert_eq!(ago(Duration::minutes(4)), "4m");
        assert_eq!(ago(Duration::hours(2)), "2h");
        assert_eq!(ago(Duration::days(3)), "3d");
        assert_eq!(ago(Duration::days(14)), "2w");
        assert_eq!(ago(Duration::days(400)), "1y");
        // Widest realistic output stays within the age column.
        assert!(ago(Duration::days(300)).len() <= 4);
    }

    #[test]
    fn boundaries_do_not_round_up_into_the_next_unit() {
        assert_eq!(ago(Duration::seconds(59)), "now");
        assert_eq!(ago(Duration::seconds(60)), "1m");
        assert_eq!(ago(Duration::minutes(59)), "59m");
        assert_eq!(ago(Duration::minutes(60)), "1h");
        assert_eq!(ago(Duration::hours(23)), "23h");
        assert_eq!(ago(Duration::hours(24)), "1d");
    }

    #[test]
    fn future_timestamps_read_as_now_not_as_negative() {
        // Server clocks run slightly ahead of ours more often than one expects.
        assert_eq!(relative(NOW + Duration::minutes(5), NOW), "now");
    }

    #[test]
    fn buckets_are_ordered_oldest_last() {
        assert!(bucket(NOW - Duration::minutes(5), NOW) < bucket(NOW - Duration::days(400), NOW));
        assert_eq!(bucket(NOW - Duration::days(400), NOW), AgeBucket::Older);
    }
}

#[cfg(test)]
mod deliberate_failure {
    /// Fails on purpose, to make CI red and produce a `ci_activity`
    /// notification — omaghy has never rendered one from live data.
    ///
    /// Delete this test and the branch once the notification arrives.
    #[test]
    fn this_test_is_meant_to_fail() {
        assert_eq!(2 + 2, 5, "deliberate: second run, with web notifications now on");
    }
}
