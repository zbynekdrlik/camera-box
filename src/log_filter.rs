//! The camera-box binary's log filter. `tracing` targets are module paths, so every crate that
//! logs for this binary is named here: a crate left out is filtered out silently, WARNs included
//! (issue 1487, `rig_core::affinity` carries the pin and realtime failure lines).

/// The `EnvFilter` directives of the camera-box binary.
pub fn directives(debug: bool) -> &'static str {
    if debug {
        "camera_box=debug,grafton_ndi=debug"
    } else {
        "camera_box=info"
    }
}
