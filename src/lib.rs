//! Library crate exposing Alfred Writer's modules so both the `alfred-writer` binary and
//! `tests/*.rs` integration tests can link against the same code. See ARCHITECTURE.md for
//! how the pieces fit together.

pub mod app;
pub mod automation;
pub mod config;
pub mod input;
pub mod providers;
pub mod targets;
pub mod telemetry;
pub mod theme;
pub mod tray;
