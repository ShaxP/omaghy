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
        if let Some(s) = &self.search {
            k.push_str(&format!(":q={s}"));
        }
        k
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
    fn default_dashboard_leads_with_what_needs_me() {
        let d = DashboardConfig::default();
        assert_eq!(d.sections[0].title, "Needs my review");
        assert!(d.sections[0].query.contains("review-requested:@me"));
    }
}
