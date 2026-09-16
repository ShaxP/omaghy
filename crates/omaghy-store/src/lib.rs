//! The `Store` trait — the only seam `omaghy-tui` sees.
//!
//! This crate exists so the trait has a home that is neither the vocabulary
//! nor an implementation. `omaghy-model` must stay a leaf (it would otherwise
//! need `async-trait` and `tokio`), and putting the trait in `omaghy-cache`
//! would make the UI depend on a storage backend.
//!
//! Two implementations ship, and the TUI cannot tell them apart:
//! `SqliteStore` (in `omaghy-cache`, M1 Wave 1) and [`fake::FakeStore`], which
//! is what surface agents build against and what snapshot tests run on.
//!
//! See `spec/20-store.md`.

pub mod event;
pub mod fake;
pub mod fresh;
pub mod query;
pub mod store;

pub use event::{RefreshTarget, StoreEvent};
pub use fake::FakeStore;
pub use fresh::{Fresh, Source};
pub use query::{DashboardConfig, DashboardSection, NotificationQuery, Page, PrQuery, ReadFilter};
pub use store::{Dashboard, DashboardSectionData, Store, Viewer};
