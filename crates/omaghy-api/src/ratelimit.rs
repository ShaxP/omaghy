//! The rate-limit governor.
//!
//! Two budgets, tracked independently (`spec/20-store.md` §6): **REST is
//! requests** (5000/hr, 304s free) and **GraphQL is points** (5000/hr, a
//! combined dashboard query measured at 1 point). GitHub reports both through
//! the same `X-RateLimit-*` headers and distinguishes them with
//! `X-RateLimit-Resource`, so one observer serves both — but they must not
//! share a counter, or a busy hour of REST polling would look like a GraphQL
//! outage.
//!
//! The governor also holds the two headers that are instructions rather than
//! information: `Retry-After` and `X-Poll-Interval`. GitHub's poll interval is
//! not a suggestion; ignoring it earns a secondary limit.
//!
//! It refuses rather than waits. A primary limit resets up to an hour out, and
//! blocking a refresh for an hour is indistinguishable from a hang — so an
//! exhausted budget is [`StoreError::RateLimited`], which the UI can render.

use crate::transport::Headers;
use omaghy_model::{LimitKind, StoreError};
use std::sync::Mutex;
use time::{Duration, OffsetDateTime};

/// The floor on how often notifications are polled, from `spec/20-store.md` §4.
/// GitHub currently advertises exactly this, but the floor holds regardless of
/// what it advertises.
pub const POLL_INTERVAL_FLOOR: Duration = Duration::seconds(60);

/// What a `Retry-After` we cannot parse is worth. GitHub sends integer seconds
/// today; the HTTP-date form is legal and this is the conservative reading of
/// one.
const RETRY_AFTER_FALLBACK: Duration = Duration::seconds(60);

/// The penalty for a first secondary limit. Doubles per consecutive strike —
/// see [`Governor::note_secondary_limit`].
const SECONDARY_BASE: Duration = Duration::seconds(30);

/// The ceiling on that doubling. Beyond a quarter of an hour the user should
/// be told rather than waited on.
const SECONDARY_CAP: Duration = Duration::seconds(900);

/// Which budget a request spends from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Resource {
    /// Requests. `core` in GitHub's vocabulary.
    Rest,
    /// Points.
    GraphQl,
}

impl Resource {
    fn from_header(value: &str) -> Option<Self> {
        match value.trim() {
            "core" => Some(Self::Rest),
            "graphql" => Some(Self::GraphQl),
            // `search`, `integration_manifest`, … have their own much smaller
            // budgets. Recording one of those against `core` would read as
            // near-exhaustion (search is 30/min), so they are ignored until a
            // surface actually spends from them.
            _ => None,
        }
    }
}

/// Wall clock, injectable.
///
/// Not a convenience: every assertion about "is this budget still exhausted"
/// is a comparison against now, and a test that depends on the real clock is
/// a test that fails on a Tuesday.
#[derive(Debug, Clone, Copy, Default)]
pub enum Clock {
    #[default]
    System,
    Fixed(OffsetDateTime),
}

impl Clock {
    pub fn now(&self) -> OffsetDateTime {
        match self {
            Self::System => OffsetDateTime::now_utc(),
            Self::Fixed(t) => *t,
        }
    }
}

/// One budget, as GitHub last reported it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub limit: u32,
    pub remaining: u32,
    pub used: u32,
    pub reset: OffsetDateTime,
}

impl Budget {
    /// Whether the budget is spent *and* has not yet rolled over.
    ///
    /// A stale exhausted budget whose reset has passed is not exhausted: the
    /// next response will tell us the new numbers, and refusing to send that
    /// request would mean never learning them.
    pub fn is_exhausted(&self, now: OffsetDateTime) -> bool {
        self.remaining == 0 && self.reset > now
    }

    fn from_headers(headers: &Headers) -> Option<Self> {
        let reset: i64 = headers.get_num("x-ratelimit-reset")?;
        Some(Self {
            limit: headers.get_num("x-ratelimit-limit").unwrap_or(0),
            remaining: headers.get_num("x-ratelimit-remaining")?,
            used: headers.get_num("x-ratelimit-used").unwrap_or(0),
            reset: OffsetDateTime::from_unix_timestamp(reset).ok()?,
        })
    }
}

/// Everything the governor knows, as a value. What `omaghy doctor` prints and
/// what the footer reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RateLimits {
    pub rest: Option<Budget>,
    pub graphql: Option<Budget>,
    /// As advertised by `X-Poll-Interval`, unfloored.
    pub poll_interval: Option<Duration>,
    /// Set by `Retry-After` or a secondary limit. Nothing is sent before this.
    pub blocked_until: Option<OffsetDateTime>,
    pub blocked_kind: Option<LimitKind>,
    /// Consecutive secondary limits, cleared by any clean response. This is
    /// what makes the secondary backoff exponential.
    pub secondary_strikes: u32,
}

impl RateLimits {
    /// How often notifications may be polled, honouring both GitHub's
    /// advertised interval and our own floor.
    pub fn effective_poll_interval(&self) -> Duration {
        self.poll_interval
            .unwrap_or(POLL_INTERVAL_FLOOR)
            .max(POLL_INTERVAL_FLOOR)
    }
}

/// Tracks both budgets and gates outgoing requests.
#[derive(Debug)]
pub struct Governor {
    clock: Clock,
    state: Mutex<RateLimits>,
}

impl Governor {
    pub fn new(clock: Clock) -> Self {
        Self {
            clock,
            state: Mutex::new(RateLimits::default()),
        }
    }

    pub fn snapshot(&self) -> RateLimits {
        self.lock().clone()
    }

    pub fn now(&self) -> OffsetDateTime {
        self.clock.now()
    }

    /// May a request against `resource` be sent right now?
    pub fn check(&self, resource: Resource) -> Result<(), StoreError> {
        let now = self.clock.now();
        let s = self.lock();

        if let (Some(until), kind) = (s.blocked_until, s.blocked_kind)
            && until > now
        {
            return Err(StoreError::RateLimited {
                kind: kind.unwrap_or(LimitKind::Secondary),
                at: until,
            });
        }

        let budget = match resource {
            Resource::Rest => s.rest,
            Resource::GraphQl => s.graphql,
        };
        match budget {
            Some(b) if b.is_exhausted(now) => Err(StoreError::RateLimited {
                kind: LimitKind::Primary,
                at: b.reset,
            }),
            _ => Ok(()),
        }
    }

    /// Fold a response's headers into what we know.
    ///
    /// `hint` says which budget the request spent from; it is overridden by
    /// `X-RateLimit-Resource` when GitHub sends one, which it does on every
    /// authenticated response.
    pub fn observe(&self, hint: Resource, headers: &Headers) {
        // A resource header we do not recognise means "these numbers are not
        // for either budget we track" — `search`, say. Falling back to the
        // caller's hint would file them under `core`, where a 30/min budget
        // reads as an outage. Absent means GitHub did not say, and the hint is
        // the best we have.
        let resource = match headers.get("x-ratelimit-resource") {
            Some(named) => Resource::from_header(named),
            None => Some(hint),
        };

        let mut s = self.lock();

        if let (Some(resource), Some(b)) = (resource, Budget::from_headers(headers)) {
            match resource {
                Resource::Rest => s.rest = Some(b),
                Resource::GraphQl => s.graphql = Some(b),
            }
        }

        if let Some(secs) = headers.get_num::<i64>("x-poll-interval") {
            s.poll_interval = Some(Duration::seconds(secs));
        }

        if let Some(after) = retry_after(headers) {
            let until = self.clock.now() + after;
            if s.blocked_until.is_none_or(|b| until > b) {
                s.blocked_until = Some(until);
                s.blocked_kind = Some(LimitKind::Secondary);
            }
        }
    }

    /// Record a block we inferred rather than read from `Retry-After` — an
    /// exhausted primary budget reported by a 403, say.
    pub fn block_until(&self, kind: LimitKind, until: OffsetDateTime) {
        let mut s = self.lock();
        if s.blocked_until.is_none_or(|b| until > b) {
            s.blocked_until = Some(until);
            s.blocked_kind = Some(kind);
        }
    }

    /// A secondary limit was hit. Returns when sending may resume.
    ///
    /// This is the exponential backoff `spec/20-store.md` §6 asks for, and it
    /// lives here rather than in the retry loop on purpose: a secondary limit
    /// is never waited out inline. Blocking a refresh for thirty seconds is
    /// indistinguishable from a hang, so the request fails, the UI says why,
    /// and the governor refuses everything until the penalty elapses.
    ///
    /// `Retry-After` is honoured whenever it asks for *longer* than our own
    /// penalty. It is an instruction, and undercutting it is what earns the
    /// next strike.
    pub fn note_secondary_limit(&self, retry_after: Option<Duration>) -> OffsetDateTime {
        let mut s = self.lock();
        s.secondary_strikes = s.secondary_strikes.saturating_add(1);
        let doublings = (s.secondary_strikes - 1).min(16);
        let penalty = SECONDARY_BASE
            .checked_mul(1i32 << doublings)
            .unwrap_or(SECONDARY_CAP)
            .min(SECONDARY_CAP);

        let until = self.clock.now() + penalty.max(retry_after.unwrap_or(Duration::ZERO));
        if s.blocked_until.is_none_or(|b| until > b) {
            s.blocked_until = Some(until);
            s.blocked_kind = Some(LimitKind::Secondary);
        }
        until
    }

    /// A response arrived that was not a limit. Clears the strike count, so a
    /// secondary limit an hour from now starts from the short penalty again.
    pub fn note_success(&self) {
        self.lock().secondary_strikes = 0;
    }

    /// How long until sending is allowed again, if it is not allowed now.
    pub fn blocked_for(&self) -> Option<Duration> {
        let now = self.clock.now();
        self.lock()
            .blocked_until
            .filter(|u| *u > now)
            .map(|u| u - now)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RateLimits> {
        // A poisoned governor is still a usable governor: the numbers are
        // advisory, and refusing to send because another thread panicked would
        // turn one bug into an outage.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Default for Governor {
    fn default() -> Self {
        Self::new(Clock::System)
    }
}

/// `Retry-After`, in seconds. The HTTP-date form falls back to a fixed minute
/// rather than being parsed — GitHub sends integers, and a date parser here
/// would be a dependency on a format we have never seen it use.
pub fn retry_after(headers: &Headers) -> Option<Duration> {
    let raw = headers.get("retry-after")?;
    Some(match raw.trim().parse::<i64>() {
        Ok(secs) if secs >= 0 => Duration::seconds(secs),
        _ => RETRY_AFTER_FALLBACK,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    const NOW: OffsetDateTime = datetime!(2026-09-12 08:00 UTC);

    fn governor() -> Governor {
        Governor::new(Clock::Fixed(NOW))
    }

    fn limit_headers(resource: &str, remaining: u32, reset_in: i64) -> Headers {
        [
            ("x-ratelimit-limit", "5000".to_owned()),
            ("x-ratelimit-remaining", remaining.to_string()),
            (
                "x-ratelimit-reset",
                ((NOW + Duration::seconds(reset_in)).unix_timestamp()).to_string(),
            ),
            ("x-ratelimit-used", (5000 - remaining).to_string()),
            ("x-ratelimit-resource", resource.to_owned()),
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn the_two_budgets_do_not_share_a_counter() {
        let g = governor();
        g.observe(Resource::Rest, &limit_headers("core", 12, 600));
        g.observe(Resource::GraphQl, &limit_headers("graphql", 4986, 600));

        let s = g.snapshot();
        assert_eq!(s.rest.unwrap().remaining, 12);
        assert_eq!(s.graphql.unwrap().remaining, 4986);
    }

    #[test]
    fn the_resource_header_beats_the_callers_guess() {
        // A REST call that GitHub bills to graphql (or vice versa) must land
        // in the budget GitHub names, not the one we assumed.
        let g = governor();
        g.observe(Resource::Rest, &limit_headers("graphql", 4000, 600));
        assert!(g.snapshot().rest.is_none());
        assert_eq!(g.snapshot().graphql.unwrap().remaining, 4000);
    }

    #[test]
    fn a_foreign_budget_is_ignored_rather_than_miscounted() {
        // `search` is 30/min. Filing it under `core` would read as an outage.
        let g = governor();
        g.observe(Resource::Rest, &limit_headers("search", 2, 60));
        assert!(g.snapshot().rest.is_none());
        assert!(g.check(Resource::Rest).is_ok());
    }

    #[test]
    fn an_exhausted_budget_refuses_rather_than_waits() {
        let g = governor();
        g.observe(Resource::Rest, &limit_headers("core", 0, 1800));
        let e = g.check(Resource::Rest).unwrap_err();
        assert!(matches!(
            e,
            StoreError::RateLimited {
                kind: LimitKind::Primary,
                ..
            }
        ));
        // The other budget is untouched.
        assert!(g.check(Resource::GraphQl).is_ok());
    }

    #[test]
    fn an_exhausted_budget_whose_window_has_passed_is_not_exhausted() {
        let g = governor();
        g.observe(Resource::Rest, &limit_headers("core", 0, -10));
        assert!(
            g.check(Resource::Rest).is_ok(),
            "refusing here would mean never learning the new numbers"
        );
    }

    #[test]
    fn retry_after_blocks_both_budgets() {
        let g = governor();
        let h: Headers = [("retry-after", "30")].into_iter().collect();
        g.observe(Resource::Rest, &h);

        for r in [Resource::Rest, Resource::GraphQl] {
            let e = g.check(r).unwrap_err();
            assert!(matches!(
                e,
                StoreError::RateLimited {
                    kind: LimitKind::Secondary,
                    ..
                }
            ));
        }
        assert_eq!(g.blocked_for(), Some(Duration::seconds(30)));
    }

    #[test]
    fn consecutive_secondary_limits_back_off_exponentially() {
        let g = governor();
        assert_eq!(g.note_secondary_limit(None), NOW + Duration::seconds(30));
        assert_eq!(g.note_secondary_limit(None), NOW + Duration::seconds(60));
        assert_eq!(g.note_secondary_limit(None), NOW + Duration::seconds(120));

        // A clean response means the next one starts over.
        g.note_success();
        assert_eq!(g.note_secondary_limit(None), NOW + Duration::seconds(30));
    }

    #[test]
    fn retry_after_wins_when_it_asks_for_longer() {
        let g = governor();
        // Longer than the first penalty: GitHub's number is an instruction.
        assert_eq!(
            g.note_secondary_limit(Some(Duration::seconds(45))),
            NOW + Duration::seconds(45)
        );
        g.note_success();
        // Shorter than the penalty: ours wins, because undercutting is what
        // earns the next strike.
        assert_eq!(
            g.note_secondary_limit(Some(Duration::seconds(5))),
            NOW + Duration::seconds(30)
        );
    }

    #[test]
    fn the_secondary_penalty_is_capped() {
        let g = governor();
        for _ in 0..20 {
            g.note_secondary_limit(None);
        }
        assert_eq!(g.blocked_for(), Some(Duration::seconds(900)));
    }

    #[test]
    fn a_longer_block_wins_over_a_shorter_one() {
        let g = governor();
        g.block_until(LimitKind::Secondary, NOW + Duration::seconds(60));
        g.block_until(LimitKind::Secondary, NOW + Duration::seconds(10));
        assert_eq!(g.blocked_for(), Some(Duration::seconds(60)));
    }

    #[test]
    fn an_unparseable_retry_after_is_read_conservatively() {
        let h: Headers = [("retry-after", "Sat, 12 Sep 2026 08:01:00 GMT")]
            .into_iter()
            .collect();
        assert_eq!(retry_after(&h), Some(Duration::seconds(60)));
        assert_eq!(retry_after(&Headers::new()), None);
    }

    #[test]
    fn the_poll_interval_is_honoured_but_floored() {
        let g = governor();
        let h: Headers = [("x-poll-interval", "60")].into_iter().collect();
        g.observe(Resource::Rest, &h);
        assert_eq!(g.snapshot().poll_interval, Some(Duration::seconds(60)));

        // GitHub asking for *more* than the floor is an instruction, not a
        // suggestion: the floor must never shorten it.
        let h: Headers = [("x-poll-interval", "300")].into_iter().collect();
        g.observe(Resource::Rest, &h);
        assert_eq!(
            g.snapshot().effective_poll_interval(),
            Duration::seconds(300)
        );

        // And an absent interval still yields the floor, never zero.
        assert_eq!(
            RateLimits::default().effective_poll_interval(),
            POLL_INTERVAL_FLOOR
        );
    }
}
