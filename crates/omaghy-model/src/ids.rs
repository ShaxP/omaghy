//! Identity. Two id spaces, and conflating them is a bug.
//!
//! See `spec/10-domain-model.md` §2.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A GraphQL global node id, e.g. `PR_kwDOA...`. The cache key for anything
/// that has one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeId(pub String);

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A REST notification id. Unrelated to [`NodeId`] — notifications are a
/// REST-only surface with their own id space.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NotificationId(pub String);

impl fmt::Display for NotificationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a notification is about.
///
/// **The `serde` representation here is omaghy's storage format, not GitHub's
/// wire format.** GitHub sends `"PullRequest"`; this serialises
/// `"pull_request"`, and `omaghy-api` translates between them. That is the
/// rule in `spec/10-domain-model.md` §1 — no GraphQL-generated type and no
/// wire shape reaches this crate — and it is why deriving `Deserialize`
/// straight onto a `/notifications` response will not work, and should not.
///
/// W2.1 filed this as a contract bug; it is the design. The note is here so
/// the next reader does not file it again.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubjectKind {
    PullRequest,
    Issue,
    Discussion,
    Release,
    CheckSuite,
    Commit,
    VulnerabilityAlert,
    /// GitHub adds subject types without warning.
    Other(String),
}

impl SubjectKind {
    /// The path segment GitHub's REST API uses for this kind.
    ///
    /// Public because `omaghy-api` builds request URLs from it.
    pub fn api_segment(&self) -> Option<&'static str> {
        match self {
            Self::PullRequest => Some("pulls"),
            Self::Issue => Some("issues"),
            Self::Discussion => Some("discussions"),
            Self::Release => Some("releases"),
            Self::CheckSuite => Some("check-suites"),
            Self::Commit => Some("commits"),
            Self::VulnerabilityAlert | Self::Other(_) => None,
        }
    }

    fn from_api_segment(seg: &str) -> Option<Self> {
        Some(match seg {
            "pulls" => Self::PullRequest,
            "issues" => Self::Issue,
            "discussions" => Self::Discussion,
            "releases" => Self::Release,
            "check-suites" => Self::CheckSuite,
            "commits" => Self::Commit,
            _ => return None,
        })
    }
}

/// How a subject is addressed within its repository.
///
/// Not always a number: commits are addressed by SHA. `spec/10-domain-model.md`
/// §2 declared this `number: u64`, which cannot represent a commit subject —
/// corrected here.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SubjectId {
    Number(u64),
    Sha(String),
}

impl fmt::Display for SubjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(n) => write!(f, "#{n}"),
            Self::Sha(s) => f.write_str(&s[..s.len().min(7)]),
        }
    }
}

/// A human coordinate for a subject, derivable without a network call.
///
/// This is what `o` (open in browser) uses, and it is how an unenriched
/// notification still offers a useful action.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SubjectRef {
    pub owner: String,
    pub repo: String,
    pub kind: SubjectKind,
    pub id: SubjectId,
}

impl SubjectRef {
    /// Parse a REST API URL, e.g.
    /// `https://api.github.com/repos/ShaxP/shax/pulls/61`.
    ///
    /// Notification payloads carry only this; everything else about the
    /// subject requires enrichment.
    pub fn from_api_url(url: &str) -> Option<Self> {
        // …/repos/{owner}/{repo}/{segment}/{id}
        let rest = url.split("/repos/").nth(1)?;
        let mut parts = rest.split('/').filter(|s| !s.is_empty());

        let owner = parts.next()?.to_owned();
        let repo = parts.next()?.to_owned();
        let segment = parts.next()?;
        let tail = parts.next()?;
        if parts.next().is_some() {
            return None; // deeper than a subject URL — not one
        }

        let kind = SubjectKind::from_api_segment(segment)?;
        let id = match kind {
            SubjectKind::Commit => SubjectId::Sha(tail.to_owned()),
            _ => SubjectId::Number(tail.parse().ok()?),
        };
        Some(Self {
            owner,
            repo,
            kind,
            id,
        })
    }

    /// The coordinate of a pull request, from things a list row already has.
    pub fn pull_request(repo: &crate::repo::RepoRef, number: u64) -> Self {
        Self {
            owner: repo.owner.clone(),
            repo: repo.name.clone(),
            kind: SubjectKind::PullRequest,
            id: SubjectId::Number(number),
        }
    }

    /// `owner/repo#61`, the form a route carries (`spec/30-ui.md` §3.2).
    ///
    /// The inverse of [`fmt::Display`] for the numbered kinds only; a route
    /// never names a commit. `kind` is the caller's, because the string does
    /// not say — `ShaxP/shax#61` is a PR on one surface and an issue on
    /// another.
    pub fn parse_numbered(s: &str, kind: SubjectKind) -> Option<Self> {
        let (full_name, number) = s.trim().split_once('#')?;
        let repo = crate::repo::RepoRef::parse(full_name)?;
        let number: u64 = number.parse().ok()?;
        Some(Self {
            owner: repo.owner,
            repo: repo.name,
            kind,
            id: SubjectId::Number(number),
        })
    }

    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }

    /// The github.com URL for this subject.
    ///
    /// `None` where it cannot be derived from the API URL alone — releases are
    /// addressed by tag on the web but by numeric id in the API, so the two
    /// are not interconvertible without a fetch.
    pub fn browser_url(&self) -> Option<String> {
        let (owner, repo) = (&self.owner, &self.repo);
        let path = match (&self.kind, &self.id) {
            (SubjectKind::PullRequest, SubjectId::Number(n)) => format!("pull/{n}"),
            (SubjectKind::Issue, SubjectId::Number(n)) => format!("issues/{n}"),
            (SubjectKind::Discussion, SubjectId::Number(n)) => format!("discussions/{n}"),
            (SubjectKind::Commit, SubjectId::Sha(s)) => format!("commit/{s}"),
            _ => return None,
        };
        Some(format!("https://github.com/{owner}/{repo}/{path}"))
    }
}

impl fmt::Display for SubjectRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}{}", self.owner, self.repo, self.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_real_pull_request_url() {
        // Taken verbatim from a live notification payload.
        let r = SubjectRef::from_api_url("https://api.github.com/repos/ShaxP/shax/pulls/61")
            .expect("should parse");
        assert_eq!(r.owner, "ShaxP");
        assert_eq!(r.repo, "shax");
        assert_eq!(r.kind, SubjectKind::PullRequest);
        assert_eq!(r.id, SubjectId::Number(61));
        assert_eq!(
            r.browser_url().as_deref(),
            Some("https://github.com/ShaxP/shax/pull/61")
        );
    }

    #[test]
    fn parses_issue_and_discussion() {
        let i = SubjectRef::from_api_url("https://api.github.com/repos/o/r/issues/7").unwrap();
        assert_eq!(i.kind, SubjectKind::Issue);
        assert_eq!(
            i.browser_url().as_deref(),
            Some("https://github.com/o/r/issues/7")
        );

        let d = SubjectRef::from_api_url("https://api.github.com/repos/o/r/discussions/9").unwrap();
        assert_eq!(d.kind, SubjectKind::Discussion);
    }

    #[test]
    fn commits_are_addressed_by_sha_not_number() {
        let sha = "9e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f";
        let c =
            SubjectRef::from_api_url(&format!("https://api.github.com/repos/o/r/commits/{sha}"))
                .unwrap();
        assert_eq!(c.kind, SubjectKind::Commit);
        assert_eq!(c.id, SubjectId::Sha(sha.to_owned()));
        assert_eq!(c.id.to_string(), "9e1f2a3");
    }

    #[test]
    fn releases_parse_but_have_no_derivable_browser_url() {
        // The web addresses releases by tag; the API by numeric id. Not
        // interconvertible without a fetch.
        let r = SubjectRef::from_api_url("https://api.github.com/repos/o/r/releases/12").unwrap();
        assert_eq!(r.kind, SubjectKind::Release);
        assert_eq!(r.browser_url(), None);
    }

    #[test]
    fn rejects_urls_that_are_not_subjects() {
        for url in [
            "https://api.github.com/repos/o/r",
            "https://api.github.com/repos/o/r/pulls/61/comments",
            "https://api.github.com/repos/o/r/pulls/not-a-number",
            "https://api.github.com/notifications",
            "",
        ] {
            assert_eq!(SubjectRef::from_api_url(url), None, "should reject {url}");
        }
    }

    #[test]
    fn a_route_coordinate_round_trips_through_display() {
        let r = SubjectRef::parse_numbered("ShaxP/shax#61", SubjectKind::PullRequest).unwrap();
        assert_eq!(r.to_string(), "ShaxP/shax#61");
        assert_eq!(r.kind, SubjectKind::PullRequest);
        assert_eq!(r.id, SubjectId::Number(61));
        assert_eq!(
            r,
            SubjectRef::pull_request(&crate::repo::RepoRef::new("ShaxP", "shax"), 61)
        );
        // The same string is an issue when the surface says so.
        let i = SubjectRef::parse_numbered("ShaxP/shax#61", SubjectKind::Issue).unwrap();
        assert_eq!(i.kind, SubjectKind::Issue);
    }

    #[test]
    fn a_route_coordinate_rejects_what_is_not_one() {
        for s in [
            "ShaxP/shax",
            "ShaxP/shax#",
            "ShaxP/shax#x",
            "shax#61",
            "a/b/c#1",
            "",
        ] {
            assert_eq!(
                SubjectRef::parse_numbered(s, SubjectKind::PullRequest),
                None,
                "should reject {s:?}"
            );
        }
    }
}
