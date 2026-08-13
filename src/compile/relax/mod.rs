//! Continuous placement: springs pull, the spacing rule pushes back, and what
//! comes out is rounded onto the lattice.
//!
//! See `docs/superpowers/specs/2026-08-13-spring-placement.md`.

mod build;
mod linear;
mod project;

// Re-exported rather than kept private: nothing outside `#[cfg(test)]` calls
// the model or the solver until Tasks 7 and 8, and a `pub` item in a private
// module that nobody reaches is `dead_code` -- an error under `check.sh`'s
// `cargo clippy --all-targets -- -D warnings`.
pub use build::{
    attach_offset, build, cells, pin_hops, Attach, Body, BodyGraph, BodyKind, Cell, Pull, Weld,
    SIGNAL_STIFFNESS,
};
pub use linear::{Factorisation, NotPositiveDefinite};
pub use project::{
    placed_cells, project, required_separations, reservation, worst_violation, Axes, PlacedCell,
    Violation, CONDUCTOR_CLEARANCE, PROJECTION_ROUNDS, ROUTE_PITCH, SETTLED, SNAP_MARGIN,
};
