//! GraphQL, by hand.
//!
//! Queries are hand-written strings deserialised with `serde_json`, not
//! generated. That decision is recorded in `spec/00-overview.md` §4.1: `cynic`
//! would need GitHub's ~5 MB schema committed and a codegen step, M1 has under
//! a dozen queries, and the translation boundary to `omaghy-model` is
//! mandatory anyway — so adopting it later is a change contained entirely
//! within this crate.
//!
//! The awkward part of GraphQL is that failure arrives as **HTTP 200 with an
//! `errors` array**. Verified against the live API: a query for a repository
//! that does not exist returns 200, `"data":{"repository":null}`, and an error
//! of `"type":"NOT_FOUND"`. Mapping that onto [`StoreError::NotFound`] is the
//! job of this module; a caller that only checked the status would report
//! success.

use omaghy_model::{LimitKind, StoreError};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use time::{Duration, OffsetDateTime};

/// One query or mutation, ready to post.
#[derive(Debug, Clone, Serialize)]
pub struct GraphQlRequest {
    pub query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variables: Option<Value>,
    /// Whether this changes anything. `POST /graphql` is a read most of the
    /// time, so the method cannot tell us — and getting this wrong is how a
    /// review gets submitted twice.
    #[serde(skip)]
    pub mutation: bool,
}

impl GraphQlRequest {
    pub fn query(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            variables: None,
            mutation: false,
        }
    }

    pub fn mutation(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            variables: None,
            mutation: true,
        }
    }

    #[must_use]
    pub fn variables(mut self, variables: Value) -> Self {
        self.variables = Some(variables);
        self
    }

    /// A short label for logs and traces. The first named operation, or the
    /// first line — never the whole query, which is kilobytes.
    pub fn label(&self) -> &str {
        self.query
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("query")
    }
}

/// One entry of GitHub's `errors` array.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct GraphQlError {
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub path: Vec<Value>,
}

#[derive(Debug, Deserialize)]
struct Envelope<T> {
    // No `#[serde(default)]`: serde already reads a missing `Option` field as
    // `None`, and asking for a default would demand `T: Default` from every
    // caller's response type.
    data: Option<T>,
    #[serde(default)]
    errors: Vec<GraphQlError>,
}

/// GitHub's `rateLimit` field, when a query asks for it.
///
/// Worth asking for: it is the only way to see the *cost* of a query, and cost
/// is what the GraphQL budget is denominated in.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitField {
    pub limit: u32,
    pub cost: u32,
    pub remaining: u32,
    #[serde(with = "time::serde::rfc3339")]
    pub reset_at: OffsetDateTime,
}

/// Decode a GraphQL response body.
///
/// Partial success — `data` alongside `errors` — is treated as failure. GitHub
/// uses it for "this one field was forbidden", and rendering a dashboard with
/// a silently missing section is worse than saying so.
pub fn decode<T: DeserializeOwned>(body: &[u8], now: OffsetDateTime) -> Result<T, StoreError> {
    let envelope: Envelope<T> = serde_json::from_slice(body).map_err(|e| StoreError::Upstream {
        status: 200,
        message: format!("GraphQL response was not understood: {e}"),
    })?;

    if let Some(first) = envelope.errors.first() {
        return Err(map_graphql_error(first, &envelope.errors, now));
    }

    envelope.data.ok_or_else(|| StoreError::Upstream {
        status: 200,
        message: "GraphQL returned neither data nor errors".to_owned(),
    })
}

/// A response that kept both halves.
///
/// See [`decode_partial`] for when this is the right answer and [`decode`] for
/// when it is not.
#[derive(Debug, Clone)]
pub struct Partial<T> {
    pub data: Option<T>,
    pub errors: Vec<GraphQlError>,
}

impl<T> Partial<T> {
    /// GitHub's message for one aliased field, if that field failed.
    ///
    /// An error's `path` names the field it belongs to — verified live, a
    /// missing repository under alias `s3` arrives as `path: ["s3"]` and a
    /// missing number under it as `path: ["s3", "issueOrPullRequest"]`. The
    /// first segment is therefore the alias in both cases.
    pub fn message_for(&self, alias: &str) -> Option<String> {
        self.errors
            .iter()
            .find(|e| e.path.first().and_then(Value::as_str) == Some(alias))
            .map(|e| e.message.clone())
    }
}

/// Decode a GraphQL response **keeping per-field errors**.
///
/// This is the exception to [`decode`]'s rule, and it exists for exactly one
/// shape: a query that asks fifty independent questions under fifty aliases.
/// There, an error is information *about one alias* — verified against the
/// live API, a batch naming one repository we cannot see answers HTTP 200 with
/// the other forty-nine resolved and a single `NOT_FOUND` pointing at the one.
/// Failing the batch would throw away forty-nine good answers to report one
/// permanent failure, and the caller would fetch them all again on the next
/// open.
///
/// It is still a failure when `data` is absent: a rate limit, an unparseable
/// query and an expired token all arrive that way, and none of them is news
/// about a particular field.
pub fn decode_partial<T: DeserializeOwned>(
    body: &[u8],
    now: OffsetDateTime,
) -> Result<Partial<T>, StoreError> {
    let envelope: Envelope<T> = serde_json::from_slice(body).map_err(|e| StoreError::Upstream {
        status: 200,
        message: format!("GraphQL response was not understood: {e}"),
    })?;

    match envelope.data {
        Some(data) => Ok(Partial {
            data: Some(data),
            errors: envelope.errors,
        }),
        None => Err(match envelope.errors.first() {
            Some(first) => map_graphql_error(first, &envelope.errors, now),
            None => StoreError::Upstream {
                status: 200,
                message: "GraphQL returned neither data nor errors".to_owned(),
            },
        }),
    }
}

fn map_graphql_error(
    first: &GraphQlError,
    all: &[GraphQlError],
    now: OffsetDateTime,
) -> StoreError {
    let joined = || {
        all.iter()
            .map(|e| e.message.as_str())
            .collect::<Vec<_>>()
            .join("; ")
    };

    match first.r#type.as_deref() {
        Some("NOT_FOUND") => StoreError::NotFound,
        Some("FORBIDDEN") => StoreError::Forbidden,
        Some("RATE_LIMITED") => StoreError::RateLimited {
            kind: LimitKind::Primary,
            at: now + Duration::hours(1),
        },
        // `UNAUTHORIZED`, and anything else typed, is still a 200 as far as
        // HTTP is concerned. `Upstream { status: 200 }` reads oddly and is
        // deliberate: it is not retryable, which is correct — a malformed
        // query does not get better on the second attempt.
        _ => StoreError::Upstream {
            status: 200,
            message: joined(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use time::macros::datetime;

    const NOW: OffsetDateTime = datetime!(2026-09-12 08:00 UTC);

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct Viewer {
        viewer: Login,
    }
    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct Login {
        login: String,
    }

    #[test]
    fn a_query_decodes_into_our_own_type() {
        // The exact body recorded from the live API.
        let body = br#"{"data":{"viewer":{"login":"ShaxP"},"rateLimit":{"limit":5000,"cost":1,"remaining":4986,"resetAt":"2026-09-12T08:23:58Z"}}}"#;
        let v: Viewer = decode(body, NOW).unwrap();
        assert_eq!(v.viewer.login, "ShaxP");
    }

    #[test]
    fn a_200_carrying_errors_is_a_failure() {
        // Recorded: a repository that does not exist.
        let body = br#"{"data":{"repository":null},"errors":[{"type":"NOT_FOUND","path":["repository"],"message":"Could not resolve to a Repository with the name 'ShaxP/nope'."}]}"#;
        let e = decode::<Value>(body, NOW).unwrap_err();
        assert_eq!(e, StoreError::NotFound, "a 200 is not success here");
    }

    #[test]
    fn a_forbidden_field_is_forbidden_not_missing() {
        let body =
            br#"{"data":null,"errors":[{"type":"FORBIDDEN","message":"Resource not accessible"}]}"#;
        assert_eq!(
            decode::<Value>(body, NOW).unwrap_err(),
            StoreError::Forbidden
        );
    }

    #[test]
    fn a_rate_limited_query_is_a_rate_limit() {
        let body = br#"{"errors":[{"type":"RATE_LIMITED","message":"API rate limit exceeded"}]}"#;
        assert!(matches!(
            decode::<Value>(body, NOW).unwrap_err(),
            StoreError::RateLimited {
                kind: LimitKind::Primary,
                ..
            }
        ));
    }

    #[test]
    fn an_untyped_error_keeps_githubs_wording() {
        // Recorded: a field that does not exist on Query. There is no `type`.
        let body = br#"{"errors":[{"path":["query","nope"],"extensions":{"code":"undefinedField"},"message":"Field 'nope' doesn't exist on type 'Query'"}]}"#;
        let e = decode::<Value>(body, NOW).unwrap_err();
        match &e {
            StoreError::Upstream { status, message } => {
                assert_eq!(*status, 200);
                assert!(message.contains("doesn't exist"), "{message}");
            }
            other => panic!("expected upstream, got {other:?}"),
        }
        assert!(!e.is_retryable(), "a bad query never gets better");
    }

    #[test]
    fn partial_success_is_not_success() {
        let body = br#"{"data":{"a":1},"errors":[{"type":"FORBIDDEN","message":"nope"}]}"#;
        assert!(
            decode::<Value>(body, NOW).is_err(),
            "a silently missing section is worse than an error"
        );
    }

    #[test]
    fn a_batch_keeps_the_answers_it_did_get() {
        // The exact body recorded from the live API: three aliases resolved,
        // one repository that does not exist. Failing the whole thing would
        // throw away three good answers and re-fetch them on the next open.
        let body = br#"{"data":{"s0":{"n":1},"s1":{"n":2},"s2":{"n":3},"s3":null},
            "errors":[{"type":"NOT_FOUND","path":["s3"],
                       "message":"Could not resolve to a Repository with the name 'o/r'."}]}"#;
        let partial: Partial<Value> = decode_partial(body, NOW).unwrap();
        assert_eq!(partial.data.as_ref().unwrap()["s0"]["n"], 1);
        assert!(
            partial.message_for("s3").unwrap().contains("Could not"),
            "the failed alias is nameable, so one row can be marked Failed"
        );
        assert_eq!(partial.message_for("s0"), None);
    }

    #[test]
    fn an_error_deeper_than_the_alias_still_names_the_alias() {
        // Recorded: a number that does not exist inside a repository that
        // does. GitHub points at the field, not at the alias.
        let body = br#"{"data":{"s3":{"issueOrPullRequest":null}},
            "errors":[{"type":"NOT_FOUND","path":["s3","issueOrPullRequest"],
                       "message":"Could not resolve to an issue or pull request with the number of 999999."}]}"#;
        let partial: Partial<Value> = decode_partial(body, NOW).unwrap();
        assert!(partial.message_for("s3").unwrap().contains("999999"));
    }

    #[test]
    fn a_batch_with_no_data_at_all_is_still_a_failure() {
        // A rate limit, a bad token and an unparseable query all arrive this
        // way, and none of them is news about one alias.
        let body = br#"{"data":null,"errors":[{"type":"RATE_LIMITED","message":"exceeded"}]}"#;
        assert!(matches!(
            decode_partial::<Value>(body, NOW).unwrap_err(),
            StoreError::RateLimited { .. }
        ));
    }

    #[test]
    fn a_body_that_is_not_graphql_at_all_is_upstream() {
        let e = decode::<Value>(b"<html>502</html>", NOW).unwrap_err();
        assert!(matches!(e, StoreError::Upstream { status: 200, .. }));
    }

    #[test]
    fn a_request_serialises_the_way_github_expects() {
        let req = GraphQlRequest::query("query { viewer { login } }")
            .variables(serde_json::json!({"first": 25}));
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["query"], "query { viewer { login } }");
        assert_eq!(json["variables"]["first"], 25);
        assert!(json.get("mutation").is_none(), "ours, not GitHub's");
    }

    #[test]
    fn a_mutation_says_so() {
        assert!(!GraphQlRequest::query("query { viewer { login } }").mutation);
        assert!(GraphQlRequest::mutation("mutation { addComment { id } }").mutation);
    }

    #[test]
    fn the_rate_limit_field_decodes() {
        let f: RateLimitField = serde_json::from_str(
            r#"{"limit":5000,"cost":1,"remaining":4986,"resetAt":"2026-09-12T08:23:58Z"}"#,
        )
        .unwrap();
        assert_eq!(f.cost, 1);
        assert_eq!(f.reset_at, datetime!(2026-09-12 08:23:58 UTC));
    }
}
