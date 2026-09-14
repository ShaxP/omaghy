//! The SQLite layer: entities, list membership, notifications, validators, kv.
//!
//! Every method here is synchronous and takes `&self`. Nothing in this file
//! knows about the network, freshness policy, or events — that is
//! [`crate::store::SqliteStore`]'s job. Keeping the split means the storage
//! can be exercised without a runtime, and the `Store` implementation reads as
//! policy rather than SQL.
//!
//! **Every statement filters on `viewer`.** `spec/20-store.md` §3.1: the
//! questions this cache answers — `i_am_requested`, `my_review`, `unread` —
//! are all "does this need *me*", and a row leaking between accounts is a
//! wrong answer rather than a missing one.

use crate::error;
use omaghy_model::{CacheError, Enrichment, NodeId, Notification, NotificationId};
use omaghy_store::query::{NotificationQuery, ReadFilter};
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, params_from_iter};
use serde::{Serialize, de::DeserializeOwned};
use std::path::{Path, PathBuf};
use time::{Duration, OffsetDateTime};

/// The `list_key` under which the notification inbox's freshness and
/// `Last-Modified` validator live.
///
/// Notifications have their own table (they are a REST id space), but they
/// still need the *list* metadata every other collection has. Putting it in
/// `list_meta` rather than inventing a second home is why `list_meta` gained
/// a `last_modified` column.
pub const NOTIFICATIONS_LIST: &str = "notifications";

/// `kv` key: GitHub's `X-Poll-Interval` for notifications, in seconds.
pub const KV_POLL_INTERVAL: &str = "notifications.poll_interval_secs";

/// The `list_key` for one dashboard section.
///
/// Derived from the section's GitHub search query rather than its title, so
/// renaming a section in config does not throw away its rows.
pub fn dashboard_list_key(query: &str) -> String {
    format!("dashboard:{query}")
}

/// What kind of thing a row in `entities` is. Stored so a future sweep can
/// expire by kind without deserializing every body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntityKind {
    PullRequest,
    Issue,
    Repo,
    CheckRuns,
    /// Anything not yet worth a variant. Carries a literal so the column stays
    /// readable in `sqlite3`.
    Other(&'static str),
}

impl EntityKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PullRequest => "pr",
            Self::Issue => "issue",
            Self::Repo => "repo",
            Self::CheckRuns => "checks",
            Self::Other(s) => s,
        }
    }
}

/// Conditional-request validators, stored beside the data they validate.
///
/// The type is `omaghy-model`'s: `omaghy-api` harvests them from a response
/// and this crate persists them, so it belongs to the vocabulary both speak
/// rather than to either one. It is re-exported here because every caller of
/// [`Cache`] needs it, and `spec/20-store.md` §4 is where it is specified.
pub use omaghy_model::Validators;

/// What we know about a stored list as a whole, as opposed to its members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListMeta {
    pub validators: Validators,
    /// Opaque GraphQL continuation token for the next page.
    pub cursor: Option<String>,
    /// Whether every page was fetched. A partial list is still shown; it just
    /// cannot claim a total.
    pub complete: bool,
    pub fetched_at: OffsetDateTime,
}

impl ListMeta {
    /// A list fetched in full, with no validator.
    pub fn complete_at(fetched_at: OffsetDateTime) -> Self {
        Self {
            validators: Validators::default(),
            cursor: None,
            complete: true,
            fetched_at,
        }
    }
}

/// A stored entity and the provenance it was stored with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored<T> {
    pub value: T,
    pub validators: Validators,
    pub fetched_at: OffsetDateTime,
}

/// The cache. One per viewer; the viewer is fixed at construction so no query
/// can forget to filter on it.
pub struct Cache {
    conn: Connection,
    viewer: String,
    path: Option<PathBuf>,
}

impl std::fmt::Debug for Cache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cache")
            .field("viewer", &self.viewer)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

type Result<T> = std::result::Result<T, CacheError>;

impl Cache {
    /// Open the cache at `path`, creating or rebuilding the schema as needed.
    ///
    /// Returns [`CacheError::Corrupt`] if the file is not a database. The
    /// database is rebuilt before the error is returned, so a retry succeeds —
    /// but the error still propagates, because the caller may be holding rows
    /// that came from the file that just went away.
    pub fn open(path: impl AsRef<Path>, viewer: &str) -> Result<Self> {
        let path = path.as_ref();
        let (conn, opened) = crate::schema::open(path)?;
        tracing::debug!(?path, ?opened, viewer, "cache opened");
        Ok(Self {
            conn,
            viewer: viewer.to_owned(),
            path: Some(path.to_owned()),
        })
    }

    /// An in-memory cache. Used by tests, and by `omaghy doctor`-style code
    /// that wants the schema without touching the user's file.
    pub fn in_memory(viewer: &str) -> Result<Self> {
        Ok(Self {
            conn: crate::schema::open_in_memory()?,
            viewer: viewer.to_owned(),
            path: None,
        })
    }

    pub fn viewer(&self) -> &str {
        &self.viewer
    }

    /// Where this cache lives, or `None` if it is in memory.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    // ---------------------------------------------------------- entities

    /// Upsert one entity. The body is a serialized `omaghy-model` type, never
    /// raw API JSON — translation happens once, at the `omaghy-api` boundary
    /// (`spec/20-store.md` §3).
    pub fn put_entity<T: Serialize>(
        &self,
        kind: EntityKind,
        node_id: &NodeId,
        value: &T,
        validators: &Validators,
        fetched_at: OffsetDateTime,
    ) -> Result<()> {
        let body = serde_json::to_vec(value).map_err(|e| error::encode("put_entity", e))?;
        self.conn
            .execute(
                "INSERT INTO entities (node_id, viewer, kind, body, etag, last_modified, fetched_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT (node_id, viewer) DO UPDATE SET
                   kind = excluded.kind, body = excluded.body, etag = excluded.etag,
                   last_modified = excluded.last_modified, fetched_at = excluded.fetched_at",
                rusqlite::params![
                    node_id.0,
                    self.viewer,
                    kind.as_str(),
                    body,
                    validators.etag,
                    validators.last_modified,
                    fetched_at.unix_timestamp(),
                ],
            )
            .map_err(|e| error::query("put_entity", e))?;
        Ok(())
    }

    /// Read one entity back. `None` means we have never stored it *for this
    /// viewer* — which is not the same as it not existing.
    pub fn entity<T: DeserializeOwned>(&self, node_id: &NodeId) -> Result<Option<Stored<T>>> {
        let row = self
            .conn
            .query_row(
                "SELECT body, etag, last_modified, fetched_at FROM entities
                 WHERE node_id = ?1 AND viewer = ?2",
                rusqlite::params![node_id.0, self.viewer],
                |r| {
                    Ok((
                        r.get::<_, Vec<u8>>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| error::query("entity", e))?;

        let Some((body, etag, last_modified, fetched_at)) = row else {
            return Ok(None);
        };
        Ok(Some(Stored {
            value: serde_json::from_slice(&body).map_err(|e| error::decode("entity", e))?,
            validators: Validators {
                etag,
                last_modified,
            },
            fetched_at: timestamp(fetched_at)?,
        }))
    }

    /// Drop one entity. Used when GitHub reports a subject as gone.
    pub fn forget_entity(&self, node_id: &NodeId) -> Result<bool> {
        let n = self
            .conn
            .execute(
                "DELETE FROM entities WHERE node_id = ?1 AND viewer = ?2",
                rusqlite::params![node_id.0, self.viewer],
            )
            .map_err(|e| error::query("forget_entity", e))?;
        Ok(n > 0)
    }

    // ------------------------------------------------------------- lists

    /// Replace a list's membership and metadata in one transaction.
    ///
    /// Membership lives apart from the entities themselves (§3), so a single
    /// PR updating does not invalidate every list it appears in — and, the
    /// other way round, re-fetching a list does not re-write bodies that did
    /// not change.
    pub fn put_list(&self, list_key: &str, ids: &[NodeId], meta: &ListMeta) -> Result<()> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| error::query("put_list", e))?;

        tx.execute(
            "DELETE FROM list_items WHERE list_key = ?1 AND viewer = ?2",
            rusqlite::params![list_key, self.viewer],
        )
        .map_err(|e| error::query("put_list: clearing", e))?;

        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO list_items (list_key, viewer, position, node_id)
                     VALUES (?1, ?2, ?3, ?4)",
                )
                .map_err(|e| error::query("put_list: preparing", e))?;
            for (position, id) in ids.iter().enumerate() {
                stmt.execute(rusqlite::params![
                    list_key,
                    self.viewer,
                    position as i64,
                    id.0
                ])
                .map_err(|e| error::query("put_list: inserting", e))?;
            }
        }

        tx.execute(
            "INSERT INTO list_meta (list_key, viewer, etag, last_modified, cursor, complete, fetched_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (list_key, viewer) DO UPDATE SET
               etag = excluded.etag, last_modified = excluded.last_modified,
               cursor = excluded.cursor, complete = excluded.complete,
               fetched_at = excluded.fetched_at",
            rusqlite::params![
                list_key,
                self.viewer,
                meta.validators.etag,
                meta.validators.last_modified,
                meta.cursor,
                meta.complete as i64,
                meta.fetched_at.unix_timestamp(),
            ],
        )
        .map_err(|e| error::query("put_list: meta", e))?;

        tx.commit().map_err(|e| error::query("put_list: commit", e))
    }

    /// A list's members, in stored order.
    pub fn list(&self, list_key: &str) -> Result<Vec<NodeId>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT node_id FROM list_items
                 WHERE list_key = ?1 AND viewer = ?2 ORDER BY position",
            )
            .map_err(|e| error::query("list", e))?;
        let rows = stmt
            .query_map(rusqlite::params![list_key, self.viewer], |r| {
                Ok(NodeId(r.get::<_, String>(0)?))
            })
            .map_err(|e| error::query("list", e))?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(|e| error::query("list", e))
    }

    /// Freshness and validators for a list. `None` = never fetched.
    pub fn list_meta(&self, list_key: &str) -> Result<Option<ListMeta>> {
        let row = self
            .conn
            .query_row(
                "SELECT etag, last_modified, cursor, complete, fetched_at FROM list_meta
                 WHERE list_key = ?1 AND viewer = ?2",
                rusqlite::params![list_key, self.viewer],
                |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| error::query("list_meta", e))?;

        let Some((etag, last_modified, cursor, complete, fetched_at)) = row else {
            return Ok(None);
        };
        Ok(Some(ListMeta {
            validators: Validators {
                etag,
                last_modified,
            },
            cursor,
            complete: complete != 0,
            fetched_at: timestamp(fetched_at)?,
        }))
    }

    /// Record that a list was fetched without changing its membership — the
    /// 304 case, which is the whole point of storing validators.
    pub fn touch_list(&self, list_key: &str, fetched_at: OffsetDateTime) -> Result<()> {
        self.conn
            .execute(
                "UPDATE list_meta SET fetched_at = ?3 WHERE list_key = ?1 AND viewer = ?2",
                rusqlite::params![list_key, self.viewer, fetched_at.unix_timestamp()],
            )
            .map_err(|e| error::query("touch_list", e))?;
        Ok(())
    }

    // ----------------------------------------------------- notifications

    /// Upsert a page of notifications.
    ///
    /// `unread`, `updated_at` and `enrichment` are denormalized out of the
    /// body so the inbox can be filtered and sorted without deserializing
    /// every row, and so "which rows still want enriching" is one query.
    pub fn put_notifications(&self, notifications: &[Notification]) -> Result<()> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| error::query("put_notifications", e))?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO notifications (id, viewer, body, unread, updated_at, enrichment)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT (id, viewer) DO UPDATE SET
                       body = excluded.body, unread = excluded.unread,
                       updated_at = excluded.updated_at, enrichment = excluded.enrichment",
                )
                .map_err(|e| error::query("put_notifications: preparing", e))?;
            for n in notifications {
                let body =
                    serde_json::to_vec(n).map_err(|e| error::encode("put_notifications", e))?;
                stmt.execute(rusqlite::params![
                    n.id.0,
                    self.viewer,
                    body,
                    n.unread as i64,
                    n.updated_at.unix_timestamp(),
                    enrichment_tag(&n.detail),
                ])
                .map_err(|e| error::query("put_notifications: inserting", e))?;
            }
        }
        tx.commit()
            .map_err(|e| error::query("put_notifications: commit", e))
    }

    /// The inbox, newest first, narrowed by `q`.
    ///
    /// `unread` and the ordering are pushed into SQL because they are columns.
    /// Reason, kind, repo and free text are matched in Rust: the specified
    /// table does not carry them as columns, and an inbox is hundreds of rows,
    /// not millions. Adding columns for them would be a schema change bought
    /// with a cost nobody has measured.
    pub fn notifications(&self, q: &NotificationQuery) -> Result<Vec<Notification>> {
        let sql = if q.read == ReadFilter::UnreadOnly {
            "SELECT body, unread FROM notifications
             WHERE viewer = ?1 AND unread = 1 ORDER BY updated_at DESC, id DESC"
        } else {
            "SELECT body, unread FROM notifications
             WHERE viewer = ?1 ORDER BY updated_at DESC, id DESC"
        };
        let mut stmt = self
            .conn
            .prepare(sql)
            .map_err(|e| error::query("notifications", e))?;
        let rows = stmt
            .query_map(rusqlite::params![self.viewer], |r| {
                Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?))
            })
            .map_err(|e| error::query("notifications", e))?;

        let mut out = Vec::new();
        for row in rows {
            let (body, unread) = row.map_err(|e| error::query("notifications", e))?;
            let mut n: Notification =
                serde_json::from_slice(&body).map_err(|e| error::decode("notifications", e))?;
            // The column is authoritative for read state: `mark_read` writes
            // it, and a body serialized before that write would otherwise
            // resurrect the old value. Reconciling here means the two cannot
            // disagree on screen.
            n.unread = unread != 0;
            if matches(&n, q) {
                out.push(n);
            }
        }
        Ok(out)
    }

    /// Read state for specific ids, as it is *now*.
    ///
    /// This is the prior value a mutation captures before writing
    /// (`spec/20-store.md` §5) — rollback is only possible if it was taken
    /// first. Ids with no row are absent from the result rather than assumed
    /// read.
    pub fn unread_flags(&self, ids: &[NotificationId]) -> Result<Vec<(NotificationId, bool)>> {
        let mut out = Vec::new();
        for chunk in ids.chunks(CHUNK) {
            let sql = format!(
                "SELECT id, unread FROM notifications WHERE viewer = ?1 AND id IN ({})",
                placeholders(chunk.len(), 2)
            );
            let mut stmt = self
                .conn
                .prepare(&sql)
                .map_err(|e| error::query("unread_flags", e))?;
            let params: Vec<Value> = std::iter::once(Value::Text(self.viewer.clone()))
                .chain(chunk.iter().map(|i| Value::Text(i.0.clone())))
                .collect();
            let rows = stmt
                .query_map(params_from_iter(params), |r| {
                    Ok((
                        NotificationId(r.get::<_, String>(0)?),
                        r.get::<_, i64>(1)? != 0,
                    ))
                })
                .map_err(|e| error::query("unread_flags", e))?;
            for row in rows {
                out.push(row.map_err(|e| error::query("unread_flags", e))?);
            }
        }
        Ok(out)
    }

    /// Set read state for a batch. Returns how many rows actually changed.
    ///
    /// Marking an already-read thread changes nothing and is not an error —
    /// the trait requires `mark_read` to be idempotent.
    pub fn set_unread(&self, ids: &[NotificationId], unread: bool) -> Result<usize> {
        let mut changed = 0;
        for chunk in ids.chunks(CHUNK) {
            let sql = format!(
                "UPDATE notifications SET unread = ?2 WHERE viewer = ?1 AND id IN ({})",
                placeholders(chunk.len(), 3)
            );
            let params: Vec<Value> = [
                Value::Text(self.viewer.clone()),
                Value::Integer(unread.into()),
            ]
            .into_iter()
            .chain(chunk.iter().map(|i| Value::Text(i.0.clone())))
            .collect();
            changed += self
                .conn
                .execute(&sql, params_from_iter(params))
                .map_err(|e| error::query("set_unread", e))?;
        }
        Ok(changed)
    }

    /// Put read state back exactly as it was. The rollback half of §5.
    pub fn restore_unread(&self, prior: &[(NotificationId, bool)]) -> Result<()> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| error::query("restore_unread", e))?;
        {
            let mut stmt = tx
                .prepare("UPDATE notifications SET unread = ?3 WHERE viewer = ?1 AND id = ?2")
                .map_err(|e| error::query("restore_unread", e))?;
            for (id, unread) in prior {
                stmt.execute(rusqlite::params![self.viewer, id.0, *unread as i64])
                    .map_err(|e| error::query("restore_unread", e))?;
            }
        }
        tx.commit()
            .map_err(|e| error::query("restore_unread: commit", e))
    }

    /// Ids whose second-pass enrichment has never been requested.
    ///
    /// `Failed` is terminal and `Pending` is in flight, so neither appears —
    /// retrying either on every open is how a repo you lost access to burns
    /// the rate limit.
    pub fn notifications_wanting_enrichment(&self, limit: usize) -> Result<Vec<NotificationId>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id FROM notifications
                 WHERE viewer = ?1 AND enrichment = 'absent'
                 ORDER BY updated_at DESC LIMIT ?2",
            )
            .map_err(|e| error::query("notifications_wanting_enrichment", e))?;
        let rows = stmt
            .query_map(rusqlite::params![self.viewer, limit as i64], |r| {
                Ok(NotificationId(r.get::<_, String>(0)?))
            })
            .map_err(|e| error::query("notifications_wanting_enrichment", e))?;
        rows.collect::<std::result::Result<_, _>>()
            .map_err(|e| error::query("notifications_wanting_enrichment", e))
    }

    // ---------------------------------------------------------------- kv

    /// `kv` is viewer-keyed like everything else: rate-limit budget and poll
    /// interval belong to a token, not to a machine (`spec/20-store.md` §3.1).
    pub fn kv_put(&self, k: &str, v: &[u8]) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO kv (k, viewer, v) VALUES (?1, ?2, ?3)
                 ON CONFLICT (k, viewer) DO UPDATE SET v = excluded.v",
                rusqlite::params![k, self.viewer, v],
            )
            .map_err(|e| error::query("kv_put", e))?;
        Ok(())
    }

    pub fn kv_get(&self, k: &str) -> Result<Option<Vec<u8>>> {
        self.conn
            .query_row(
                "SELECT v FROM kv WHERE k = ?1 AND viewer = ?2",
                rusqlite::params![k, self.viewer],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(|e| error::query("kv_get", e))
    }

    pub fn kv_put_json<T: Serialize>(&self, k: &str, v: &T) -> Result<()> {
        let bytes = serde_json::to_vec(v).map_err(|e| error::encode("kv_put_json", e))?;
        self.kv_put(k, &bytes)
    }

    pub fn kv_get_json<T: DeserializeOwned>(&self, k: &str) -> Result<Option<T>> {
        match self.kv_get(k)? {
            None => Ok(None),
            Some(bytes) => Ok(Some(
                serde_json::from_slice(&bytes).map_err(|e| error::decode("kv_get_json", e))?,
            )),
        }
    }

    /// GitHub's advertised notification poll interval, if we have seen one.
    pub fn poll_interval(&self) -> Result<Option<Duration>> {
        Ok(self
            .kv_get_json::<i64>(KV_POLL_INTERVAL)?
            .map(Duration::seconds))
    }

    pub fn set_poll_interval(&self, d: Duration) -> Result<()> {
        self.kv_put_json(KV_POLL_INTERVAL, &d.whole_seconds())
    }
}

/// SQLite's bind parameters are one-based and the viewer always takes the
/// first slots, so a generated `IN` list has to start counting after them.
fn placeholders(n: usize, first: usize) -> String {
    (first..first + n)
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Well under SQLite's default 32766-parameter ceiling, and small enough that
/// a bulk mark-read is several statements rather than one enormous one.
const CHUNK: usize = 500;

fn timestamp(secs: i64) -> Result<OffsetDateTime> {
    OffsetDateTime::from_unix_timestamp(secs)
        .map_err(|e| CacheError::Corrupt(format!("stored timestamp {secs} is not a time: {e}")))
}

fn enrichment_tag<T>(e: &Enrichment<T>) -> &'static str {
    match e {
        Enrichment::Absent => "absent",
        Enrichment::Pending => "pending",
        Enrichment::Failed { .. } => "failed",
        Enrichment::NotApplicable => "not-applicable",
        Enrichment::Ready(_) => "ready",
    }
}

/// The predicate the inbox is narrowed by, for the parts SQL cannot see.
fn matches(n: &Notification, q: &NotificationQuery) -> bool {
    if q.read == ReadFilter::UnreadOnly && !n.unread {
        return false;
    }
    if let Some(repo) = &q.repo
        && &n.repo != repo
    {
        return false;
    }
    if !q.reasons.is_empty() && !q.reasons.contains(&n.reason) {
        return false;
    }
    if !q.kinds.is_empty() && !q.kinds.contains(&n.kind) {
        return false;
    }
    if let Some(s) = &q.search {
        let s = s.to_lowercase();
        let hay = format!(
            "{} {}",
            n.title.to_lowercase(),
            n.repo.to_string().to_lowercase()
        );
        if !hay.contains(&s) {
            return false;
        }
    }
    true
}
