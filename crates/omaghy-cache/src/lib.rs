//! The SQLite cache, and the `Store` that reads from it.
//!
//! `spec/20-store.md` §3–§5. Four decisions here are load-bearing and are not
//! style:
//!
//! **Viewer keying is not optional.** Every table carries `viewer` and every
//! statement filters on it. The questions this cache answers —
//! `i_am_requested`, `my_review`, `unread` — are all "does this need *me*", so
//! a cache shared across accounts silently answers the wrong question, which
//! is worse than failing. See [`cache`].
//!
//! **List membership is stored apart from entities.** One PR updating must not
//! invalidate every list it appears in.
//!
//! **Bodies are serialized `omaghy-model` types**, never raw API JSON.
//! Translation happens once, at the `omaghy-api` boundary, never on a cache
//! read. That also makes the model part of the schema: a model change bumps
//! [`SCHEMA_VERSION`].
//!
//! **A version mismatch deletes the database and rebuilds.** It is a cache;
//! migrations would be effort spent protecting data we can re-fetch. See
//! [`schema`].
//!
//! # What is wired, and what is not
//!
//! Reads, freshness, viewer keying, optimistic mutation and rollback are
//! complete. The network is not: [`SqliteStore::refresh`] records intent and
//! emits events, and the fetch behind it arrives at M1 integration through the
//! [`remote::Remote`] trait — see that module for why the dependency is
//! inverted rather than `omaghy-cache` making requests itself.

pub mod cache;
mod error;
pub mod remote;
pub mod schema;
pub mod store;
pub mod ttl;

#[cfg(test)]
mod tests;

pub use cache::{
    Cache, EntityKind, KV_POLL_INTERVAL, ListMeta, NOTIFICATIONS_LIST, Stored, Validators,
    dashboard_list_key, pr_detail_key,
};
pub use remote::{RecordIntent, Remote};
pub use schema::{Opened, SCHEMA_VERSION};
pub use store::{Clock, SqliteStore};
