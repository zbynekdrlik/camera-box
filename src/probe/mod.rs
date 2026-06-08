//! Frame-loss & latency E2E probe (Phase 1).
//!
//! Pure, unit-tested logic: `payload`, `luma`, `qr`, `analyzer`, `differ`.
//! Hardware glue (excluded from coverage): `fb`, `painter`, `reader`, `run`.

pub mod analyzer;
pub mod differ;
pub mod luma;
pub mod payload;
pub mod qr;

pub mod fb;
pub mod painter;
pub mod reader;
pub mod run;
