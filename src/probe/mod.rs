//! Frame-loss & latency E2E probe (Phase 1).
//!
//! Pure, unit-tested logic: `payload`, `luma`, `qr`, `analyzer`.
//! Hardware glue (excluded from coverage): `painter`, `reader`, `run`.

pub mod analyzer;
pub mod luma;
pub mod payload;
pub mod qr;

pub mod painter;
pub mod reader;
pub mod run;
