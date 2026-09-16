//! The ratatui frontend: router, surfaces, widgets, keymap, theme.
//!
//! This crate performs **no I/O**. It consumes the `Store` trait; if a surface
//! needs data, that is a `Store` change, not an HTTP call. The boundary is
//! enforced by the dependency graph, not by discipline.
//!
//! See `spec/30-ui.md`.

pub mod app;
pub mod config;
pub mod keys;
pub mod open;
pub mod route;
pub mod surface;
pub mod surfaces;
pub mod terminal;
pub mod theme;
pub mod widgets;

pub use app::App;
pub use route::{Route, SurfaceId};
pub use surface::{Ctx, Outcome, Surface};
