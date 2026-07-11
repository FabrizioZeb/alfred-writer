//! Library crate exposing Alfred Writer's modules so both the `alfred-writer` binary and
//! `tests/*.rs` integration tests can link against the same code. See ARCHITECTURE.md for
//! how the pieces fit together.

pub mod app;
pub mod automation;
pub mod claude;
pub mod config;
pub mod input;
pub mod targets;
pub mod tray;
