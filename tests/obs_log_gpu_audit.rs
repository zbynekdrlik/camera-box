//! Regression guard for #81 — the harness must be able to AUDIT a stream OBS log
//! and surface the GPU device-removed (TDR) signature as a dead-GPU diagnosis, so
//! "stream emitted ~0 frames" is explained as a downstream GPU crash instead of an
//! unexplained zero-frames mystery.
//!
//! ## Root cause signature (captured live, stream.lan 10.77.9.204, 2026-06-16)
//!
//! The stream box's OBS log carried 6071 occurrences of:
//!   `device_texture_create (D3D11): Failed to create 2D texture (887A0005)`
//!   `  Device Removed Reason: 887A0007`
//! 887A0007 = DXGI_ERROR_DEVICE_RESET, 887A0005 = DXGI_ERROR_DEVICE_REMOVED — a GPU
//! TDR on the RTX 4060. OBS never recovered, so the compositor could not create
//! textures and the Main Output NDI emitted nothing. The genlock ingest FIFO was
//! healthy throughout (received kept climbing) — the fault is purely a dead GPU
//! downstream of the genlock ingest.

use camera_box::probe::obs_log_audit::audit_obs_log;

/// A real fixture block from the wedged stream OBS log (verbatim, abbreviated). The
/// audit MUST diagnose a GPU device-removed crash, count the occurrences, and report
/// the first timestamp it saw — so the harness can print "downstream OBS dead?
/// (GPU device-removed)" with evidence instead of a silent zero-frames result.
const WEDGED_LOG: &str = "\
03:33:37.043: genlock-fifo audit 'NDI 2ME PGM': received=379659 consumed=379657 underruns=962 overruns=0 depth=1 peak=13 preload=1 (#70)
03:33:39.533: device_texture_create (D3D11): Failed to create 2D texture (887A0005)
03:33:39.533:   Device Removed Reason: 887A0007
03:33:42.050: genlock-fifo audit 'NDI 2ME PGM': received=379660 consumed=379658 underruns=1112 overruns=0 depth=1 peak=13 preload=1 (#70)
03:33:44.265: device_texture_create (D3D11): Failed to create 2D texture (887A0005)
03:33:44.266:   Device Removed Reason: 887A0007
03:33:47.084: genlock-fifo audit 'NDI 2ME PGM': received=379852 consumed=379791 underruns=1129 overruns=1 depth=30 peak=30 preload=1 (#70)
03:33:48.938: device_texture_create (D3D11): Failed to create 2D texture (887A0005)
03:33:48.938:   Device Removed Reason: 887A0007
";

/// A clean log block (normal genlock operation, no device-removed) from before the
/// crash — the audit must report NO dead-GPU diagnosis.
const HEALTHY_LOG: &str = "\
00:02:01.000: [DistroAV] Main Output 'stream' started
00:02:02.000: genlock: render tick ENABLED (wall-clock slaved)
00:02:03.000: genlock-fifo audit 'NDI 2ME PGM': received=120 consumed=120 underruns=0 overruns=0 depth=10 peak=10 preload=1 (#70)
00:02:08.000: genlock-fifo audit 'NDI 2ME PGM': received=270 consumed=270 underruns=0 overruns=0 depth=10 peak=10 preload=1 (#70)
";

#[test]
fn wedged_log_diagnoses_dead_gpu() {
    let audit = audit_obs_log(WEDGED_LOG);
    assert!(
        audit.device_removed,
        "the 887A0007 / device-removed signature must be detected as a dead GPU"
    );
    // Counts EVERY device-lost line — both the texture-create failures and the
    // device-removed reason lines (3 of each in the fixture = 6). Each line is an
    // independent symptom of the wedged GPU.
    assert_eq!(
        audit.device_removed_count, 6,
        "must count every device-lost line (887A000x texture-create + reason)"
    );
    assert_eq!(
        audit.first_timestamp.as_deref(),
        Some("03:33:39.533"),
        "must report the first device-removed timestamp for triage"
    );
    // The 887A000x DXGI codes must be surfaced in the diagnosis so an operator can
    // grep / confirm the exact failure.
    assert!(
        audit.codes.iter().any(|c| c == "887A0007"),
        "must surface the 887A0007 (DEVICE_RESET) code"
    );
    assert!(
        audit.codes.iter().any(|c| c == "887A0005"),
        "must surface the 887A0005 (DEVICE_REMOVED) code"
    );
    let d = audit.diagnosis().to_lowercase();
    assert!(
        d.contains("device-removed") || d.contains("device removed"),
        "diagnosis must name the device-removed condition, got: {}",
        audit.diagnosis()
    );
    assert!(
        d.contains("gpu"),
        "diagnosis must point at the GPU, got: {}",
        audit.diagnosis()
    );
}

#[test]
fn healthy_log_has_no_dead_gpu_diagnosis() {
    let audit = audit_obs_log(HEALTHY_LOG);
    assert!(
        !audit.device_removed,
        "a clean genlock log must NOT be diagnosed as a dead GPU"
    );
    assert_eq!(audit.device_removed_count, 0);
    assert!(audit.first_timestamp.is_none());
    assert!(audit.codes.is_empty());
    assert!(
        audit.diagnosis().to_lowercase().contains("no")
            || audit.diagnosis().to_lowercase().contains("healthy")
            || audit.diagnosis().to_lowercase().contains("clean"),
        "a healthy diagnosis must read as no-fault, got: {}",
        audit.diagnosis()
    );
}

/// The audit must also catch the bare texture-create failure code (887A0005) even
/// if the "Device Removed Reason" line is absent (some OBS builds log only one),
/// because the texture-create failure alone is the symptom that black-holes the
/// output.
#[test]
fn texture_create_failure_alone_is_caught() {
    let log =
        "12:00:00.000: device_texture_create (D3D11): Failed to create 2D texture (887A0005)\n";
    let audit = audit_obs_log(log);
    assert!(
        audit.device_removed,
        "a 887A0005 texture-create failure alone must be diagnosed as a dead GPU"
    );
    assert_eq!(audit.device_removed_count, 1);
}
