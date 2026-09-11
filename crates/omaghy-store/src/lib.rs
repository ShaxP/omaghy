//! The `Store` trait — the only seam `omaghy-tui` sees.
//!
//! This crate exists so the trait has a home that is neither the vocabulary
//! nor an implementation. `omaghy-model` must stay a leaf (it would otherwise
//! need `async-trait` and `tokio`), and putting the trait in `omaghy-cache`
//! would make the UI depend on a storage backend.
//!
//! The trait, `FakeStore`, and the fixtures land in P0.3; this crate is
//! created in P0.2 so the dependency graph is settled before any fan-out.
//!
//! See `spec/20-store.md`.
