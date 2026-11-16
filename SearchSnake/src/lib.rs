//! A Battlesnake agent built around an iterative-deepening maximin search.
//!
//! The modules split along the lines you would want to test independently:
//!
//! - [`state`] — board representation and the official turn-resolution rules
//! - [`fill`] — time-aware flood fill and Voronoi area control
//! - [`eval`] — the static evaluation and its tunable weights
//! - [`search`] — iterative-deepening maximin with locality and alpha-beta
//! - [`api`] — Battlesnake HTTP payloads to and from [`state::State`]
//! - [`http`] — a minimal HTTP server
//! - [`arena`] — a self-play harness used to measure strength

pub mod api;
pub mod arena;
pub mod eval;
pub mod fill;
pub mod http;
pub mod json;
pub mod search;
pub mod state;
