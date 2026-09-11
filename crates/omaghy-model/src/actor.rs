//! Users, orgs and bots — one type. See `spec/10-domain-model.md` §3.

use crate::ids::NodeId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    pub login: String,
    /// Absent for ghost actors — deleted accounts, and some bots.
    pub node_id: Option<NodeId>,
    pub avatar_url: Option<String>,
    pub is_bot: bool,
}

impl Actor {
    pub fn new(login: impl Into<String>) -> Self {
        Self {
            login: login.into(),
            node_id: None,
            avatar_url: None,
            is_bot: false,
        }
    }

    /// What to render when there is no actor at all.
    pub const GHOST: &'static str = "ghost";

    pub fn display(this: Option<&Self>) -> &str {
        this.map_or(Self::GHOST, |a| a.login.as_str())
    }
}
