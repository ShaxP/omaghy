//! Per-kind TTLs — `spec/20-store.md` §4.
//!
//! A TTL decides one thing: whether a read is reported `stale`. It never
//! decides whether data is shown. Stale data renders normally with an
//! indicator; only *absent* data shows an empty state.

use time::Duration;

/// Dashboard sections.
pub const DASHBOARD: Duration = Duration::minutes(5);
/// PR and issue lists.
pub const LIST: Duration = Duration::minutes(5);
/// PR and issue detail.
pub const DETAIL: Duration = Duration::minutes(2);
/// Check runs — the one thing a user watches change.
pub const CHECKS: Duration = Duration::seconds(30);
/// Repository metadata.
pub const REPO: Duration = Duration::hours(24);

/// The floor under GitHub's `X-Poll-Interval`, per §4.
///
/// GitHub's header is authoritative upwards — ignoring it earns a secondary
/// rate limit — but it can also come back small, and polling an inbox more
/// than once a minute buys nothing a human notices.
pub const NOTIFICATIONS_FLOOR: Duration = Duration::seconds(60);

/// Notifications TTL: GitHub's advertised poll interval, floored.
///
/// `None` is the cold case — we have not yet seen an `X-Poll-Interval` header,
/// which is normal before the first fetch.
pub fn notifications(poll_interval: Option<Duration>) -> Duration {
    match poll_interval {
        Some(d) if d > NOTIFICATIONS_FLOOR => d,
        _ => NOTIFICATIONS_FLOOR,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_poll_interval_is_a_floor_not_an_override() {
        // GitHub says slow down: obey it.
        assert_eq!(
            notifications(Some(Duration::seconds(300))),
            Duration::seconds(300)
        );
        // GitHub says speed up: decline. A sub-minute inbox poll buys nothing
        // a human notices and costs a secondary rate limit if we are wrong.
        assert_eq!(
            notifications(Some(Duration::seconds(5))),
            NOTIFICATIONS_FLOOR
        );
        assert_eq!(notifications(None), NOTIFICATIONS_FLOOR);
    }

    #[test]
    fn checks_expire_faster_than_anything_else() {
        // CI is the one thing a user sits and watches change.
        assert!(CHECKS < DETAIL);
        assert!(DETAIL < LIST);
        assert!(LIST <= DASHBOARD);
        assert!(DASHBOARD < REPO);
    }
}
