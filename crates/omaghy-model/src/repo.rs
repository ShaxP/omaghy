//! Repositories. See `spec/10-domain-model.md` §3.

use crate::ids::NodeId;
use serde::{Deserialize, Serialize};
use std::fmt;

/// The minimum needed to name a repository in a list row.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RepoRef {
    pub owner: String,
    pub name: String,
}

impl RepoRef {
    pub fn new(owner: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            owner: owner.into(),
            name: name.into(),
        }
    }

    /// Parse `owner/name`. Rejects anything else.
    pub fn parse(full_name: &str) -> Option<Self> {
        let (owner, name) = full_name.split_once('/')?;
        if owner.is_empty() || name.is_empty() || name.contains('/') {
            return None;
        }
        Some(Self::new(owner, name))
    }
}

impl fmt::Display for RepoRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repo {
    pub node_id: NodeId,
    pub r#ref: RepoRef,
    pub is_private: bool,
    pub description: Option<String>,
    pub default_branch: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_names() {
        assert_eq!(
            RepoRef::parse("ShaxP/shax"),
            Some(RepoRef::new("ShaxP", "shax"))
        );
        assert_eq!(
            RepoRef::parse("some-very-long-organization-name/an-equally-long-repository-name")
                .map(|r| r.to_string())
                .as_deref(),
            Some("some-very-long-organization-name/an-equally-long-repository-name")
        );
    }

    #[test]
    fn rejects_malformed_names() {
        for s in ["", "noslash", "/name", "owner/", "a/b/c"] {
            assert_eq!(RepoRef::parse(s), None, "should reject {s:?}");
        }
    }
}
