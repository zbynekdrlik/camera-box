//! issue 1351 — the `[0/8]` dantesync version-parity gate must read the M4 Linux strih (strih-lx,
//! 10.77.9.202) via the gate's `--linux` arm (`dantesync --version` over plain ssh, which answers on
//! Linux), NOT the `--win` Windows quoted-exe-path arm (which returns nothing on Linux → strih
//! UNKNOWN → the gate refuses, exit 11). A Windows strih keeps strih+stream BOTH under `--win`
//! (byte-identical to the pre-1351 argv).
//!
//! Anchor-safe within the recording-e2e.sh minefield: the `--win` argument is computed into a
//! `DV_WIN_NODES` variable so the SINGLE gate invocation stays count-1 (no `if/else` invocation
//! duplication), and the platform routing is a `strih_platform "$STRIH"` branch mirroring the
//! `STRIH_LINUX_GATE_ARG` idiom already used for the version-integrity gate. Design-by main
//! (issue 1351, comment 5762681495), Approach 1.

use std::fs;
use std::path::PathBuf;

fn read(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// The version-parity gate band: from its banner to the following camera-box version-parity comment.
fn version_parity_band(body: &str) -> String {
    let start = body
        .find("[0/8] dantesync version-parity gate")
        .expect("the dantesync version-parity banner must exist");
    let end = body[start..]
        .find("camera-box binary CROSS-BOX version-parity gate")
        .map(|i| start + i)
        .expect("the camera-box version-parity comment must follow the version-parity gate");
    body[start..end].to_string()
}

/// The gate invocation must be platform-branched on `strih_platform "$STRIH"` so a Linux strih is
/// read via the `--linux` arm rather than the Windows `--win` exe-path arm.
#[test]
fn version_parity_gate_routes_strih_by_platform_1351() {
    let body = read("scripts/recording-e2e.sh");
    let band = version_parity_band(&body);
    assert!(
        band.contains("strih_platform \"$STRIH\""),
        "issue 1351: the version-parity gate band must branch on strih_platform \"$STRIH\" so a \
         Linux strih is read via --linux, not the Windows --win exe-path arm. band=\n{band}"
    );
}

/// On the Linux branch strih is appended to the `--linux` node list (read via the working
/// `dantesync --version` ssh call), as `strih=${WIN_SSH_USER:-newlevel}@$STRIH`.
#[test]
fn linux_strih_is_read_via_the_linux_arm_1351() {
    let body = read("scripts/recording-e2e.sh");
    let band = version_parity_band(&body);
    assert!(
        band.contains(
            "DANTESYNC_VERSION_LINUX=\"$DANTESYNC_VERSION_LINUX strih=${WIN_SSH_USER:-newlevel}@$STRIH\""
        ),
        "issue 1351: on a Linux strih, strih must be appended to DANTESYNC_VERSION_LINUX so it is \
         read via the working `dantesync --version` --linux arm. band=\n{band}"
    );
}

/// The gate's `--win` argument is the computed `DV_WIN_NODES` variable — NOT a hard-coded
/// `--win "strih=…"` — so strih moves off `--win` on the Linux path, and the single invocation is
/// preserved (no `if/else` invocation duplication).
#[test]
fn the_win_arg_is_computed_not_hardcoded_strih_1351() {
    let body = read("scripts/recording-e2e.sh");
    let band = version_parity_band(&body);
    assert!(
        band.contains("--win \"$DV_WIN_NODES\""),
        "issue 1351: the version-parity gate must pass --win via the computed DV_WIN_NODES \
         variable (strih routed off --win on the Linux path). band=\n{band}"
    );
    assert!(
        !band.contains("--win \"strih="),
        "issue 1351: the gate must NOT hard-code `--win \"strih=…` any more — strih is routed by \
         platform (Windows: DV_WIN_NODES carries strih+stream; Linux: strih -> --linux). band=\n{band}"
    );
    assert_eq!(
        body.matches("\"$HERE/dantesync-version-gate.sh\"").count(),
        1,
        "issue 1351: the fix computes the --win arg into a variable; it must NOT duplicate the \
         gate invocation (recording-e2e.sh anchor discipline)."
    );
}

/// The default (Windows) `DV_WIN_NODES` value carries BOTH strih and stream under `--win`, so a
/// Windows strih's version read is byte-identical to the pre-1351 argv.
#[test]
fn windows_strih_stays_under_win_via_dv_win_nodes_1351() {
    let body = read("scripts/recording-e2e.sh");
    let band = version_parity_band(&body);
    assert!(
        band.contains(
            "DV_WIN_NODES=\"strih=${WIN_SSH_USER:-newlevel}@$STRIH stream=${WIN_SSH_USER:-newlevel}@$STREAM\""
        ),
        "issue 1351: the default DV_WIN_NODES (Windows path) must carry strih+stream under --win \
         (byte-identical to the pre-1351 argv). band=\n{band}"
    );
}
