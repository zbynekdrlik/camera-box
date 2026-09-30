//! #457 — `setup-device.sh` must install the SAME dev-build binary the fleet runs (never a
//! GitHub release) and fetch the licensed NDI runtime from a known fleet peer — no manual
//! per-box copy required for either.
//!
//! Live incident (cam6, 2026-07-03): a fresh setup-device.sh run installed a camera-box binary
//! with NO `-dev.N` version string (a GitHub *release* build) while the whole fleet ran
//! `1.7.0-dev.157` (a CI dev build) — cam6 drifted from the fleet and required a manual `scp` of
//! cam1's binary to restore parity. Two manual steps a "one-shot" install must not require:
//!   1. STEP 3 binary: downloaded from `releases/latest` — a GitHub release, never a dev build.
//!   2. STEP 4 NDI: never fetched at all — the script only PRINTED an `scp ... root@<DEVICE_IP>
//!      ...` instruction for the operator to run manually BEFORE invoking the script.
//!
//! These guards pin the load-bearing contract of the fix: read the REAL provisioning script and
//! assert on the REAL commands. Style mirrors `provisioning_realtime_isolation.rs` /
//! `setup_imag_guards.rs`. RED before the fix (old releases/latest URL + manual-scp-only NDI step
//! present, new resolution/fetch logic absent); GREEN after.

use std::path::PathBuf;

const SCRIPT: &str = "scripts/setup-device.sh";

fn read_script() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(SCRIPT);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// True if `needle` appears on a line that is NOT a `#` comment — a comment merely mentioning the
/// string can't satisfy the assertion; the real command must be present. Mirrors the
/// `on_noncomment_line` helper in `appliance_boot_hardening.rs` / `provisioning_realtime_isolation.rs`.
fn on_noncomment_line(body: &str, needle: &str) -> bool {
    body.lines()
        .any(|l| l.contains(needle) && !l.trim_start().starts_with('#'))
}

/// 1. The old GitHub RELEASE url must be gone — it is what silently version-drifted a fresh box
///    from the fleet's dev build (cam6, 2026-07-03).
#[test]
fn setup_device_no_longer_installs_a_github_release_binary() {
    let body = read_script();
    assert!(
        !body.contains("releases/latest/download/camera-box-linux-amd64.tar.gz"),
        "setup-device.sh must NOT install a GitHub *release* binary any more — the fleet runs CI \
         dev-builds (e.g. 1.7.0-dev.157), and a release install silently version-drifts a fresh \
         box from the fleet (cam6, #457)"
    );
}

/// 2. A fresh box must be able to match the fleet EXACTLY with no manual copy: an explicit
///    override (`--binary <url|path>` / `CAMERA_BOX_BINARY_URL`) plus a default that pulls the
///    CURRENT CI dev-build artifact — mirroring scripts/deploy-fleet.sh's own gh-run-download
///    mechanism (never re-invent a raw curl/API artifact fetch; GitHub Actions artifacts need
///    `gh`'s auth even on a public repo).
///
///    #1394: the run is resolved by the ONE shared resolver deploy-fleet.sh also uses
///    (`ci_run_latest_success`, scripts/lib/ci-run-resolve.sh, anchored on the branch head),
///    never by an inline `gh run list` query of its own.
#[test]
fn setup_device_binary_install_supports_override_and_ci_default() {
    let body = read_script();
    for needle in [
        "--binary",
        "CAMERA_BOX_BINARY_URL",
        "ci_run_latest_success",
        "gh run download",
        "camera-box-linux-amd64",
    ] {
        assert!(
            on_noncomment_line(&body, needle),
            "setup-device.sh STEP 3 must reference `{needle}` — an explicit override plus a \
             gh-run-download CI-artifact default so a fresh box matches the fleet without a hand \
             copy (#457)"
        );
    }
}

/// 3. The default CI lookup must target the fleet's PRODUCTION channel (`main`) and filter to
///    successful runs only — never the dev tip and never unfiltered (an in-flight/failed run could
///    otherwise become "latest").
///
///    CONTRACT REVERSAL (#1066/#1136, was #457): this test used to REQUIRE the `dev` default, on
///    the premise "the fleet never runs a main build". That premise is now stale — under the
///    early-gate PIN doctrine (issue 1136) the fleet's production truth is MAIN's pinned release,
///    and `scripts/deploy-fleet.sh` already defaults to `BRANCH=main`. A dev-tip provision drifted
///    cam1 to dev.626 vs the fleet's main pin dev.624 and tripped the `[0/8]` PIN-DRIFT gate
///    (2026-09-13). So this asserts the REVERSED contract (default `main`), matching deploy-fleet.
///
///    CONTRACT CHANGE (#1394): the lookup no longer carries its own server-side `--status success
///    --limit 1` query — that query trusts ONE runs listing, which GitHub intermittently serves
///    STALE (30.9.2026: a 3-week-old build went to four camboxes). It now calls the shared
///    resolver with the branch, which picks the branch head's own successful run and keeps
///    "success only" on the client side (conclusion == success, pinned by
///    tests/python/test_ci_run_resolve_1394.py).
#[test]
fn setup_device_ci_binary_lookup_targets_main_branch_success_only() {
    let body = read_script();
    assert!(
        on_noncomment_line(
            &body,
            r#"ci_run_latest_success "$GITHUB_REPO" "$CI_BRANCH" ci.yml camera-box-linux-amd64"#
        ),
        "setup-device.sh's default CI-artifact lookup must resolve the CI_BRANCH run through the \
         shared resolver (#457, #1394)"
    );
    assert!(
        !on_noncomment_line(&body, "--status success"),
        "setup-device.sh must not keep its own server-side `--status success` listing query — \
         the shared resolver filters success on the client side and anchors on the branch head \
         (#1394)"
    );
    assert!(
        body.contains(r#"CI_BRANCH="${CAMERA_BOX_CI_BRANCH:-main}"#),
        "CI_BRANCH must default to the fleet's PRODUCTION channel ('main', matching \
         deploy-fleet.sh's BRANCH=main), not the dev tip — a dev-tip provision trips the [0/8] \
         PIN-DRIFT gate (#1066/#1136)"
    );
}

/// 4. `gh run list -q '.[0].databaseId'` on an EMPTY result list yields the literal text "null"
///    (jq's normal behaviour indexing a nonexistent array element), not an empty string — a bare
///    `[ -n "$RUN_ID" ]` guard would wrongly treat "null" as a real id and proceed to `gh run
///    download null ...`. `// empty` was the fix for the old inline query (matches the
///    setup-imag.sh pattern, `.claude/skills/ops` #458 footgun #3).
///
///    #1394: the inline query is gone; RUN_ID now comes from the shared resolver, which prints
///    only a numeric run id (anything else is skipped, pinned by the resolver's pytest suites) and
///    an empty stdout on failure. So the guard here is: no `.[0].databaseId` query is left in
///    setup-device.sh, and the resolver lib is sourced.
#[test]
fn setup_device_run_id_resolution_guards_against_jq_null_string() {
    let body = read_script();
    assert!(
        !on_noncomment_line(&body, ".[0].databaseId"),
        "setup-device.sh must not resolve RUN_ID with its own `.[0].databaseId` listing query any \
         more — the shared resolver prints only a numeric run id (#457, #1394)"
    );
    assert!(
        on_noncomment_line(&body, r#". "$HERE/lib/ci-run-resolve.sh""#),
        "setup-device.sh must source the shared CI run resolver lib (#1394)"
    );
}

/// 5. STEP 4 must actually FETCH the NDI runtime (not just print a copy instruction) from a
///    known fleet peer, mirroring the already-proven `setup-imag.sh` step-10 pattern (scp a
///    versioned `libndi.so.*.*.* ` from a live fleet member, then symlink `libndi.so.6`/`libndi.so`
///    onto it) — never re-invent that dance.
#[test]
fn setup_device_ndi_step_fetches_from_a_fleet_peer() {
    let body = read_script();
    for needle in ["NDI_PEER", "sshpass", "libndi.so.*.*.*"] {
        assert!(
            on_noncomment_line(&body, needle),
            "setup-device.sh STEP 4 must reference `{needle}` — it must actively FETCH the \
             licensed NDI runtime from a known fleet peer, not just print a manual scp \
             instruction (#457)"
        );
    }
    assert!(
        body.contains(r#"NDI_PEER="${CAMERA_BOX_NDI_PEER:-10.77.9.61}"#),
        "NDI_PEER must default to the known-good fleet source (cam1, 10.77.9.61) and be \
         env-overridable via CAMERA_BOX_NDI_PEER (#457)"
    );
}

/// 6. STEP 4 must not treat "print an scp instruction" as success any more when the fetch
///    itself fails — the OLD unconditional print-only text must be gone. (A manual fallback
///    message pointing at the resolved `$NDI_PEER` after a genuinely failed fetch is fine and
///    expected; the old ALWAYS-manual text keyed on a literal DEVICE_IP placeholder is not.)
#[test]
fn setup_device_ndi_step_no_longer_always_requires_a_manual_copy() {
    let body = read_script();
    assert!(
        !body.contains("scp /usr/lib/ndi/* root@<DEVICE_IP>:/usr/lib/ndi/"),
        "setup-device.sh must NOT print the old unconditional 'copy from dev machine BEFORE \
         running this script' instruction — NDI must be actively fetched from a fleet peer \
         instead (#457)"
    );
}

/// 7. The STEP 19 final summary must no longer tell the operator to manually copy NDI from a
///    hardcoded `root@10.77.9.61` — it already got fetched in STEP 4 (or the fallback message
///    now names the resolved `$NDI_PEER` variable, not a bare literal IP repeated a second time).
#[test]
fn setup_device_summary_no_longer_hardcodes_manual_ndi_copy_instruction() {
    let body = read_script();
    assert!(
        !body.contains("Copy NDI library: scp root@10.77.9.61:/usr/lib/ndi/* /usr/lib/ndi/"),
        "setup-device.sh's STEP 19 summary must not repeat the old hardcoded manual NDI copy \
         instruction — STEP 4 now fetches NDI automatically (#457)"
    );
}
