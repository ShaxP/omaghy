//! Turning HTTP into [`StoreError`].
//!
//! The distinctions in `spec/20-store.md` §7 earn their place by producing
//! *different UI*, so this module is where most of the care goes:
//!
//! - **`Offline` is not `Upstream`.** DNS, TLS and timeouts mean we never
//!   reached GitHub; with a warm cache that is a banner, while `Upstream` is a
//!   server that answered and said no.
//! - **`Forbidden` is not `NotFound`.** 403 means access was lost, so the
//!   subject is recorded as permanently unenriched rather than retried in a
//!   loop; 404 means it is gone or was never ours to see.
//! - **401 is `Auth`**, and an actionable one — it names `gh auth login`.
//!
//! The hard case is 403, which GitHub overloads for three unrelated things:
//! a spent primary budget, a secondary (abuse) limit, and genuine denial. They
//! are told apart by `X-RateLimit-Remaining`, `Retry-After`, and the body.

use crate::ratelimit::{Governor, retry_after};
use crate::transport::{Headers, HttpResponse, TransportError};
use omaghy_model::{AuthError, LimitKind, StoreError};
use time::{Duration, OffsetDateTime};

/// GitHub's standard error envelope. Present on essentially every 4xx.
#[derive(Debug, serde::Deserialize)]
struct ApiError {
    message: Option<String>,
}

/// The human-readable half of a failure response.
pub(crate) fn message_of(response: &HttpResponse) -> String {
    serde_json::from_slice::<ApiError>(&response.body)
        .ok()
        .and_then(|e| e.message)
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| {
            // Not JSON — a proxy's HTML error page, or an empty body. Neither
            // belongs in a footer, so fall back to the status line.
            format!("HTTP {}", response.status)
        })
}

fn mentions_a_secondary_limit(body: &str) -> bool {
    let b = body.to_ascii_lowercase();
    b.contains("secondary rate limit") || b.contains("abuse detection")
}

/// The scope a 403 is complaining about, if it is complaining about scopes.
///
/// GitHub advertises what the endpoint accepts in `X-Accepted-OAuth-Scopes`
/// and what the token holds in `X-OAuth-Scopes`. If the token holds none of
/// the accepted ones, this is a scope problem, and `gh auth refresh -s …` is
/// the fix rather than "ask someone for access".
fn missing_scope(headers: &Headers) -> Option<String> {
    let accepted: Vec<&str> = headers
        .get("x-accepted-oauth-scopes")?
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if accepted.is_empty() {
        return None;
    }
    let held: Vec<&str> = headers
        .get("x-oauth-scopes")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .collect();
    accepted
        .iter()
        .all(|a| !held.contains(a))
        .then(|| accepted[0].to_owned())
}

/// Map a failure response onto the taxonomy, and tell the governor about any
/// block it implies.
///
/// `now` is passed rather than read so the mapping is testable; the governor
/// keeps its own clock for everything else.
pub(crate) fn map_response(
    response: &HttpResponse,
    governor: &Governor,
    now: OffsetDateTime,
) -> StoreError {
    let h = &response.headers;
    let message = message_of(response);

    match response.status {
        401 => AuthError::Rejected.into(),

        403 | 429 => {
            // A spent primary budget. GitHub reports this as a 403 with the
            // remaining count at zero, which is otherwise indistinguishable
            // from a denial.
            let primary_spent = h.get_num::<u32>("x-ratelimit-remaining") == Some(0);
            if primary_spent {
                let at = h
                    .get_num::<i64>("x-ratelimit-reset")
                    .and_then(|t| OffsetDateTime::from_unix_timestamp(t).ok())
                    .unwrap_or(now + Duration::hours(1));
                governor.block_until(LimitKind::Primary, at);
                return StoreError::RateLimited {
                    kind: LimitKind::Primary,
                    at,
                };
            }

            let retry = retry_after(h);
            if response.status == 429 || retry.is_some() || mentions_a_secondary_limit(&message) {
                // The governor owns the penalty: it is exponential across
                // consecutive strikes, and it is never waited out inline.
                let at = governor.note_secondary_limit(retry);
                return StoreError::RateLimited {
                    kind: LimitKind::Secondary,
                    at,
                };
            }

            if response.status == 403 {
                match missing_scope(h) {
                    Some(scope) => AuthError::MissingScope { scope }.into(),
                    None => StoreError::Forbidden,
                }
            } else {
                StoreError::Upstream {
                    status: response.status,
                    message,
                }
            }
        }

        404 => StoreError::NotFound,

        status => StoreError::Upstream { status, message },
    }
}

impl From<TransportError> for StoreError {
    fn from(e: TransportError) -> Self {
        match e {
            // Every one of these means the request never got an answer. That
            // is `Offline`, and with a warm cache it is a banner rather than
            // an empty screen.
            TransportError::Unreachable(m) | TransportError::Timeout(m) => StoreError::Offline(m),
            TransportError::Body(m) => StoreError::Offline(m),
            TransportError::Malformed(m) => StoreError::Upstream {
                status: 0,
                message: m,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ratelimit::{Clock, Resource};
    use time::macros::datetime;

    const NOW: OffsetDateTime = datetime!(2026-09-12 08:00 UTC);

    fn gov() -> Governor {
        Governor::new(Clock::Fixed(NOW))
    }

    fn map(r: &HttpResponse) -> StoreError {
        map_response(r, &gov(), NOW)
    }

    #[test]
    fn a_rejected_token_is_auth_and_names_the_fix() {
        // Recorded shape: tests/cassettes/unauthorized.json
        let r = HttpResponse::new(401)
            .body(br#"{"message":"Bad credentials","status":"401"}"#.to_vec());
        let e = map(&r);
        assert!(matches!(e, StoreError::Auth(AuthError::Rejected)));
        assert!(e.terse().contains("gh auth login"));
    }

    #[test]
    fn forbidden_is_not_notfound() {
        let denied = map(&HttpResponse::new(403).body(
            br#"{"message":"Must have push access to view repository collaborators."}"#.to_vec(),
        ));
        assert_eq!(denied, StoreError::Forbidden);

        let gone = map(&HttpResponse::new(404).body(br#"{"message":"Not Found"}"#.to_vec()));
        assert_eq!(gone, StoreError::NotFound);

        // They differ in what the UI does next: one is retried, one never is.
        assert!(!denied.is_retryable());
        assert!(!gone.is_retryable());
    }

    #[test]
    fn a_403_with_a_spent_budget_is_a_rate_limit_not_a_denial() {
        let reset = (NOW + Duration::minutes(30)).unix_timestamp();
        let r = HttpResponse::new(403)
            .header("x-ratelimit-remaining", "0")
            .header("x-ratelimit-reset", reset.to_string())
            .body(br#"{"message":"API rate limit exceeded for user ID 1."}"#.to_vec());
        assert!(matches!(
            map(&r),
            StoreError::RateLimited {
                kind: LimitKind::Primary,
                ..
            }
        ));
    }

    #[test]
    fn a_403_naming_the_secondary_limit_is_secondary() {
        let r = HttpResponse::new(403)
            .header("x-ratelimit-remaining", "4200")
            .body(
                br#"{"message":"You have exceeded a secondary rate limit. Please wait a few minutes before you try again."}"#
                    .to_vec(),
            );
        assert!(matches!(
            map(&r),
            StoreError::RateLimited {
                kind: LimitKind::Secondary,
                ..
            }
        ));
    }

    #[test]
    fn a_429_is_secondary_and_honours_retry_after() {
        let r = HttpResponse::new(429).header("retry-after", "45");
        match map(&r) {
            StoreError::RateLimited {
                kind: LimitKind::Secondary,
                at,
            } => assert_eq!(at, NOW + Duration::seconds(45)),
            other => panic!("expected a secondary limit, got {other:?}"),
        }
    }

    #[test]
    fn a_rate_limit_response_blocks_the_governor_too() {
        let g = gov();
        let r = HttpResponse::new(429).header("retry-after", "45");
        let _ = map_response(&r, &g, NOW);
        assert_eq!(g.blocked_for(), Some(Duration::seconds(45)));
        assert!(g.check(Resource::GraphQl).is_err(), "a block is global");
    }

    #[test]
    fn a_403_about_scopes_names_the_scope_to_add() {
        let r = HttpResponse::new(403)
            .header("x-accepted-oauth-scopes", "workflow")
            .header("x-oauth-scopes", "gist, read:org, repo")
            .body(br#"{"message":"Resource not accessible by personal access token"}"#.to_vec());
        match map(&r) {
            StoreError::Auth(AuthError::MissingScope { scope }) => assert_eq!(scope, "workflow"),
            other => panic!("expected a scope error, got {other:?}"),
        }
    }

    #[test]
    fn a_403_whose_scope_we_already_hold_is_plain_denial() {
        // Recorded on the live API: /notifications accepts `notifications, repo`
        // and the token holds `repo`. A scope error here would be a lie.
        let r = HttpResponse::new(403)
            .header("x-accepted-oauth-scopes", "notifications, repo")
            .header("x-oauth-scopes", "gist, read:org, repo, workflow")
            .body(br#"{"message":"Forbidden"}"#.to_vec());
        assert_eq!(map(&r), StoreError::Forbidden);
    }

    #[test]
    fn a_5xx_is_upstream_and_retryable() {
        let e = map(&HttpResponse::new(502).body(b"<html>bad gateway</html>".to_vec()));
        match &e {
            StoreError::Upstream { status, message } => {
                assert_eq!(*status, 502);
                assert_eq!(message, "HTTP 502", "an HTML error page is not a message");
            }
            other => panic!("expected upstream, got {other:?}"),
        }
        assert!(e.is_retryable());
    }

    #[test]
    fn a_422_carries_githubs_own_words() {
        let e = map(&HttpResponse::new(422)
            .body(br#"{"message":"Validation Failed","errors":[]}"#.to_vec()));
        assert_eq!(
            e,
            StoreError::Upstream {
                status: 422,
                message: "Validation Failed".to_owned()
            }
        );
        assert!(!e.is_retryable(), "retrying a rejected body cannot help");
    }

    #[test]
    fn unreachable_is_offline_and_keeps_the_cache() {
        for t in [
            TransportError::Unreachable("dns error: no record".to_owned()),
            TransportError::Timeout("operation timed out".to_owned()),
        ] {
            let e: StoreError = t.into();
            assert!(matches!(e, StoreError::Offline(_)));
            assert!(e.is_retryable());
            assert!(e.keeps_cached_content());
        }
    }
}
