//! Provenance. Every read carries where it came from and how old it is, so
//! the UI can say "cached, 3 minutes old, refreshing" rather than lying.
//!
//! See `spec/20-store.md` §1.

use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Cache,
    Network,
}

/// A value plus everything the UI needs to be honest about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fresh<T> {
    pub value: T,
    /// `None` means never fetched — distinct from fetched-and-empty.
    pub fetched_at: Option<OffsetDateTime>,
    pub source: Source,
    pub stale: bool,
    pub refreshing: bool,
}

impl<T> Fresh<T> {
    /// A value that has never been fetched. The cold-start case.
    pub fn never(value: T) -> Self {
        Self {
            value,
            fetched_at: None,
            source: Source::Cache,
            stale: true,
            refreshing: false,
        }
    }

    pub fn from_cache(
        value: T,
        fetched_at: OffsetDateTime,
        ttl: Duration,
        now: OffsetDateTime,
    ) -> Self {
        Self {
            value,
            fetched_at: Some(fetched_at),
            source: Source::Cache,
            stale: now - fetched_at > ttl,
            refreshing: false,
        }
    }

    pub fn from_network(value: T, at: OffsetDateTime) -> Self {
        Self {
            value,
            fetched_at: Some(at),
            source: Source::Network,
            stale: false,
            refreshing: false,
        }
    }

    pub fn refreshing(mut self, yes: bool) -> Self {
        self.refreshing = yes;
        self
    }

    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Fresh<U> {
        Fresh {
            value: f(self.value),
            fetched_at: self.fetched_at,
            source: self.source,
            stale: self.stale,
            refreshing: self.refreshing,
        }
    }

    /// Whether the header should admit this is cached.
    ///
    /// Fresh network data needs no note; anything stale does, and so does
    /// anything from cache while a refresh is in flight.
    pub fn needs_provenance_note(&self) -> bool {
        self.stale || (self.source == Source::Cache && self.refreshing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    const NOW: OffsetDateTime = datetime!(2026-09-11 12:00 UTC);

    #[test]
    fn never_fetched_is_distinct_from_fetched_and_empty() {
        let cold: Fresh<Vec<u8>> = Fresh::never(vec![]);
        assert!(cold.fetched_at.is_none());
        assert!(cold.stale);

        let empty = Fresh::from_network(Vec::<u8>::new(), NOW);
        assert!(empty.fetched_at.is_some());
        assert!(!empty.stale);
    }

    #[test]
    fn staleness_follows_the_ttl() {
        let ttl = Duration::minutes(5);
        let recent = Fresh::from_cache(1, NOW - Duration::minutes(2), ttl, NOW);
        assert!(!recent.stale);
        let old = Fresh::from_cache(1, NOW - Duration::minutes(9), ttl, NOW);
        assert!(old.stale);
    }

    #[test]
    fn fresh_network_data_needs_no_provenance_note() {
        assert!(!Fresh::from_network(1, NOW).needs_provenance_note());
        assert!(
            Fresh::from_cache(1, NOW - Duration::hours(1), Duration::minutes(5), NOW)
                .needs_provenance_note()
        );
        // Cached but current, with a refresh in flight: still worth saying.
        assert!(
            Fresh::from_cache(1, NOW, Duration::minutes(5), NOW)
                .refreshing(true)
                .needs_provenance_note()
        );
    }

    #[test]
    fn map_preserves_provenance() {
        let f = Fresh::from_cache(
            vec![1, 2, 3],
            NOW - Duration::hours(1),
            Duration::minutes(5),
            NOW,
        )
        .refreshing(true);
        let mapped = f.clone().map(|v| v.len());
        assert_eq!(mapped.value, 3);
        assert_eq!(mapped.stale, f.stale);
        assert_eq!(mapped.refreshing, f.refreshing);
        assert_eq!(mapped.fetched_at, f.fetched_at);
    }
}
