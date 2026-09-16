//! What a surface asks for. Queries are values so they can be compared,
//! cached under, and used as refresh targets.

use omaghy_model::{NotificationReason, RepoRef, SubjectKind};
use serde::{Deserialize, Serialize};

/// A page of results plus whether more exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    /// Opaque continuation token. `None` means this is the last page.
    pub cursor: Option<String>,
    /// Total as reported by GitHub, where it reports one. Often absent, and
    /// often an estimate when present — never use it to size a scrollbar.
    pub total: Option<u32>,
}

impl<T> Page<T> {
    pub fn complete(items: Vec<T>) -> Self {
        let total = items.len() as u32;
        Self {
            items,
            cursor: None,
            total: Some(total),
        }
    }

    pub fn empty() -> Self {
        Self {
            items: Vec::new(),
            cursor: None,
            total: Some(0),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadFilter {
    #[default]
    All,
    UnreadOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NotificationQuery {
    pub read: ReadFilter,
    /// Restrict to one repository.
    pub repo: Option<RepoRef>,
    /// Restrict to specific reasons. Empty means all.
    pub reasons: Vec<NotificationReason>,
    pub kinds: Vec<SubjectKind>,
    /// Free text, matched against title and repository name.
    pub search: Option<String>,
    pub limit: Option<usize>,
}

impl NotificationQuery {
    pub fn unread() -> Self {
        Self {
            read: ReadFilter::UnreadOnly,
            ..Default::default()
        }
    }

    /// A stable key for caching and for coalescing refreshes.
    ///
    /// Two queries that differ only in `limit` share a key: the limit is a
    /// view concern, and fetching twice for it would be waste.
    pub fn cache_key(&self) -> String {
        let mut k = String::from("notifications");
        if self.read == ReadFilter::UnreadOnly {
            k.push_str(":unread");
        }
        if let Some(r) = &self.repo {
            k.push_str(&format!(":repo={r}"));
        }
        for reason in &self.reasons {
            k.push_str(&format!(":reason={}", reason.label()));
        }
        // `kinds` narrows the result set exactly as `reasons` does, so omitting
        // it made two different queries share a key. Harmless while the cache
        // holds one inbox per viewer and filters in Rust, but it silently
        // misleads anyone using this as a list key. Found by W1.2.
        for kind in &self.kinds {
            k.push_str(&format!(":kind={kind:?}"));
        }
        if let Some(s) = &self.search {
            k.push_str(&format!(":q={s}"));
        }
        k
    }
}

/// What a pull-request list is a list *of*.
///
/// GitHub search syntax, because that is the only vocabulary that can say
/// both "this repository's open PRs" and "everything awaiting my review
/// across every repository" — and the second is the list a client is for.
/// `00-overview.md` §2 allowed "repo- or query-scoped"; a repo scope is one
/// query among others, so the type has one field and [`PrQuery::repo`] is
/// sugar.
///
/// Also the [`RefreshTarget`](crate::RefreshTarget), so the fetcher receives
/// the query and not a key it would have to parse back.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PrQuery {
    /// Verbatim from the caller. Read it through [`PrQuery::effective`].
    pub query: String,
}

impl PrQuery {
    /// Any GitHub search string. `is:pr` is implied; see [`Self::effective`].
    pub fn search(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
        }
    }

    /// One repository's open pull requests — what `pr:owner/name` opens.
    pub fn repo(repo: &RepoRef) -> Self {
        Self::search(format!("repo:{repo} is:open"))
    }

    /// The query as it is sent, and as it is keyed.
    ///
    /// A pull-request list that returns issues is wrong, so `is:pr` is added
    /// when the caller left it out. Done here, once, so the key and the fetch
    /// cannot disagree — and so `search("author:@me")` and
    /// `search("is:pr author:@me")` are one list, not two fetches.
    pub fn effective(&self) -> String {
        let q = self.query.trim();
        if q.split_whitespace().any(|t| t == "is:pr") {
            q.to_owned()
        } else if q.is_empty() {
            "is:pr".to_owned()
        } else {
            format!("is:pr {q}")
        }
    }

    /// A stable key for caching and for coalescing refreshes.
    pub fn cache_key(&self) -> String {
        format!("prs:{}", self.effective())
    }
}

/// One section of the dashboard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DashboardSection {
    pub title: String,
    /// GitHub search syntax, e.g. `is:open is:pr review-requested:@me`.
    pub query: String,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DashboardConfig {
    pub sections: Vec<DashboardSection>,
}

impl Default for DashboardConfig {
    /// The sections that answer "what needs me", in the order they do.
    fn default() -> Self {
        let s = |title: &str, query: &str| DashboardSection {
            title: title.into(),
            query: query.into(),
            limit: 10,
        };
        Self {
            sections: vec![
                s("Needs my review", "is:open is:pr review-requested:@me"),
                s("My pull requests", "is:open is:pr author:@me"),
                s("Assigned to me", "is:open assignee:@me"),
                s("Recently mentioned", "is:open mentions:@me"),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_keys_distinguish_what_matters() {
        let all = NotificationQuery::default();
        let unread = NotificationQuery::unread();
        assert_ne!(all.cache_key(), unread.cache_key());

        let repo = NotificationQuery {
            repo: RepoRef::parse("ShaxP/shax"),
            ..Default::default()
        };
        assert_ne!(all.cache_key(), repo.cache_key());
        assert!(repo.cache_key().contains("ShaxP/shax"));
    }

    #[test]
    fn kinds_change_the_cache_key_just_as_reasons_do() {
        use omaghy_model::SubjectKind;
        let all = NotificationQuery::default();
        let prs = NotificationQuery {
            kinds: vec![SubjectKind::PullRequest],
            ..Default::default()
        };
        let issues = NotificationQuery {
            kinds: vec![SubjectKind::Issue],
            ..Default::default()
        };
        assert_ne!(all.cache_key(), prs.cache_key());
        assert_ne!(
            prs.cache_key(),
            issues.cache_key(),
            "two filters must not share a key"
        );
    }

    #[test]
    fn limit_does_not_change_the_cache_key() {
        // The limit is a view concern. Fetching twice for it would be waste.
        let a = NotificationQuery {
            limit: Some(10),
            ..Default::default()
        };
        let b = NotificationQuery {
            limit: Some(50),
            ..Default::default()
        };
        assert_eq!(a.cache_key(), b.cache_key());
    }

    #[test]
    fn an_empty_page_is_not_a_missing_page() {
        let p: Page<u8> = Page::empty();
        assert!(p.is_empty());
        assert_eq!(p.total, Some(0));
        assert!(p.cursor.is_none());
    }

    #[test]
    fn a_pr_query_is_always_a_pr_query() {
        assert_eq!(
            PrQuery::search("author:@me").effective(),
            "is:pr author:@me"
        );
        assert_eq!(
            PrQuery::search("is:open is:pr author:@me").effective(),
            "is:open is:pr author:@me",
            "already there: left where it was"
        );
        assert_eq!(PrQuery::search("  ").effective(), "is:pr");
        // `is:private` is not `is:pr`; a prefix match would have said it was.
        assert_eq!(
            PrQuery::search("is:private").effective(),
            "is:pr is:private"
        );
    }

    #[test]
    fn two_spellings_of_one_list_share_a_key() {
        assert_eq!(
            PrQuery::search("author:@me").cache_key(),
            PrQuery::search("is:pr author:@me").cache_key()
        );
        assert_ne!(
            PrQuery::search("author:@me").cache_key(),
            PrQuery::search("review-requested:@me").cache_key()
        );
    }

    #[test]
    fn a_repo_scope_is_a_query_like_any_other() {
        let q = PrQuery::repo(&RepoRef::new("ShaxP", "shax"));
        assert_eq!(q.effective(), "is:pr repo:ShaxP/shax is:open");
        assert!(q.cache_key().starts_with("prs:"));
    }

    #[test]
    fn default_dashboard_leads_with_what_needs_me() {
        let d = DashboardConfig::default();
        assert_eq!(d.sections[0].title, "Needs my review");
        assert!(d.sections[0].query.contains("review-requested:@me"));
    }
}
