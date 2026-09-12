//! Turning `rusqlite` failures into the taxonomy in `spec/20-store.md` §7.
//!
//! The distinction that matters is [`CacheError::Corrupt`]: it is the one
//! error for which [`omaghy_model::StoreError::keeps_cached_content`] returns
//! `false`, so the UI must throw away what it is holding rather than render it
//! under a banner. Everything else is a failed query over a cache that is
//! still trustworthy.

use omaghy_model::CacheError;
use rusqlite::ErrorCode;

/// Whether SQLite is telling us the file is not a usable database.
///
/// `NotADatabase` is what a file of arbitrary bytes produces; `DatabaseCorrupt`
/// is what a truncated or scribbled-on real database produces. Both mean the
/// same thing to us — the bytes are not a cache.
pub(crate) fn is_corruption(err: &rusqlite::Error) -> bool {
    match err {
        rusqlite::Error::SqliteFailure(e, _) => {
            matches!(e.code, ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt)
        }
        _ => false,
    }
}

/// Map a query failure, preserving the corrupt/not-corrupt distinction.
///
/// `what` names the operation rather than echoing the SQL: the message reaches
/// a footer (`spec/30-ui.md` §8), and a user cannot act on a SELECT.
pub(crate) fn query(what: &str, err: rusqlite::Error) -> CacheError {
    if is_corruption(&err) {
        CacheError::Corrupt(format!("{what}: {err}"))
    } else {
        CacheError::Query(format!("{what}: {err}"))
    }
}

/// Map a failure while opening or preparing the database.
pub(crate) fn open(what: &str, err: rusqlite::Error) -> CacheError {
    if is_corruption(&err) {
        CacheError::Corrupt(format!("{what}: {err}"))
    } else {
        CacheError::Open(format!("{what}: {err}"))
    }
}

/// A stored body that will not deserialize.
///
/// This is corruption, not a query failure: the row is bytes we wrote and can
/// no longer read, and the honest response is to rebuild rather than to skip
/// the row and quietly render a shorter list. In practice the schema version
/// (`spec/20-store.md` §3.2) prevents it — a model change bumps the version and
/// the database is rebuilt before anything tries to read the old shape.
pub(crate) fn decode(what: &str, err: serde_json::Error) -> CacheError {
    CacheError::Corrupt(format!("{what}: stored body does not deserialize: {err}"))
}

/// A value that will not serialize. Never expected; reported rather than
/// swallowed, because silently not writing to a cache is indistinguishable
/// from a cache that is simply always cold.
pub(crate) fn encode(what: &str, err: serde_json::Error) -> CacheError {
    CacheError::Query(format!("{what}: value does not serialize: {err}"))
}
