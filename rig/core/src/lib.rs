//! The rig primitives the appliance and the JPEG XS link share (issue 1487).
//!
//! `camera-box` re-exports each module at its old path (`camera_box::genlock_grid`, ...), and
//! `camera-box-jxs` depends on this crate instead of on the appliance, so the static library the
//! OBS build links carries none of the appliance. Each module exists once: `genlock_grid` is the
//! Rust authority of `obs-genlock-grid.h`, pinned by `tests/genlock_grid_parity_1355.rs`.

#[cfg(target_os = "linux")]
pub mod affinity;
pub mod camera_number;
pub mod genlock_grid;
pub mod irq_offload;
