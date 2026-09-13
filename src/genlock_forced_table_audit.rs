//! #1303 part 4 — the REPORT-ONLY per-box-class certified-table AUDIO-parity audit.
//!
//! The DistroAV fork pins a certified `GENLOCK_FORCED_SETTINGS` table on every NDI input at every
//! `ndi_source_update` (`vendor/distroav/src/ndi-source.cpp`). Those values were read off a CAMERA
//! input on strih, where audio is irrelevant, so `ndi_audio` was forced `false` fleet-wide. On the
//! cg box (RESOLUME-SNV) the PROGRAM sources ARE the `sp-*_video` NDI inputs carrying SongPlayer
//! audio, so that certified `false` silently disabled program audio and only surfaced on air
//! (2026-09-13 event-morning incident, #1295 / #1303). `43de2f16f` moved `PROP_AUDIO` back to the
//! per-source whitelist (stock default true), but nothing in the deploy path VERIFIES, per box
//! class, that each input's saved `ndi_audio` / `yuv_*` matches what that box's role needs.
//!
//! This module is that verification, as a PURE decision: given a box class and the list of NDI
//! inputs with their saved `ndi_audio` + `yuv_*` colour settings, it classifies each input as
//! expected-audio / expected-silent and emits a per-input `Ok` / `Mismatch` verdict plus a
//! report-only `yuv_range=partial on a program source` advisory. It NEVER forces anything and it is
//! NEVER a gate — the deploy preflight (`scripts/lib/genlock-forced-table-audit.sh`, a byte-for-byte
//! bash replica of this table, pinned by `tests/genlock_forced_table_audit_1303.rs`) prints the
//! verdict BEFORE the swap so a program-audio box is never shipped with audio silently off and a
//! camera box is never shipped with audio bleeding into the mixer.
//!
//! ## Why crate-root + pure `std`
//!
//! This is deploy-time preflight logic, not a runtime OBS code path, so it touches no vendored C and
//! needs no C mirror (unlike `genlock_audio_pairing.rs`). It lives here as a pure module — the
//! `genlock_lock_state.rs` / `resolume_playback.rs` pattern — so it unit-tests Tier-0 (default
//! features, standalone-rustc) and is the canonical source of truth the bash replica mirrors.

/// The managed genlock OBS box classes. The audit's expectation table is keyed on the box's ROLE,
/// not its hostname: `Strih`/`Stream`/`Imag` are camera-chain boxes (unknown inputs default silent);
/// `Resolume` is the cg/program box (unknown inputs default audio).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoxClass {
    /// The camera switcher (`10.77.9.202`): camera NDI inputs (silent) + a `cg` program input.
    Strih,
    /// The final program encoder (`10.77.9.204`): program (`2ME PGM`) + music/`mbc` audio sources.
    Stream,
    /// The 60fps projection box: camera NDI inputs only (all silent).
    Imag,
    /// The traveling cg OBS (RESOLUME-SNV): SongPlayer `sp-*`, `NDIAr *`, `VBAN` program sources.
    Resolume,
}

impl BoxClass {
    /// Parse a box name (as `deploy-genlock-fleet.sh` uses it) into a class. Case-insensitive.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "strih" => Some(BoxClass::Strih),
            "stream" => Some(BoxClass::Stream),
            "imag" => Some(BoxClass::Imag),
            "resolume" => Some(BoxClass::Resolume),
            _ => None,
        }
    }

    /// The default audio expectation for an input whose name matches NO specific role rule on this
    /// box. Camera-chain boxes treat an unrecognised input like a camera (silent); the cg box treats
    /// one like a program source (audio).
    fn default_expectation(self) -> AudioExpectation {
        match self {
            BoxClass::Resolume => AudioExpectation::ExpectedAudio,
            BoxClass::Strih | BoxClass::Stream | BoxClass::Imag => AudioExpectation::ExpectedSilent,
        }
    }
}

/// Whether a genlock NDI input SHOULD carry audio into the mixer, given its role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioExpectation {
    /// A program / music / SongPlayer source — audio MUST be enabled (`ndi_audio=true`).
    ExpectedAudio,
    /// A camera source — audio into the mixer is not wanted (`ndi_audio=false`, the certified
    /// camera value).
    ExpectedSilent,
}

/// The per-input audio verdict comparing the expectation against the saved `ndi_audio`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioVerdict {
    /// The saved `ndi_audio` matches the role's expectation.
    Ok,
    /// A program source with audio DISABLED — the #1303 live defect (silent program audio).
    MismatchProgramSilent,
    /// A camera source with audio ENABLED — audio bleeding into the camera chain.
    MismatchCameraAudible,
}

impl AudioVerdict {
    /// True for any mismatch (either direction) — the summary flag the preflight prints.
    pub fn is_mismatch(self) -> bool {
        !matches!(self, AudioVerdict::Ok)
    }
}

/// One NDI input's saved settings, as enumerated over OBS-WS
/// (`GetInputList` + `GetInputSettings`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NdiInput {
    /// The OBS input name (e.g. `sp-fast_video`, `CAM3 (usb)`, `cg`).
    pub name: String,
    /// The saved `ndi_audio` whitelist value.
    pub ndi_audio: bool,
    /// The saved `yuv_range` (e.g. `partial` / `full` / empty when at the default).
    pub yuv_range: String,
    /// The saved `yuv_colorspace` (e.g. `BT.709` / empty).
    pub yuv_colorspace: String,
}

/// The audit result for one input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputAudit {
    /// The input name (echoed for the printed report).
    pub name: String,
    /// What the role needs.
    pub expected: AudioExpectation,
    /// The observed `ndi_audio`.
    pub actual_ndi_audio: bool,
    /// The audio verdict.
    pub verdict: AudioVerdict,
    /// Report-only advisory: a program source with a forced `yuv_range=partial`, which colour-shifts
    /// a full-range sender (the owner's "distorted picture" secondary symptom). NOT part of the
    /// audio verdict — a separate note the preflight prints so the range is eyeballed against the
    /// sender's declared range.
    pub yuv_partial_on_program: bool,
}

/// A camera NDI input: `cam` followed (eventually) by a digit, or a `(usb)` capture-card suffix.
/// These carry no program audio on the genlock receiver and keep the certified `ndi_audio=false`.
pub fn is_camera_input(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    if n.contains("(usb)") {
        return true;
    }
    // "cam" somewhere AND a digit somewhere -> a camera label like "CAM3", "cam 2", "camera1".
    n.contains("cam") && n.bytes().any(|b| b.is_ascii_digit())
}

/// A program / music / SongPlayer NDI input whose audio MUST be enabled. Matched by the concrete rig
/// naming (`sp-*_video`, `cg`, `NDI 2ME PGM`, `mbc`, `NDI obs hudba`, `NDIAr *`, `VBAN cg-resolume`).
pub fn is_program_audio_input(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    const KEYS: [&str; 9] = [
        "sp-",
        "songplayer",
        "pgm",
        "program",
        "hudba",
        "mbc",
        "vban",
        "ndiar",
        "cg",
    ];
    KEYS.iter().any(|k| n.contains(k))
}

/// The role-derived audio expectation for `name` on `box_class`. Camera inputs are silent on EVERY
/// box; program/music/SongPlayer inputs carry audio on every box; anything else falls back to the
/// box-class default.
pub fn expected_audio(box_class: BoxClass, name: &str) -> AudioExpectation {
    if is_camera_input(name) {
        AudioExpectation::ExpectedSilent
    } else if is_program_audio_input(name) {
        AudioExpectation::ExpectedAudio
    } else {
        box_class.default_expectation()
    }
}

/// The audio verdict for an expectation vs the saved `ndi_audio`.
pub fn audio_verdict(expected: AudioExpectation, ndi_audio: bool) -> AudioVerdict {
    match (expected, ndi_audio) {
        (AudioExpectation::ExpectedAudio, false) => AudioVerdict::MismatchProgramSilent,
        (AudioExpectation::ExpectedSilent, true) => AudioVerdict::MismatchCameraAudible,
        _ => AudioVerdict::Ok,
    }
}

/// The report-only yuv advisory: a program source with a forced `yuv_range` of `partial`.
pub fn yuv_partial_on_program(expected: AudioExpectation, yuv_range: &str) -> bool {
    matches!(expected, AudioExpectation::ExpectedAudio)
        && yuv_range.trim().eq_ignore_ascii_case("partial")
}

/// Classify one input on `box_class`.
pub fn classify(box_class: BoxClass, input: &NdiInput) -> InputAudit {
    let expected = expected_audio(box_class, &input.name);
    InputAudit {
        name: input.name.clone(),
        expected,
        actual_ndi_audio: input.ndi_audio,
        verdict: audio_verdict(expected, input.ndi_audio),
        yuv_partial_on_program: yuv_partial_on_program(expected, &input.yuv_range),
    }
}

/// Classify every input on `box_class` (convenience for the audit's summary).
pub fn audit_box(box_class: BoxClass, inputs: &[NdiInput]) -> Vec<InputAudit> {
    inputs.iter().map(|i| classify(box_class, i)).collect()
}

/// True iff any input mismatches (the summary flag the preflight surfaces).
pub fn any_mismatch(audits: &[InputAudit]) -> bool {
    audits.iter().any(|a| a.verdict.is_mismatch())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(name: &str, ndi_audio: bool, yuv_range: &str) -> NdiInput {
        NdiInput {
            name: name.to_string(),
            ndi_audio,
            yuv_range: yuv_range.to_string(),
            yuv_colorspace: String::new(),
        }
    }

    #[test]
    fn box_class_parses_case_insensitively() {
        assert_eq!(BoxClass::parse("strih"), Some(BoxClass::Strih));
        assert_eq!(BoxClass::parse(" Stream "), Some(BoxClass::Stream));
        assert_eq!(BoxClass::parse("IMAG"), Some(BoxClass::Imag));
        assert_eq!(BoxClass::parse("resolume"), Some(BoxClass::Resolume));
        assert_eq!(BoxClass::parse("bogus"), None);
    }

    #[test]
    fn camera_inputs_are_recognised() {
        for n in ["CAM1 (usb)", "cam3", "CAM 7", "camera2"] {
            assert!(is_camera_input(n), "{n} should be a camera input");
        }
        for n in ["sp-fast_video", "cg", "NDI 2ME PGM", "VBAN cg-resolume"] {
            assert!(!is_camera_input(n), "{n} is not a camera input");
        }
    }

    #[test]
    fn program_audio_inputs_are_recognised() {
        for n in [
            "sp-fast_video",
            "cg",
            "NDI 2ME PGM",
            "mbc",
            "NDI obs hudba",
            "NDIAr ppt",
            "VBAN cg-resolume",
        ] {
            assert!(
                is_program_audio_input(n),
                "{n} should be a program-audio input"
            );
        }
        assert!(!is_program_audio_input("CAM3 (usb)"));
    }

    #[test]
    fn camera_input_silent_is_ok_on_every_box() {
        for bc in [
            BoxClass::Strih,
            BoxClass::Stream,
            BoxClass::Imag,
            BoxClass::Resolume,
        ] {
            let a = classify(bc, &input("CAM3 (usb)", false, ""));
            assert_eq!(a.expected, AudioExpectation::ExpectedSilent);
            assert_eq!(a.verdict, AudioVerdict::Ok, "camera silent OK on {bc:?}");
        }
    }

    #[test]
    fn program_source_audio_on_is_ok() {
        let a = classify(BoxClass::Resolume, &input("sp-fast_video", true, "full"));
        assert_eq!(a.expected, AudioExpectation::ExpectedAudio);
        assert_eq!(a.verdict, AudioVerdict::Ok);
        assert!(!a.yuv_partial_on_program);
    }

    #[test]
    fn program_source_audio_off_is_the_1303_mismatch() {
        // The exact event-morning defect: sp-* on cg OBS with ndi_audio=false.
        let a = classify(
            BoxClass::Resolume,
            &input("sp-slow_video", false, "partial"),
        );
        assert_eq!(a.verdict, AudioVerdict::MismatchProgramSilent);
        assert!(a.verdict.is_mismatch());
        // and the yuv advisory fires for a forced-partial program source
        assert!(a.yuv_partial_on_program);
    }

    #[test]
    fn camera_input_with_audio_on_is_a_mismatch() {
        let a = classify(BoxClass::Strih, &input("CAM2 (usb)", true, ""));
        assert_eq!(a.verdict, AudioVerdict::MismatchCameraAudible);
        assert!(a.verdict.is_mismatch());
    }

    #[test]
    fn stream_program_and_music_expect_audio() {
        assert_eq!(
            classify(BoxClass::Stream, &input("NDI 2ME PGM", true, "")).verdict,
            AudioVerdict::Ok
        );
        assert_eq!(
            classify(BoxClass::Stream, &input("NDI 2ME PGM", false, "")).verdict,
            AudioVerdict::MismatchProgramSilent
        );
    }

    #[test]
    fn imag_unknown_input_defaults_silent() {
        // imag is a camera-chain box: an unrecognised input defaults to silent.
        assert_eq!(
            expected_audio(BoxClass::Imag, "some_odd_input"),
            AudioExpectation::ExpectedSilent
        );
    }

    #[test]
    fn resolume_unknown_input_defaults_audio() {
        assert_eq!(
            expected_audio(BoxClass::Resolume, "some_odd_input"),
            AudioExpectation::ExpectedAudio
        );
    }

    #[test]
    fn any_mismatch_summary_flag() {
        let clean = audit_box(
            BoxClass::Resolume,
            &[input("sp-fast_video", true, "full"), input("cg", true, "")],
        );
        assert!(!any_mismatch(&clean));
        let dirty = audit_box(
            BoxClass::Resolume,
            &[input("sp-fast_video", false, "partial")],
        );
        assert!(any_mismatch(&dirty));
    }

    #[test]
    fn yuv_advisory_only_on_program_partial() {
        // A camera input at partial range is NOT flagged (partial is correct for cameras).
        assert!(!yuv_partial_on_program(
            AudioExpectation::ExpectedSilent,
            "partial"
        ));
        // A program input at full range is fine.
        assert!(!yuv_partial_on_program(
            AudioExpectation::ExpectedAudio,
            "full"
        ));
        // A program input forced partial IS flagged.
        assert!(yuv_partial_on_program(
            AudioExpectation::ExpectedAudio,
            "Partial"
        ));
    }
}
