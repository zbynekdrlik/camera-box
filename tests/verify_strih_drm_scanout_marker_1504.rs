//! Issue 1504: the script half of the retired `drm_output_vk_direct_1346.rs`.
//!
//! The vendored DRM-output module (both backends) now lives in the obs repo, whose own CI pins
//! that it logs `drm-output: program scanout LIVE` (and `drm-output: multiview bind LIVE`). The
//! consumer of that log contract stays here: verify-strih.sh item 4c reads the newest OBS log for
//! the exact marker text and feeds the result into the verdict. A renamed marker on either side
//! turns item 4c red on a healthy box, so this side is pinned by the exact string.
//!
//! Std-only: reads the script as text.

use std::fs;
use std::path::PathBuf;

const VERIFY_STRIH: &str = "scripts/verify-strih.sh";
const PROGRAM_MARKER: &str = "drm-output: program scanout LIVE";
const MULTIVIEW_MARKER: &str = "drm-output: multiview bind LIVE";

fn verify_strih() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(VERIFY_STRIH);
    fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("issue 1504: cannot read {}: {e}", p.display()))
}

/// The one line that greps `marker` in the newest OBS log and sets `flag`.
fn grep_line<'a>(script: &'a str, marker: &str, flag: &str) -> &'a str {
    let needle = format!("'{marker}' \"$DRM_LOG\"");
    let lines: Vec<&str> = script
        .lines()
        .filter(|l| l.contains("grep") && l.contains(&needle))
        .collect();
    assert_eq!(
        lines.len(),
        1,
        "issue 1504: {VERIFY_STRIH} must grep `{marker}` in \"$DRM_LOG\" exactly once, found {lines:?}"
    );
    let line = lines[0];
    assert!(
        line.trim_end().ends_with(&format!("&& {flag}=1")),
        "issue 1504: the `{marker}` grep must set {flag}=1, got `{line}`"
    );
    line
}

#[test]
fn verify_strih_greps_the_program_scanout_marker_1504() {
    let s = verify_strih();
    let line = grep_line(&s, PROGRAM_MARKER, "DRM_LIVE");
    // A fixed-string, binary-safe grep: OBS logs carry raw invalid UTF-8, and the marker holds no
    // regex metacharacters that a plain grep would read differently today but could tomorrow.
    assert!(
        line.contains("LC_ALL=C grep -aqF"),
        "issue 1504: the marker grep must be `LC_ALL=C grep -aqF`, got `{line}`"
    );
}

#[test]
fn verify_strih_greps_the_multiview_bind_marker_1504() {
    let s = verify_strih();
    let line = grep_line(&s, MULTIVIEW_MARKER, "DRM_MV_LIVE");
    assert!(line.contains("LC_ALL=C grep -aqF"), "got `{line}`");
}

#[test]
fn both_marker_flags_reach_the_verdict_1504() {
    let s = verify_strih();
    let call = s
        .lines()
        .find(|l| l.contains("DRM_VERDICT=\"$(strih_drm_output_verdict "))
        .unwrap_or_else(|| panic!("issue 1504: {VERIFY_STRIH} must call strih_drm_output_verdict"));
    assert!(
        call.contains("\"$DRM_LIVE\" \"$DRM_MV_LIVE\""),
        "issue 1504: the verdict must take DRM_LIVE then DRM_MV_LIVE, got `{call}`"
    );
    assert!(
        s.contains(&format!("never reached '{PROGRAM_MARKER}'")),
        "issue 1504: the lease-not-live line must name the marker the operator should look for"
    );
}
