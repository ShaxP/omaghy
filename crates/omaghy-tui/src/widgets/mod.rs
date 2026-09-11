//! Shared chrome. The real widget set lands in W1.3; this is the minimum the
//! shell needs to prove itself.

pub mod chrome;
pub mod state;

pub use chrome::{footer, header};
pub use state::empty_state;
