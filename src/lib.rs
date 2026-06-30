//! camera-box library
//!
//! This module exports the public APIs for testing and benchmarking.

// #193: the camera APPLIANCE modules are Linux-only — they bind v4l (capture/config/ndi),
// the /dev/fb0 framebuffer + libc ioctl (display/ndi_display), and ALSA/cpal/evdev
// (intercom). The cameras are x86_64 Ubuntu, so these only ever build on Linux. Gating
// them on cfg(target_os="linux") lets the PROBE tooling (recording-verdict, src/bin) build
// for Windows, so the #193 recording decode runs ON stream.lan where the video lives — never
// downloaded to dev1. grab_record calls crate::capture::yuyv_to_gray8, so it is gated in
// lock-step. vban (pure UDP) and the probe tree (which the verdict needs) stay cross-platform.
#[cfg(target_os = "linux")]
pub mod affinity;
#[cfg(target_os = "linux")]
pub mod capture;
#[cfg(target_os = "linux")]
pub mod config;
#[cfg(target_os = "linux")]
pub mod display;
#[cfg(target_os = "linux")]
pub mod grab_record;
#[cfg(target_os = "linux")]
pub mod intercom;
#[cfg(target_os = "linux")]
pub mod ndi;
#[cfg(target_os = "linux")]
pub mod ndi_display;
pub mod vban;

// #297 — NDI sender re-announce trigger (pure decision + network signature). Cross-platform
// (no v4l/libc) so it unit-tests Tier-0; the Linux-only IO (interface read + sender re-create)
// lives in `ndi`.
pub mod reannounce;

// #367 — colour-scale reference layout (pure geometry + colour table). Cross-platform, no
// probe deps, so it unit-tests Tier-0; the probe-gated framebuffer blit lives in `probe::qr`.
pub mod colour_scale;

// #364 — per-camera COLOUR-correctness gate (pure decision + sampler). Iterates the SAME
// `colour_scale` table/geometry, samples each reference patch's mean colour from a frame
// (dodging the burn columns), and decides per-patch + per-camera PASS/FAIL (grayscale collapse,
// hue-shift, out-of-tolerance). No probe deps, so it unit-tests Tier-0; the probe-gated pixel
// sampling + ffmpeg colour pass live in `probe::colour_sample`, and the verdict gate wiring is in
// `bin/recording-verdict`.
pub mod colour_verify;

#[cfg(feature = "probe")]
pub mod probe;
