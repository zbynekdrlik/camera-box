//! Regression guard for #832 — the rig's imag-nb role must be repointed from the incumbent
//! notebook (HDMI disconnected per #831, now retired at `10.77.9.189`) to the replacement
//! (wired to the wall's HDMI since 2026-07-27, and holding the role's permanent historical
//! address `10.77.9.182` since the 2026-07-29 IP swap) via ONE declared source of truth,
//! never a literal
//! scattered across `scripts/recording-e2e.sh`, `scripts/rig-mode.sh`,
//! `scripts/recording-verdict-on-imag.sh`, and `scripts/drift-guard.sh`'s `--check-imag` call.
//!
//! Same design as #827's `scripts/camera-set.sh` / `CAMERA_ACTIVE_SET`: a `case` FACT lookup
//! (`imag_host_resolve incumbent|replacement`) that resolves BOTH boxes regardless of which is
//! active, plus ONE selector (`IMAG_HOST_ACTIVE`) deciding which fact every consumer derives
//! `IMAG_IP` from. These tests pin BOTH directions — the default active host is the replacement,
//! and overriding `IMAG_HOST_ACTIVE` (or `IMAG_IP` directly) flows through to every derived
//! consumer — driven entirely via env override, no rig needed.
//!
//! RED before #832 (no `scripts/imag-host.sh`; `recording-e2e.sh`/`rig-mode.sh` each
//! independently hardcode `IMAG_IP="${IMAG_IP:-10.77.9.182}"`; `recording-verdict-on-imag.sh` and
//! the `drift-guard.sh --check-imag` call in `rig-mode.sh` never receive the resolved host at
//! all). GREEN after.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn imag_host_script() -> PathBuf {
    manifest_dir().join("scripts/imag-host.sh")
}

/// Source `scripts/imag-host.sh` (optionally with env overrides) and read back
/// IMAG_HOST_ACTIVE / IMAG_HOST_IP / IMAG_IP via the self-check it prints when executed.
fn resolve(env: &[(&str, &str)]) -> (bool, String) {
    let script = imag_host_script();
    assert!(script.exists(), "{} not found", script.display());

    let mut cmd = Command::new("bash");
    cmd.arg(&script);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("failed to run scripts/imag-host.sh");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    )
}

#[test]
fn imag_host_defaults_to_the_replacement_182() {
    // #832: the default ACTIVE imag host is the replacement notebook -- the incumbent's HDMI is
    // disconnected (#831), so a rig run with NO override must target the replacement.
    //
    // 2026-07-29 IP SWAP (user directive): the imag ROLE keeps the historical address .182
    // permanently, so the replacement was moved ONTO .182 (it held .187 from 2026-07-27 until
    // the swap) and the retired incumbent was moved OFF it to .189. The ACTIVE selector is
    // unchanged -- still `replacement`; only the address FACT behind it moved.
    let (ok, out) = resolve(&[]);
    assert!(
        ok,
        "scripts/imag-host.sh must exit 0 by default. got: {out}"
    );
    assert!(
        out.contains("IMAG_HOST_ACTIVE=replacement") && out.contains("IMAG_IP=10.77.9.182"),
        "#832: the default active imag host must resolve to the replacement (10.77.9.182 since \
         the 2026-07-29 IP swap). got: {out}"
    );
}

#[test]
fn imag_host_active_env_override_swaps_the_incumbent_back_in() {
    // #832 requirement 2/3: the incumbent must stay a resolvable FACT, and overriding
    // IMAG_HOST_ACTIVE must flow through to IMAG_IP with ZERO code changes -- the actual proof
    // that swapping back is a one-line change, not just a comment claiming it works.
    let (ok, out) = resolve(&[("IMAG_HOST_ACTIVE", "incumbent")]);
    assert!(
        ok,
        "scripts/imag-host.sh must accept IMAG_HOST_ACTIVE=incumbent. got: {out}"
    );
    assert!(
        out.contains("IMAG_HOST_ACTIVE=incumbent") && out.contains("IMAG_IP=10.77.9.189"),
        "#832: IMAG_HOST_ACTIVE=incumbent must resolve IMAG_IP to the incumbent (10.77.9.189 \
         since the 2026-07-29 IP swap moved the retired box off .182). got: {out}"
    );
}

#[test]
fn imag_host_rejects_unknown_active_selector() {
    // A typo in IMAG_HOST_ACTIVE must FAIL loudly, never silently guess an address and certify
    // the wrong (or a nonexistent) box.
    let (ok, out) = resolve(&[("IMAG_HOST_ACTIVE", "bogus")]);
    assert!(
        !ok,
        "scripts/imag-host.sh must reject an unknown IMAG_HOST_ACTIVE value. got: {out}"
    );
}

#[test]
fn imag_ip_direct_override_wins_over_imag_host_active() {
    // An explicit IMAG_IP (an ad-hoc third address) must win over the resolved active host,
    // without needing to touch IMAG_HOST_ACTIVE or this file at all.
    let (ok, out) = resolve(&[("IMAG_IP", "10.77.9.199")]);
    assert!(ok, "got: {out}");
    assert!(
        out.contains("IMAG_IP=10.77.9.199"),
        "#832: an explicit IMAG_IP override must win over the resolved IMAG_HOST_ACTIVE default. got: {out}"
    );
}

#[test]
fn recording_e2e_sources_the_shared_imag_host_lib() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains(". \"$HERE/imag-host.sh\""),
        "#832: recording-e2e.sh must source scripts/imag-host.sh (the ONE declared imag host) \
         instead of independently hardcoding IMAG_IP's default."
    );
    assert!(
        !s.contains("IMAG_IP=\"${IMAG_IP:-10.77.9.182}\""),
        "#832: recording-e2e.sh must no longer independently declare IMAG_IP's default -- it \
         must derive it from the sourced scripts/imag-host.sh."
    );
}

#[test]
fn rig_mode_sources_the_shared_imag_host_lib() {
    let s = read("scripts/rig-mode.sh");
    assert!(
        s.contains(". \"$RIG_MODE_DIR/imag-host.sh\""),
        "#832: rig-mode.sh must source scripts/imag-host.sh (the ONE declared imag host) instead \
         of independently hardcoding IMAG_IP's default."
    );
    assert!(
        !s.contains("IMAG_IP=\"${IMAG_IP:-10.77.9.182}\""),
        "#832: rig-mode.sh must no longer independently declare IMAG_IP's default -- it must \
         derive it from the sourced scripts/imag-host.sh."
    );
}

#[test]
fn preflight_projector_count_failure_names_the_imag_host_checked() {
    // #832 requirement 4: a projector-count mismatch must name WHICH imag host was checked, so a
    // future host mismatch reads as "checked the wrong box", not a mysterious projector count.
    let s = read("scripts/recording-e2e.sh");
    let fail_line = s
        .lines()
        .find(|l| l.contains("projector count is Multiview="))
        .expect("#832: recording-e2e.sh must have the imag projector-count FAIL line");
    assert!(
        fail_line.contains("${IMAG_IP}") || fail_line.contains("$IMAG_IP"),
        "#832: the projector-count FAIL message must name the imag host it checked (${{IMAG_IP}}). \
         got: {fail_line}"
    );
}

#[test]
fn recording_e2e_passes_the_resolved_imag_host_to_recording_verdict_on_imag() {
    // recording-verdict-on-imag.sh has its OWN independent IMAG_BOX default -- without this,
    // the [8/8c] decode-on-imag step would keep ssh-ing to the OLD incumbent even after
    // recording-e2e.sh itself correctly resolved the replacement.
    let s = read("scripts/recording-e2e.sh");
    // Anchor on the ACTUAL invocation (quoted "$HERE/..." call), not a bare substring match --
    // a nearby explanatory comment mentioning the same script NAME would otherwise be the first
    // (wrong) match for a naive `.find("recording-verdict-on-imag.sh")`.
    let call = s
        .find("\"$HERE/recording-verdict-on-imag.sh\"")
        .expect("#832: recording-e2e.sh must call recording-verdict-on-imag.sh");
    let line_start = s[..call].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line = &s[line_start..call];
    assert!(
        line.contains("IMAG_BOX=\"$IMAG_IP\""),
        "#832: recording-e2e.sh must pass IMAG_BOX=\"$IMAG_IP\" to recording-verdict-on-imag.sh \
         so the [8/8c] decode step targets the SAME resolved imag host as the rest of the run. \
         line:\n{line}"
    );
}

#[test]
fn rig_mode_passes_the_resolved_imag_host_to_drift_guard_check_imag() {
    // drift-guard.sh --check-imag already supports a `host=` override -- rig-mode.sh's pre-event
    // staleness warning must actually pass it, or the check always targets drift-guard's own
    // hardcoded default regardless of which imag box the rig is actually driving.
    let s = read("scripts/rig-mode.sh");
    // Anchor on the ACTUAL invocation (`bash scripts/drift-guard.sh ...`), not a bare substring
    // match -- an explanatory comment a few lines above mentions the same script+flag inside
    // backticks with no `bash ` prefix, which would otherwise be the first (wrong) match.
    let call = s
        .find("bash scripts/drift-guard.sh --check-imag")
        .expect("#832: rig-mode.sh must call drift-guard.sh --check-imag");
    let line_start = s[..call].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line_end = s[call..].find('\n').map(|i| call + i).unwrap_or(s.len());
    let line = &s[line_start..line_end];
    assert!(
        line.contains("host=$IMAG_IP") || line.contains("host=\"$IMAG_IP\""),
        "#832: rig-mode.sh's drift-guard.sh --check-imag call must pass host=$IMAG_IP so the \
         pre-event staleness check follows the ACTIVE imag host, not drift-guard's own hardcoded \
         default. line: {line}"
    );
}
