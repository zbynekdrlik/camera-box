//! VBAN protocol implementation — re-export of the `intercom-vban` crate of the intercom project,
//! pinned by git tag in the root Cargo.toml (issue 1487).
//!
//! The intercom hub and this appliance speak ONE byte-identical wire format. `src/intercom.rs` (and
//! every existing caller/test) keeps importing `crate::vban::{...}`, and the cambox contract stays
//! byte-for-byte the same. The codec's unit tests run in the intercom repo; `pub mod vban;` in
//! `src/lib.rs` is deliberately preserved (cross-platform, un-gated —
//! `tests/probe_windows_buildable.rs` anchors on it).

pub use intercom_vban::*;
