//! Continuous placement: springs pull, the spacing rule pushes back, and what
//! comes out is rounded onto the lattice.
//!
//! See `docs/superpowers/specs/2026-08-13-spring-placement.md`.

mod linear;

// Re-exported rather than kept private: nothing outside `#[cfg(test)]` calls
// the solver until Task 8, and a `pub` item in a private module that nobody
// reaches is `dead_code` -- an error under `check.sh`'s
// `cargo clippy --all-targets -- -D warnings`.
pub use linear::{Factorisation, NotPositiveDefinite};
