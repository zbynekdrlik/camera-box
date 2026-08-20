//! #1141 — head-end OPTICAL blur/shutter preflight (pure crate-root SOURCE OF TRUTH).
//!
//! The owner's founding requirement (every cambox carries all frames monotonically, smooth
//! motion to the eye) was failing at the OPTICAL HEAD of the chain, which no gate guarded. A
//! genuinely misconfigured camera — a slow shutter (1/60), PAL/50 Hz, or anti-flicker — blurs
//! the captured picture, and the existing `[0/8]` capture-RATE preflight (#656) is BLIND to it:
//! a 1/60 shutter still captures ~60 frames/s, just smeared. That is exactly the #216 precedent
//! (1/60 shutter → 16.7 ms exposure = a full 60 Hz frame period → the moving dual-QR smears
//! across a tick transition → optically undecodable → a 175 s optical-read gap), and #656's
//! rate check waves it straight through.
//!
//! This module holds the PURE decision + calibrated thresholds + the NAMED Slovak ABORT
//! message. The head-end signal it reads is the `rough=` term the running `camera-box` service
//! ALREADY logs on its `capture chroma: … rough=N` line ([`crate::capture::luma_roughness`],
//! #1079): the mean |Y0−Y1| of horizontally-adjacent luma pairs on the captured frame — HIGH
//! for a crisp high-contrast pattern, LOW when motion blur smears adjacent pixels together. The
//! painter's dual-QR ADVANCES every 60 Hz flip, so it is a MOVING pattern: a healthy fast
//! shutter (1/1000) captures each tick crisply (high roughness); a slow shutter smears
//! consecutive ticks into one frame (low roughness AND undecodable). So a persistently LOW
//! head-end roughness is the direct optical signature of the misconfigured-camera class this
//! preflight aborts on.
//!
//! ## Calibration (corrected per #1130 — the ticket's own table is the OBSERVER EFFECT)
//! MEASURED healthy baseline (live CAM1 2026-08-20, 1/1000 shutter, 60 fps, 800+ samples):
//! `rough ∈ 7.1–8.0`, median 7.6, very tight. The [`crate::capture::NOISE_ROUGHNESS_THRESHOLD`]
//! (= 30) scale confirms structured content sits low (typ. < 15); motion blur collapses it
//! further toward the flat floor (~0–2). The `imag_optical_stuck_density ≈ 0.195` /
//! `presentation_cadence uniform ≈ 0.70` numbers in the #1141 body are NOT a sick camera — they
//! are the OBSERVER EFFECT (imag OBS x264 recorder load during the E2E recording, #1130 hop-by-hop
//! finding; the head-end raw v4l2 capture decodes +1/frame monotonic, ~0 %). So they are NOT this
//! detector's calibration source; the sick signature is the #216 BLUR class, read off head-end
//! `rough=` before the recorder chain ever runs.
//!
//! ## Boundary
//! `rough=` catches the BLUR class (shutter / anti-flicker). A crisp-but-wrong-CADENCE camera
//! (e.g. PAL 50 Hz producing sharp frames at 50 fps) is already covered by the #656 capture-RATE
//! preflight — the two preflights together cover blur + rate. The complementary head-end
//! tick/stuck-DECODE detector needs a QR decoder the cam boxes do not have (no zbar, no probe
//! binary — verified live 2026-08-20), so it is deferred (the ticket's own task 3, blocked on the
//! #1130 observer-effect fix + a genuinely-sick clean-run calibration).
//!
//! The shell orchestration (`scripts/lib/optical-preflight.sh`) REPLICATES the constants +
//! message below and is pinned to them by `tests/harness_optical_preflight_1141.rs`, so the two
//! can never drift (the repo's python/shell anchor-replication pattern).

/// The head-end roughness ABORT floor: when the recent `rough=` telemetry is SUSTAINED at or
/// below this, the captured picture is blurred (a misconfigured camera) and the run is aborted.
///
/// Calibrated CONSERVATIVELY (the owner's hardest constraint: never false-abort a CI gate). It
/// sits ~3× below the measured healthy baseline (median 7.6, min 7.1 across 800+ live samples on
/// the healthy 1/1000-shutter fleet, 2026-08-20), well inside the "blur collapses roughness
/// toward the flat floor (~0–2)" band. A healthy structured capture never approaches it; a
/// full-frame-period (1/60) blur clearly crosses it. `f32` to match `rough=`
/// ([`crate::capture::luma_roughness`]) exactly.
pub const OPTICAL_PREFLIGHT_ROUGH_FLOOR: f32 = 2.5;

/// Minimum number of head-end `rough=` samples required before the preflight JUDGES. Fewer than
/// this (a freshly-restarted service with little telemetry yet) is [`OpticalPreflightVerdict::InsufficientData`]
/// → the caller NOTEs and PROCEEDS, never aborts on thin data. At the ~5 s `capture chroma:`
/// cadence this is ~25 s of telemetry — enough for a robust median, short enough for a preflight.
pub const OPTICAL_PREFLIGHT_MIN_SAMPLES: usize = 5;

/// The operator-facing NAMED abort message (Slovak). Ownership is explicit: the camera must be
/// physically re-configured — software cannot fix a blurred optical capture.
///
/// Held as a fixed const so `scripts/lib/optical-preflight.sh` can reproduce it BYTE-for-byte and
/// `tests/harness_optical_preflight_1141.rs` can pin the two together.
pub const OPTICAL_PREFLIGHT_ABORT_MESSAGE: &str =
    "kamera je zle nastavená — snímaný obraz je rozmazaný (pomalý shutter / anti-flicker), \
dual-QR sa opticky nedá čítať. Nastav shutter 1/500+, 60p, anti-flicker/flicker OFF. \
Treba FYZICKY nastaviť kameru — softvér to nevyrieši.";

/// The verdict of the head-end optical preflight over a slice of recent `rough=` samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpticalPreflightVerdict {
    /// Enough samples AND the median roughness is comfortably above the floor — the captured
    /// picture is crisp; proceed.
    Healthy,
    /// Enough samples AND the median roughness is AT OR BELOW [`OPTICAL_PREFLIGHT_ROUGH_FLOOR`] —
    /// the captured picture is blurred (misconfigured camera). ABORT.
    SickBlur,
    /// Fewer than [`OPTICAL_PREFLIGHT_MIN_SAMPLES`] finite samples — NOTE and proceed, never
    /// abort on thin/absent telemetry.
    InsufficientData,
}

/// Extract every `rough=<number>` value from a block of journal text (the `capture chroma: …
/// rough=N -> …` lines the running service emits). Non-finite / unparseable tokens are skipped.
/// Pure so the shell lib's `grep -oE 'rough=[0-9.]+'` extraction can be pinned against it.
pub fn parse_rough_samples(journal: &str) -> Vec<f32> {
    let mut out = Vec::new();
    for tok in journal.split_whitespace() {
        if let Some(num) = tok.strip_prefix("rough=") {
            if let Ok(v) = num.parse::<f32>() {
                if v.is_finite() {
                    out.push(v);
                }
            }
        }
    }
    out
}

/// Median of the FINITE samples (non-finite values are dropped first). `None` when no finite
/// sample exists. Even counts average the two central values. Robust to a single outlier — a
/// systematic blur pulls the whole distribution down, so the median tracks it faithfully.
pub fn median(samples: &[f32]) -> Option<f32> {
    let mut v: Vec<f32> = samples.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(f32::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        Some(v[n / 2])
    } else {
        Some((v[n / 2 - 1] + v[n / 2]) / 2.0)
    }
}

/// Classify the head-end optical health from recent `rough=` samples.
///
/// - Fewer than [`OPTICAL_PREFLIGHT_MIN_SAMPLES`] finite samples → [`OpticalPreflightVerdict::InsufficientData`]
///   (NOTE + proceed; never abort on thin telemetry).
/// - Median roughness AT OR BELOW [`OPTICAL_PREFLIGHT_ROUGH_FLOOR`] → [`OpticalPreflightVerdict::SickBlur`]
///   (a blurred, misconfigured camera — ABORT).
/// - Otherwise → [`OpticalPreflightVerdict::Healthy`].
///
/// The MEDIAN (not the mean or a single dip) is the "sustained" test: a systematic blur pulls the
/// whole distribution below the floor, while a lone spurious low sample cannot cross it — the
/// owner's hardest constraint that a healthy run is never false-aborted.
pub fn classify(rough_samples: &[f32]) -> OpticalPreflightVerdict {
    let finite = rough_samples.iter().filter(|x| x.is_finite()).count();
    if finite < OPTICAL_PREFLIGHT_MIN_SAMPLES {
        return OpticalPreflightVerdict::InsufficientData;
    }
    match median(rough_samples) {
        Some(m) if m <= OPTICAL_PREFLIGHT_ROUGH_FLOOR => OpticalPreflightVerdict::SickBlur,
        Some(_) => OpticalPreflightVerdict::Healthy,
        None => OpticalPreflightVerdict::InsufficientData,
    }
}

// #1141 [review]: the operator-facing ERROR line is composed by the SHELL (optical_preflight_assert
// in scripts/lib/optical-preflight.sh) — the layer that actually runs the preflight. A Rust-side
// composer would be production-dead here and could silently drift from the message users see (the
// parity that matters — the fixed OPTICAL_PREFLIGHT_ABORT_MESSAGE — is pinned by the harness test),
// so no `abort_line` is kept (MVP: no dead code).

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rough_samples_extracts_all_values_1141() {
        let journal = "\
capture chroma: u_dev=6.3 v_dev=10.4 rough=7.8 -> colour
capture chroma: u_dev=6.4 v_dev=9.3 rough=7.5 -> colour
NDI display: 16.0 fps (1920x1080 -> 1920x1080)
capture chroma: u_dev=5.9 v_dev=9.2 rough=1.4 -> colour";
        assert_eq!(parse_rough_samples(journal), vec![7.8, 7.5, 1.4]);
    }

    #[test]
    fn parse_rough_samples_skips_garbage_and_nonfinite_1141() {
        assert_eq!(
            parse_rough_samples("rough=abc rough= rough=inf rough=3.2"),
            vec![3.2]
        );
        assert!(parse_rough_samples("no rough here").is_empty());
    }

    #[test]
    fn median_odd_even_and_empty_1141() {
        assert_eq!(median(&[7.6]), Some(7.6));
        assert_eq!(median(&[1.0, 3.0, 2.0]), Some(2.0));
        assert_eq!(median(&[1.0, 2.0, 3.0, 4.0]), Some(2.5));
        assert_eq!(median(&[]), None);
        assert_eq!(median(&[f32::NAN, f32::INFINITY]), None);
    }

    #[test]
    fn median_is_robust_to_a_single_outlier_1141() {
        // One spurious high sample must not rescue a blurred distribution.
        assert_eq!(median(&[1.0, 1.2, 0.9, 1.1, 40.0]), Some(1.1));
    }

    /// The measured healthy fleet baseline (live CAM1 2026-08-20, 1/1000 shutter) must PROCEED.
    #[test]
    fn healthy_fleet_baseline_is_healthy_1141() {
        let healthy = [7.8, 7.5, 7.7, 7.6, 7.9, 7.4, 7.6, 7.6];
        assert_eq!(classify(&healthy), OpticalPreflightVerdict::Healthy);
    }

    /// A sustained blurred capture (a 1/60 shutter collapses roughness toward the flat floor)
    /// must be caught as SickBlur — the #216 class this preflight exists to abort.
    #[test]
    fn sustained_blur_is_detected_as_sick_blur_1141() {
        let blurred = [1.4, 1.1, 0.9, 1.3, 1.2, 1.0, 1.5];
        assert_eq!(classify(&blurred), OpticalPreflightVerdict::SickBlur);
    }

    /// A single healthy sample above the floor amid blur must NOT rescue the run (median-based).
    #[test]
    fn blur_with_one_healthy_spike_still_sick_1141() {
        let mostly_blur = [1.2, 0.8, 7.6, 1.0, 1.1, 0.9];
        assert_eq!(classify(&mostly_blur), OpticalPreflightVerdict::SickBlur);
    }

    /// Thin telemetry (fewer than the minimum) never aborts — it NOTEs and proceeds.
    #[test]
    fn thin_telemetry_is_insufficient_data_1141() {
        assert_eq!(
            classify(&[1.0, 1.0]),
            OpticalPreflightVerdict::InsufficientData
        );
        assert_eq!(classify(&[]), OpticalPreflightVerdict::InsufficientData);
    }

    /// A run at the exact floor is sick (at-or-below); one just above is healthy.
    #[test]
    fn floor_boundary_is_inclusive_sick_1141() {
        let at_floor = [OPTICAL_PREFLIGHT_ROUGH_FLOOR; OPTICAL_PREFLIGHT_MIN_SAMPLES];
        assert_eq!(classify(&at_floor), OpticalPreflightVerdict::SickBlur);
        let just_above = [OPTICAL_PREFLIGHT_ROUGH_FLOOR + 0.5; OPTICAL_PREFLIGHT_MIN_SAMPLES];
        assert_eq!(classify(&just_above), OpticalPreflightVerdict::Healthy);
    }
}
