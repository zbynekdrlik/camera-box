//! VBAN protocol implementation — re-export of the shared `intercom-vban` crate (issue 1345 M1).
//!
//! The VBAN codec (header parse/encode, `VbanCodec`/`VbanHeader`, the sample-rate index table and
//! the port/size constants) moved into the standalone `intercom/vban` workspace crate so the
//! strih-lx intercom hub (`intercom/hub`) and this appliance speak ONE byte-identical wire format.
//! This module is now a thin re-export, so `src/intercom.rs` (and every existing caller/test) keeps
//! importing `crate::vban::{...}` UNCHANGED and the cambox contract stays byte-for-byte the same.
//!
//! The unit tests moved with the codec (`intercom/vban/src/lib.rs`) and run in the `intercom-hub`
//! CI job; `pub mod vban;` in `src/lib.rs` is deliberately preserved (cross-platform, un-gated —
//! `tests/probe_windows_buildable.rs` anchors on it).

pub use intercom_vban::*;
