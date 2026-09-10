//! The poll loop — what refreshes when nobody presses `r`.
//!
//! Until this existed omaghy fetched on entering a surface and on `r`, and
//! then never again: an inbox left open all afternoon showed the morning's
//! notifications with no indication that it had stopped looking.
//!
//! Three rules, all from `spec/20-store.md` §6:
//!
//! **GitHub's `X-Poll-Interval` wins upward.** It is an instruction, not a
//! suggestion, and ignoring it earns a secondary rate limit. Configuration can
//! ask to poll *less* often than GitHub allows; it cannot ask for more.
//!
//! **A tick schedules; it does not fetch.** Polling goes through
//! `Store::refresh` like a keypress, so a tick that lands while a refresh is
//! in flight coalesces into it instead of stacking a second request, and the
//! UI's "refreshing" indicator comes from the same place either way.
//!
//! **Failure backs off.** `omaghy-api` already refuses to send while rate
//! limited, so a tick during a limit costs no network — but it does emit a
//! `RefreshFailed` the UI will show. Doubling the wait after each consecutive
//! failure keeps an offline laptop from painting an error banner every minute
//! for an hour.

use crate::Syncer;
use omaghy_cache::SqliteStore;
use omaghy_store::{RefreshTarget, Store};
use std::sync::{Arc, Weak};
use time::Duration;
use tokio::task::JoinHandle;

/// How often each target is polled, before floors and back-off.
///
/// These are `40-config.md` §2's `[refresh]` defaults. Config cannot reach
/// them yet — there is no reader — so they arrive here as the same numbers
/// the file documents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PollConfig {
    pub notifications: Duration,
    pub dashboard: Duration,
}

impl Default for PollConfig {
    fn default() -> Self {
        Self {
            notifications: Duration::seconds(60),
            dashboard: Duration::seconds(300),
        }
    }
}

/// The longest we will wait after repeated failures.
///
/// Fifteen minutes is past every transient cause — a primary rate limit resets
/// hourly but `omaghy-api` reports its own resume time, and a closed laptop
/// does not care how long the wait was. Long enough to stop being noise,
/// short enough that reconnecting is noticed without a restart.
pub const MAX_BACKOFF: Duration = Duration::minutes(15);

/// Never faster than this, whatever anything says.
///
/// The same floor `omaghy_cache::ttl` puts under notifications, applied here
/// to every target: polling more than once a minute buys nothing a human
/// notices and spends a budget that is shared with everything the user does.
pub const FLOOR: Duration = Duration::seconds(60);

impl Syncer {
    /// Start polling. One task per target; both stop when the store drops.
    ///
    /// The handles are returned rather than detached so a caller — `omaghy
    /// watch`, a test — can stop them. Dropping them is fine: the tasks end on
    /// their own once nothing holds the store.
    pub fn start_polling(self: &Arc<Self>, cfg: PollConfig) -> Vec<JoinHandle<()>> {
        vec![
            self.poll(RefreshTarget::Notifications, cfg.notifications),
            self.poll(RefreshTarget::Dashboard, cfg.dashboard),
        ]
    }

    fn poll(self: &Arc<Self>, target: RefreshTarget, base: Duration) -> JoinHandle<()> {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move { poll_forever(weak, target, base).await })
    }
}

async fn poll_forever(syncer: Weak<Syncer>, target: RefreshTarget, base: Duration) {
    loop {
        let wait = {
            // Scoped so no `Arc` is held across the sleep: a poll task must
            // not be the reason the store outlives the TUI.
            let Some(s) = syncer.upgrade() else { return };
            let Some(store) = s.store() else { return };
            interval_for(&store, &target, base, s.failures(&target))
        };

        tokio::time::sleep(std::time::Duration::from_secs_f64(
            wait.as_seconds_f64().max(1.0),
        ))
        .await;

        let Some(s) = syncer.upgrade() else { return };
        let Some(store) = s.store() else { return };
        tracing::debug!(?target, seconds = wait.whole_seconds(), "poll tick");
        store.refresh(target.clone());
    }
}

/// How long to wait before the next tick.
///
/// `base` from configuration, raised to whatever GitHub asked for, floored,
/// then doubled once per consecutive failure up to [`MAX_BACKOFF`].
fn interval_for(
    store: &SqliteStore,
    target: &RefreshTarget,
    base: Duration,
    failures: u32,
) -> Duration {
    let mut wait = base.max(FLOOR);

    // GitHub's advertised interval applies to notifications, which is the
    // endpoint it sends the header for. Reading it every tick rather than once
    // is deliberate: it changes, and a client that cached the first value
    // would keep polling at a rate GitHub has since withdrawn.
    if *target == RefreshTarget::Notifications
        && let Ok(Some(advertised)) = store.with_cache(|c| c.poll_interval())
    {
        wait = wait.max(advertised);
    }

    for _ in 0..failures.min(8) {
        wait = (wait * 2i32).min(MAX_BACKOFF);
    }
    wait
}

#[cfg(test)]
mod tests {
    use super::*;
    use omaghy_store::Viewer;

    fn store_with_advertised(interval: Option<Duration>) -> SqliteStore {
        let s = SqliteStore::in_memory(Viewer::new("ShaxP")).unwrap();
        if let Some(d) = interval {
            s.with_cache(|c| c.set_poll_interval(d)).unwrap();
        }
        s
    }

    fn secs(store: &SqliteStore, target: RefreshTarget, base: Duration, failures: u32) -> i64 {
        interval_for(store, &target, base, failures).whole_seconds()
    }

    #[test]
    fn github_can_ask_us_to_slow_down_but_never_to_speed_up() {
        let slow = store_with_advertised(Some(Duration::seconds(120)));
        assert_eq!(
            secs(
                &slow,
                RefreshTarget::Notifications,
                Duration::seconds(60),
                0
            ),
            120,
            "X-Poll-Interval is an instruction, not a suggestion"
        );

        // GitHub asking for less than we intended does not speed us up.
        let fast = store_with_advertised(Some(Duration::seconds(5)));
        assert_eq!(
            secs(
                &fast,
                RefreshTarget::Notifications,
                Duration::seconds(300),
                0
            ),
            300
        );
    }

    #[test]
    fn nothing_polls_faster_than_the_floor() {
        let s = store_with_advertised(None);
        assert_eq!(
            secs(&s, RefreshTarget::Notifications, Duration::seconds(1), 0),
            60,
            "a config asking for one second gets the floor"
        );
    }

    #[test]
    fn a_cold_cache_has_no_advertised_interval_and_that_is_not_an_error() {
        let s = store_with_advertised(None);
        assert_eq!(
            secs(&s, RefreshTarget::Notifications, Duration::seconds(90), 0),
            90
        );
    }

    /// GitHub sends `X-Poll-Interval` for `/notifications`. Stretching the
    /// dashboard by it would apply one endpoint's instruction to another.
    #[test]
    fn the_dashboard_does_not_inherit_the_inbox_instruction() {
        let s = store_with_advertised(Some(Duration::seconds(3600)));
        assert_eq!(
            secs(&s, RefreshTarget::Dashboard, Duration::seconds(300), 0),
            300
        );
    }

    #[test]
    fn consecutive_failures_back_off_and_then_stop_growing() {
        let s = store_with_advertised(None);
        let at = |f| secs(&s, RefreshTarget::Notifications, Duration::seconds(60), f);

        assert_eq!(at(0), 60, "a success polls at the configured rate");
        assert_eq!(at(1), 120);
        assert_eq!(at(2), 240);
        assert_eq!(
            at(30),
            MAX_BACKOFF.whole_seconds(),
            "an offline laptop must not paint an error banner every minute"
        );
        assert!(
            at(u32::MAX) <= MAX_BACKOFF.whole_seconds(),
            "the counter is clamped before it is used as a shift"
        );
    }

    /// A poll task must not be the reason the store stays alive.
    #[tokio::test]
    async fn polling_stops_once_nothing_holds_the_syncer() {
        let client = Arc::new(omaghy_api::GitHubClient::new(
            omaghy_api::Token::new("gho_notarealtoken").unwrap(),
            Arc::new(omaghy_api::cassette::StubTransport::sequence([])),
        ));
        let syncer = Syncer::new(client);
        let handles = syncer.start_polling(PollConfig::default());
        drop(syncer);

        // Returns without waiting out an interval: the first thing the loop
        // does is upgrade the `Weak`, and there is nothing to upgrade to.
        for h in handles {
            tokio::time::timeout(std::time::Duration::from_secs(5), h)
                .await
                .expect("a poll task outlived its syncer")
                .expect("and it did not panic");
        }
    }
}
