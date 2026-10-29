//! Issue 1387 -- the in-OBS JPEG XS indicator's decision (owner request 29.10.2026: "indikator ako
//! je genlock ... ze sa ide v jpeg-xs mode", design 6506884476).
//!
//! DistroAV's JPEG XS source (`camera_box_jxs_source`) and program output (`camera_box_jxs_output`)
//! publish one row each into a process-wide table (`vendor/distroav/src/jxs-state-table.h`); the
//! frontend reads it once a second through the libobs proc handler `camera_box_jxs_state` and
//! grades it here: each row OK / DEGRADED / DOWN ([`entry_health`]), then the box OFF / OK /
//! DEGRADED / DOWN ([`decide`]). No row (no JPEG XS source or running output, or a build without
//! them: the Windows bundle) = OFF, never an error.
//!
//! This is the authority. The C port is `vendor/obs-studio/frontend/widgets/GenlockJxsState.hpp`,
//! parity-gated by `tests/genlock_jxs_state_parity_1387.rs`. Std-only, crate root, no feature.

/// A row's last frame older than this (ms) = the link is DOWN (the source's own "not connected"
/// bound, `CB_JXS_CONNECTED_TIMEOUT_NS` in `jxs-source-decide.h`).
pub const JXS_DOWN_AGE_MS: i64 = 3000;
/// A window below this many frames per second x100 is DEGRADED. The JPEG XS link runs on the
/// 30 fps strih grid (owner decision 29.10.2026); 27 = 90 % of it.
pub const JXS_MIN_FPS_X100: u32 = 2700;
/// The program output with no receiver report for longer than this (ms) is DEGRADED: the auto bpp
/// holds and nobody confirms the frames arrive (the auto bpp's own 3 s hold bound).
pub const JXS_REPORT_MAX_AGE_MS: i64 = 3000;

/// Which side of the link a row is.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JxsKind {
    /// A `camera_box_jxs_source`: JPEG XS arriving into this OBS.
    Input = 0,
    /// The `camera_box_jxs_output`: this OBS's program leaving as JPEG XS.
    Output = 1,
}

/// One row's health.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JxsHealth {
    /// Frames flow, the last window was clean.
    Ok = 0,
    /// Frames flow, but the last window lost, dropped or ran short.
    Degraded = 1,
    /// No frame for [`JXS_DOWN_AGE_MS`] (or none yet).
    Down = 2,
}

/// The indicator's state. Discriminants match the C `genlock_jxs_state` enum.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JxsState {
    /// No JPEG XS on this box (grey).
    Off = 0,
    /// Every row OK (green).
    Ok = 1,
    /// Some row lost frames / is down while others carry the link (amber).
    Degraded = 2,
    /// The program output is down, or every input is (red).
    Down = 3,
}

/// The dominant reason. Discriminants match the C `genlock_jxs_reason` enum.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JxsReason {
    /// The state is OK.
    None = 0,
    /// No JPEG XS source or running output exists.
    NoJxs = 1,
    /// The program output sends no frame.
    OutputDown = 2,
    /// Every JPEG XS input receives nothing.
    AllInputsDown = 3,
    /// Some (not all) inputs receive nothing.
    InputDown = 4,
    /// The program output dropped, failed or hears no receiver.
    OutputDegraded = 5,
    /// An input lost slices / frames or ran short.
    InputDegraded = 6,
}

/// One row of the table, scalarised (the JSON row the proc handler returns).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JxsEntryFacets {
    /// Input or output.
    pub kind: JxsKind,
    /// ms since the row's last frame; negative = no frame yet.
    pub age_ms: i64,
    /// Whether a 5 s status window has been published (fps and the counters below are its).
    pub window: bool,
    /// The window's frames per second x100.
    pub fps_x100: u32,
    /// Lost slices in the window (input).
    pub lost: u64,
    /// Undecodable frames in the window (input).
    pub undecodable: u64,
    /// Frames dropped in the window (input: unread; output: sender busy).
    pub dropped: u64,
    /// Send / encode / refused errors (output) or report send errors (input) in the window.
    pub errors: u64,
    /// ms since the output's last receiver report; negative = never one. Ignored for an input.
    pub report_age_ms: i64,
}

/// One row's health: DOWN without a frame for [`JXS_DOWN_AGE_MS`]; DEGRADED when its last window
/// ran below [`JXS_MIN_FPS_X100`], lost, dropped or failed anything, or (the output) heard no
/// receiver report for [`JXS_REPORT_MAX_AGE_MS`]; else OK. Before the first window only the age
/// counts.
pub fn entry_health(e: &JxsEntryFacets) -> JxsHealth {
    if e.age_ms < 0 || e.age_ms > JXS_DOWN_AGE_MS {
        return JxsHealth::Down;
    }
    if !e.window {
        return JxsHealth::Ok;
    }
    if e.fps_x100 < JXS_MIN_FPS_X100
        || e.lost > 0
        || e.undecodable > 0
        || e.dropped > 0
        || e.errors > 0
    {
        return JxsHealth::Degraded;
    }
    if e.kind == JxsKind::Output && (e.report_age_ms < 0 || e.report_age_ms > JXS_REPORT_MAX_AGE_MS)
    {
        return JxsHealth::Degraded;
    }
    JxsHealth::Ok
}

/// The rows counted by kind and health.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct JxsCounts {
    /// JPEG XS inputs.
    pub n_inputs: i32,
    /// ... of them DOWN.
    pub n_inputs_down: i32,
    /// ... of them DEGRADED.
    pub n_inputs_degraded: i32,
    /// Running JPEG XS outputs.
    pub n_outputs: i32,
    /// ... of them DOWN.
    pub n_outputs_down: i32,
    /// ... of them DEGRADED.
    pub n_outputs_degraded: i32,
}

/// The box: OFF with no row; DOWN when an output is down or every input is (precedence output >
/// all inputs); DEGRADED when some input is down, an output is degraded, an input is degraded (in
/// that order); else OK. A count above its total saturates (all of them).
pub fn decide(c: &JxsCounts) -> (JxsState, JxsReason) {
    if c.n_inputs <= 0 && c.n_outputs <= 0 {
        return (JxsState::Off, JxsReason::NoJxs);
    }
    if c.n_outputs > 0 && c.n_outputs_down > 0 {
        return (JxsState::Down, JxsReason::OutputDown);
    }
    if c.n_inputs > 0 && c.n_inputs_down >= c.n_inputs {
        return (JxsState::Down, JxsReason::AllInputsDown);
    }
    if c.n_inputs > 0 && c.n_inputs_down > 0 {
        return (JxsState::Degraded, JxsReason::InputDown);
    }
    if c.n_outputs > 0 && c.n_outputs_degraded > 0 {
        return (JxsState::Degraded, JxsReason::OutputDegraded);
    }
    if c.n_inputs > 0 && c.n_inputs_degraded > 0 {
        return (JxsState::Degraded, JxsReason::InputDegraded);
    }
    (JxsState::Ok, JxsReason::None)
}

/// The Sources-dock transport tag of a source kind (its unversioned id): `NDI` for DistroAV's
/// `ndi_source`, `JPEG XS` for `camera_box_jxs_source`, none for anything else. By kind, never by
/// name, so it shows whatever the operator called the source.
pub fn transport_tag(unversioned_id: &str) -> Option<&'static str> {
    match unversioned_id {
        "ndi_source" => Some("NDI"),
        "camera_box_jxs_source" => Some("JPEG XS"),
        _ => None,
    }
}

/// The state's name on the label and the `jxs-state:` line.
pub fn state_name(s: JxsState) -> &'static str {
    match s {
        JxsState::Off => "OFF",
        JxsState::Ok => "OK",
        JxsState::Degraded => "DEGRADED",
        JxsState::Down => "DOWN",
    }
}

/// The reason's token on the `jxs-state:` line.
pub fn reason_key(r: JxsReason) -> &'static str {
    match r {
        JxsReason::None => "none",
        JxsReason::NoJxs => "no_jxs",
        JxsReason::OutputDown => "output_down",
        JxsReason::AllInputsDown => "all_inputs_down",
        JxsReason::InputDown => "input_down",
        JxsReason::OutputDegraded => "output_degraded",
        JxsReason::InputDegraded => "input_degraded",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(age_ms: i64) -> JxsEntryFacets {
        JxsEntryFacets {
            kind: JxsKind::Input,
            age_ms,
            window: true,
            fps_x100: 3000,
            lost: 0,
            undecodable: 0,
            dropped: 0,
            errors: 0,
            report_age_ms: -1,
        }
    }

    #[test]
    fn a_row_without_a_frame_or_older_than_the_bound_is_down() {
        assert_eq!(entry_health(&input(-1)), JxsHealth::Down);
        assert_eq!(entry_health(&input(JXS_DOWN_AGE_MS + 1)), JxsHealth::Down);
        assert_eq!(entry_health(&input(JXS_DOWN_AGE_MS)), JxsHealth::Ok);
        assert_eq!(entry_health(&input(0)), JxsHealth::Ok);
    }

    #[test]
    fn a_window_that_lost_dropped_failed_or_ran_short_degrades() {
        for e in [
            JxsEntryFacets {
                lost: 1,
                ..input(10)
            },
            JxsEntryFacets {
                undecodable: 1,
                ..input(10)
            },
            JxsEntryFacets {
                dropped: 1,
                ..input(10)
            },
            JxsEntryFacets {
                errors: 1,
                ..input(10)
            },
            JxsEntryFacets {
                fps_x100: JXS_MIN_FPS_X100 - 1,
                ..input(10)
            },
        ] {
            assert_eq!(entry_health(&e), JxsHealth::Degraded, "{e:?}");
        }
        assert_eq!(
            entry_health(&JxsEntryFacets {
                fps_x100: JXS_MIN_FPS_X100,
                ..input(10)
            }),
            JxsHealth::Ok
        );
        // before the first window only the age counts
        assert_eq!(
            entry_health(&JxsEntryFacets {
                window: false,
                lost: 9,
                fps_x100: 0,
                ..input(10)
            }),
            JxsHealth::Ok
        );
    }

    #[test]
    fn the_output_needs_a_fresh_receiver_report_an_input_never_does() {
        let out = JxsEntryFacets {
            kind: JxsKind::Output,
            report_age_ms: 900,
            ..input(10)
        };
        assert_eq!(entry_health(&out), JxsHealth::Ok);
        assert_eq!(
            entry_health(&JxsEntryFacets {
                report_age_ms: -1,
                ..out
            }),
            JxsHealth::Degraded
        );
        assert_eq!(
            entry_health(&JxsEntryFacets {
                report_age_ms: JXS_REPORT_MAX_AGE_MS + 1,
                ..out
            }),
            JxsHealth::Degraded
        );
        assert_eq!(entry_health(&input(10)), JxsHealth::Ok);
    }

    #[test]
    fn the_box_state_follows_its_rows_with_the_named_precedence() {
        let c = JxsCounts::default();
        assert_eq!(decide(&c), (JxsState::Off, JxsReason::NoJxs));
        let one_out = JxsCounts { n_outputs: 1, ..c };
        assert_eq!(decide(&one_out), (JxsState::Ok, JxsReason::None));
        assert_eq!(
            decide(&JxsCounts {
                n_outputs_down: 1,
                n_inputs: 2,
                n_inputs_down: 2,
                ..one_out
            }),
            (JxsState::Down, JxsReason::OutputDown)
        );
        let two_in = JxsCounts { n_inputs: 2, ..c };
        assert_eq!(
            decide(&JxsCounts {
                n_inputs_down: 2,
                ..two_in
            }),
            (JxsState::Down, JxsReason::AllInputsDown)
        );
        assert_eq!(
            decide(&JxsCounts {
                n_inputs_down: 5,
                ..two_in
            }),
            (JxsState::Down, JxsReason::AllInputsDown)
        );
        assert_eq!(
            decide(&JxsCounts {
                n_inputs_down: 1,
                n_outputs: 1,
                n_outputs_degraded: 1,
                ..two_in
            }),
            (JxsState::Degraded, JxsReason::InputDown)
        );
        assert_eq!(
            decide(&JxsCounts {
                n_inputs_degraded: 1,
                n_outputs: 1,
                n_outputs_degraded: 1,
                ..two_in
            }),
            (JxsState::Degraded, JxsReason::OutputDegraded)
        );
        assert_eq!(
            decide(&JxsCounts {
                n_inputs_degraded: 1,
                ..two_in
            }),
            (JxsState::Degraded, JxsReason::InputDegraded)
        );
        // a count of outputs down with no output is never DOWN (no output exists)
        assert_eq!(
            decide(&JxsCounts {
                n_outputs_down: 1,
                ..two_in
            }),
            (JxsState::Ok, JxsReason::None)
        );
    }

    #[test]
    fn the_transport_tag_is_by_kind_only() {
        assert_eq!(transport_tag("ndi_source"), Some("NDI"));
        assert_eq!(transport_tag("camera_box_jxs_source"), Some("JPEG XS"));
        assert_eq!(transport_tag("ffmpeg_source"), None);
        assert_eq!(transport_tag("NDI cam5"), None);
        assert_eq!(transport_tag(""), None);
    }
}
