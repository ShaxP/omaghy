//! The schema, and the two ways of losing it.
//!
//! `spec/20-store.md` §3.2: versioning is a `user_version` pragma and **on
//! mismatch the database is deleted and rebuilt**. This is a cache; a
//! migration would be effort spent protecting data we can re-fetch, and it
//! would have to be maintained for every model change in `omaghy-model`.
//!
//! Corruption is handled the same way mechanically but reported differently:
//! the database is rebuilt *and* [`omaghy_model::CacheError::Corrupt`] is
//! returned, because the caller is holding rows that came from the old file
//! and `StoreError::keeps_cached_content()` says those are the one thing that
//! must not stay on screen.

use crate::error;
use omaghy_model::CacheError;
use rusqlite::Connection;
use std::path::Path;

/// Bump this whenever the DDL below changes **or** a serialized
/// `omaghy-model` type changes shape. Bodies are serialized model types
/// (`spec/20-store.md` §3), so the model is part of the schema whether or not
/// the SQL moved.
pub const SCHEMA_VERSION: i64 = 1;

/// The tables from `spec/20-store.md` §3, with three corrections that §3 did
/// not survive contact with — see the crate docs and the spec's own §3 notes:
///
/// * `entities` and `list_meta` carry `last_modified` beside `etag`. §4 says
///   to store both validators beside the data; the DDL only had one.
/// * `kv` carries `viewer`. §3.1 says *every* table is viewer-keyed, and the
///   values `kv` was specified to hold — rate limit, poll interval — are
///   per-token, so a shared row makes one account's budget govern another's.
/// * `notifications` has an index on `(viewer, updated_at)`. Every read of
///   that table is "this viewer's inbox, newest first".
const DDL: &str = "
CREATE TABLE IF NOT EXISTS entities (
  node_id       TEXT    NOT NULL,
  viewer        TEXT    NOT NULL,
  kind          TEXT    NOT NULL,
  body          BLOB    NOT NULL,
  etag          TEXT,
  last_modified TEXT,
  fetched_at    INTEGER NOT NULL,
  PRIMARY KEY (node_id, viewer)
);

CREATE TABLE IF NOT EXISTS list_items (
  list_key TEXT    NOT NULL,
  viewer   TEXT    NOT NULL,
  position INTEGER NOT NULL,
  node_id  TEXT    NOT NULL,
  PRIMARY KEY (list_key, viewer, position)
);

CREATE TABLE IF NOT EXISTS list_meta (
  list_key      TEXT    NOT NULL,
  viewer        TEXT    NOT NULL,
  etag          TEXT,
  last_modified TEXT,
  cursor        TEXT,
  complete      INTEGER NOT NULL,
  fetched_at    INTEGER NOT NULL,
  PRIMARY KEY (list_key, viewer)
);

CREATE TABLE IF NOT EXISTS notifications (
  id         TEXT    NOT NULL,
  viewer     TEXT    NOT NULL,
  body       BLOB    NOT NULL,
  unread     INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  enrichment TEXT    NOT NULL,
  PRIMARY KEY (id, viewer)
);

CREATE INDEX IF NOT EXISTS notifications_by_viewer_updated
  ON notifications (viewer, updated_at DESC);

CREATE TABLE IF NOT EXISTS kv (
  k      TEXT NOT NULL,
  viewer TEXT NOT NULL,
  v      BLOB NOT NULL,
  PRIMARY KEY (k, viewer)
);
";

/// How an open resolved. Callers need to know, because a rebuild means every
/// row the UI is holding is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opened {
    /// The file was already at [`SCHEMA_VERSION`].
    Existing,
    /// There was no database, or it was at another version, and one was built.
    Rebuilt,
}

/// Open a file-backed cache, rebuilding it if the version does not match.
///
/// Returns [`CacheError::Corrupt`] — after rebuilding, so the next call
/// succeeds — when the file is not a database.
pub(crate) fn open(path: &Path) -> Result<(Connection, Opened), CacheError> {
    match try_open(path) {
        Ok(pair) => Ok(pair),
        Err(e) if matches!(e, CacheError::Corrupt(_)) => {
            // Rebuild now rather than on the next launch, so a corrupt file is
            // a single bad read and not a permanently broken install. The
            // error still propagates: the caller is holding rows from the file
            // we just deleted.
            tracing::warn!(?path, "cache is corrupt; deleting and rebuilding");
            remove(path);
            let _ = try_open(path)?;
            Err(e)
        }
        Err(e) => Err(e),
    }
}

fn try_open(path: &Path) -> Result<(Connection, Opened), CacheError> {
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir)
            .map_err(|e| CacheError::Open(format!("creating {}: {e}", dir.display())))?;
    }

    let conn = Connection::open(path).map_err(|e| error::open("opening the cache", e))?;
    prepare(&conn)?;

    let version = user_version(&conn)?;
    if version == SCHEMA_VERSION {
        // A file at the right version but with no tables is a database that
        // was created and never initialised; `CREATE TABLE IF NOT EXISTS`
        // makes that case free rather than a special one.
        apply(&conn)?;
        return Ok((conn, Opened::Existing));
    }

    if version == 0 && is_empty(&conn)? {
        apply(&conn)?;
        set_user_version(&conn)?;
        return Ok((conn, Opened::Rebuilt));
    }

    tracing::info!(
        found = version,
        want = SCHEMA_VERSION,
        "cache schema version mismatch; rebuilding"
    );
    drop(conn);
    remove(path);

    let conn = Connection::open(path).map_err(|e| error::open("reopening the cache", e))?;
    prepare(&conn)?;
    apply(&conn)?;
    set_user_version(&conn)?;
    Ok((conn, Opened::Rebuilt))
}

/// Build the schema in memory. Used by tests and by anything that wants a
/// cache that outlives nothing.
pub(crate) fn open_in_memory() -> Result<Connection, CacheError> {
    let conn =
        Connection::open_in_memory().map_err(|e| error::open("opening an in-memory cache", e))?;
    prepare(&conn)?;
    apply(&conn)?;
    set_user_version(&conn)?;
    Ok(conn)
}

/// WAL, so `omaghy watch` and the TUI can share the file without a protocol
/// (`spec/20-store.md` §3). This is also the first statement to touch the
/// file's header, so it is where a non-database is detected.
///
/// An in-memory connection silently reports `memory` instead; that is fine and
/// deliberately not treated as a failure.
fn prepare(conn: &Connection) -> Result<(), CacheError> {
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| error::open("enabling WAL", e))?;
    // NORMAL is the documented companion to WAL: durable against a process
    // crash, which is all a cache needs. FULL would fsync on every commit for
    // data we can re-fetch.
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|e| error::open("setting synchronous", e))?;
    // A second process holding the write lock is normal with `omaghy watch`
    // running; wait for it rather than returning SQLITE_BUSY to a draw path.
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| error::open("setting the busy timeout", e))?;
    Ok(())
}

fn apply(conn: &Connection) -> Result<(), CacheError> {
    conn.execute_batch(DDL)
        .map_err(|e| error::open("creating the schema", e))
}

fn user_version(conn: &Connection) -> Result<i64, CacheError> {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| error::open("reading the schema version", e))
}

fn set_user_version(conn: &Connection) -> Result<(), CacheError> {
    // PRAGMA values cannot be bound; SCHEMA_VERSION is a constant integer, so
    // formatting it is not an injection surface.
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(|e| error::open("writing the schema version", e))
}

fn is_empty(conn: &Connection) -> Result<bool, CacheError> {
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type = 'table' \
             AND name NOT LIKE 'sqlite_%'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| error::open("inspecting the schema", e))?;
    Ok(n == 0)
}

/// Delete the database and both WAL sidecars. Leaving `-wal` behind next to a
/// fresh file is how a "rebuilt" cache comes back with the old contents.
fn remove(path: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let mut p = path.as_os_str().to_owned();
        p.push(suffix);
        let _ = std::fs::remove_file(std::path::PathBuf::from(p));
    }
}
