//! The JPEG XS link (issue 1387): a crate of its own since issue 1487, so `jxs-recv-ffi` and
//! `jxs-send-ffi` carry none of the camera-box appliance. `camera-box` re-exports every module at
//! its old path (`camera_box::jxs_link`, ...). The rules: `.claude/rules/jxs-link.md`.

// The shared rig modules at the paths this crate's code names them by.
#[cfg(target_os = "linux")]
pub(crate) use rig_core::affinity;
pub(crate) use rig_core::{camera_number, genlock_grid};

// The wire format + reassembly + pixel helpers (pure, every platform).
pub mod jxs_link;
// The sender's automatic bpp controller (pure state machine, every platform).
pub mod jxs_auto_bpp;
// Issue 1387 item 2: the sender's delay-based congestion control (pure) and its bottleneck bench.
pub mod jxs_congestion;
#[cfg(test)]
mod jxs_congestion_bench;
// The pure helpers of the jxs-encode-bench bin (synthetic content, percentiles).
pub mod jxs_bench;
// The cambox JPEG XS sender (off unless CAMERA_BOX_JXS_TARGET is set) and the receive side the
// jxs-recv-probe bin and the loopback tests use. The codec itself needs the `jxs` feature.
#[cfg(target_os = "linux")]
pub mod jxs_recv;
#[cfg(target_os = "linux")]
pub mod jxs_send;
