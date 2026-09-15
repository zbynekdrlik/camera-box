//! #1096 (reopened 15.9.2026) — a strih DistroAV receiver whose SDK finder never re-discovers a
//! restarted cambox sender's mDNS record has NO code path to the known-good address. The original
//! #1096 fix only connects BY-URL when a FRESH `NDIlib_find` resolves the name; when the finder stays
//! blind (the issue-1199 flaky-NIC / multicast-reception class) it falls to BY-NAME (the poisoned
//! per-process finder) forever. Live: strih `'NDI cam7'` ran 607 identical
//! `#1096 connect BY-NAME 'CAM7 (usb)' (fresh finder resolved no URL; no worse than upstream)` cycles
//! over 12 min at `received=` Δ0 while the sender emitted 60.0 fps and imag reconnected BY-URL in 6 s.
//!
//! The fix (`vendor/distroav/src/ndi-source.cpp` `ndi_source_thread`), vendored receiver side only:
//!   (b) persist `last_delivered_url_1096` (the URL that actually DELIVERED frames on a BY-URL bind)
//!       and retry it BY-URL before BY-NAME when the fresh finder resolves nothing;
//!   (a) after K=`NDI_FLEET_AFTER_NO_URL_CYCLES` consecutive finder-blind resets, synthesize the URL
//!       from the camera-box naming contract via the pure `ndi_fleet_url_for_name(name, port_index,
//!       buf, buflen)` ("CAMn (usb)" <-> 10.77.9.6n:5961; false for any non-camera name so cg /
//!       "NDI obs hudba" are never guessed), cycling ports 5961..5963 across consecutive frame-less
//!       fleet binds. The BY-URL fallback choice is the pure `ndi_fallback_bind_mode_1096(...)`.
//! Both new binds are BY-URL, so `connected_by_url_1180` stays `= url_resolved_1096` and the existing
//! #1180 identity verify + #1287 frame-less alternation apply UNCHANGED (a wrong-sender/dead-port
//! guess is caught + forced BY-NAME, exactly as today). See `.claude/rules/distroav-receiver-
//! lifecycle.md` ("finder-blind fallback ladder").
//!
//! Why std-only + offline: camera-box's `# airuleset:build-ok` bypass is disabled and the vendored C
//! compiles only on CI, so per `.claude/rules/vendored-libobs-change-safety.md` (the #767/#1096/#1180/
//! #1287 pattern) this file (a) SOURCE-ANCHORS the C tokens with a `fs::read_to_string` guard runnable
//! via `rustc --test`, and (b) LIFTS the two NEW pure helpers VERBATIM, compiles each standalone under
//! -Werror, and runs it over a hand-written truth table encoding the intended decision at every guard
//! boundary. Nothing in the Rust appliance consumes the helpers, so the truth tables ARE the spec. Per
//! test-strictness the lift-compile FAILS LOUDLY when no C compiler is present, never skips. The LIVE
//! receive-path cure is NOT offline-verifiable (the wedge reproduces only live) — UNVERIFIED until the
//! supervisor's post-deploy rig repro (bounce a cambox sender against strih and confirm self-recovery
//! without an OBS restart).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const NDI_SOURCE: &str = "vendor/distroav/src/ndi-source.cpp";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn vendor_file(rel: &str) -> String {
    let p = repo(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

fn count(hay: &str, needle: &str) -> usize {
    hay.matches(needle).count()
}

// ----------------------------------------------------------------------------------------------
// Facet A — source anchors (revert protection + wiring presence).
// ----------------------------------------------------------------------------------------------

#[test]
fn fleet_url_helper_is_defined() {
    let src = vendor_file(NDI_SOURCE);
    assert!(
        src.contains("static inline bool ndi_fleet_url_for_name("),
        "{NDI_SOURCE}: #1096 (reopen) missing the pure ndi_fleet_url_for_name(...) helper — the \
         fleet-map URL synthesis for a finder-blind sender is gone."
    );
}

#[test]
fn fallback_bind_mode_helper_is_defined() {
    let src = vendor_file(NDI_SOURCE);
    assert!(
        src.contains("static inline int ndi_fallback_bind_mode_1096("),
        "{NDI_SOURCE}: #1096 (reopen) missing the pure ndi_fallback_bind_mode_1096(...) ladder helper."
    );
}

#[test]
fn escalation_state_is_declared() {
    let src = vendor_file(NDI_SOURCE);
    assert!(
        src.contains("char *last_delivered_url_1096 = nullptr;"),
        "{NDI_SOURCE}: #1096 (reopen) missing last_delivered_url_1096 state (the last-known-good URL)."
    );
    assert!(
        src.contains("unsigned no_url_cycles_1096 = 0;"),
        "{NDI_SOURCE}: #1096 (reopen) missing the no_url_cycles_1096 finder-blind escalation clock."
    );
    assert!(
        src.contains("unsigned fleet_port_index_1096 = 0;"),
        "{NDI_SOURCE}: #1096 (reopen) missing the fleet_port_index_1096 port cursor."
    );
}

#[test]
fn constants_bound_the_ladder() {
    let src = vendor_file(NDI_SOURCE);
    assert!(
        src.contains("static const unsigned NDI_FLEET_AFTER_NO_URL_CYCLES ="),
        "{NDI_SOURCE}: #1096 (reopen) missing the K-cycle constant NDI_FLEET_AFTER_NO_URL_CYCLES."
    );
    assert!(
        src.contains("static const unsigned NDI_FLEET_PORT_CANDIDATES ="),
        "{NDI_SOURCE}: #1096 (reopen) missing the NDI_FLEET_PORT_CANDIDATES port-count constant."
    );
}

#[test]
fn ladder_is_wired_into_the_reset_block() {
    let src = vendor_file(NDI_SOURCE);
    assert!(
        src.contains("ndi_fallback_bind_mode_1096(have_last_known,"),
        "{NDI_SOURCE}: #1096 (reopen) the reset block no longer consults ndi_fallback_bind_mode_1096 \
         when the fresh finder resolves no URL."
    );
    assert!(
        src.contains("ndi_fleet_url_for_name(owned_source_name, fleet_port_index_1096,"),
        "{NDI_SOURCE}: #1096 (reopen) the reset block no longer derives the fleet-map URL from the \
         current port index."
    );
    assert!(
        src.contains("fleet_port_index_1096 = (fleet_port_index_1096 + 1) % NDI_FLEET_PORT_CANDIDATES;"),
        "{NDI_SOURCE}: #1096 (reopen) the fleet port cursor is not advanced across consecutive fleet \
         binds — the 5961..5963 probe never cycles."
    );
}

#[test]
fn by_url_umbrella_flag_is_preserved() {
    let src = vendor_file(NDI_SOURCE);
    // The last-known / fleet binds MUST route through the SAME connected_by_url_1180 = url_resolved_1096
    // wiring so #1180 identity verify + #1287 alternation apply unchanged (regression guard on the 1180
    // arming — see distroav_by_url_identity_verify_1180.rs).
    assert!(
        src.contains("connected_by_url_1180 = url_resolved_1096;"),
        "{NDI_SOURCE}: #1096 (reopen) broke the connected_by_url_1180 = url_resolved_1096 arming — a \
         last-known/fleet BY-URL bind would escape the #1180/#1287 machinery."
    );
}

#[test]
fn recovery_records_last_known_and_resets_the_clock() {
    let src = vendor_file(NDI_SOURCE);
    assert!(
        src.contains("last_delivered_url_1096 = bstrdup(owned_source_url);"),
        "{NDI_SOURCE}: #1096 (reopen) frames-delivered recovery no longer records the delivering URL \
         as last-known-good."
    );
    assert!(
        src.contains("bind_recovery_recorded_1096 = false;"),
        "{NDI_SOURCE}: #1096 (reopen) the per-bind recovery one-shot flag is not re-armed on reset."
    );
    // last_delivered_url_1096 is bfree'd in the recovery record (before bstrdup) AND on thread exit.
    assert!(
        count(&src, "bfree(last_delivered_url_1096);") >= 2,
        "{NDI_SOURCE}: #1096 (reopen) last_delivered_url_1096 is not bfree'd in BOTH the recovery \
         record and on thread exit (leak)."
    );
}

#[test]
fn new_log_markers_are_present_and_non_substring() {
    let src = vendor_file(NDI_SOURCE);
    let last_known = "#1096 rebind BY-URL '%s' (last-known good; fresh finder resolved none)";
    let fleet = "#1096 rebind BY-URL '%s' (fleet map after finder-blind cycles";
    assert!(
        src.contains(last_known),
        "{NDI_SOURCE}: #1096 (reopen) missing the last-known-good BY-URL log marker."
    );
    assert!(
        src.contains(fleet),
        "{NDI_SOURCE}: #1096 (reopen) missing the fleet-map BY-URL log marker."
    );
    // Mutually non-substring vs the existing #1096 connect BY-URL / BY-NAME lines (other tests anchor
    // on those): the new markers use "rebind BY-URL", never "connect BY-URL".
    let existing_fresh =
        "#1096 connect BY-URL '%s' (fresh finder; bypassing poisoned name resolver)";
    assert!(
        !last_known.contains(existing_fresh) && !existing_fresh.contains(last_known),
        "#1096 (reopen) last-known marker collides with the existing fresh BY-URL marker."
    );
    assert!(
        !fleet.contains(existing_fresh) && !existing_fresh.contains(fleet),
        "#1096 (reopen) fleet marker collides with the existing fresh BY-URL marker."
    );
}

// ----------------------------------------------------------------------------------------------
// Facet B — lift each NEW pure helper, compile standalone under -Werror, run a truth table.
// ----------------------------------------------------------------------------------------------

fn lift_helper(signature: &str, tag: &str) -> String {
    let src = vendor_file(NDI_SOURCE);
    let start = src.find(signature).unwrap_or_else(|| {
        panic!("#1096 (reopen): {NDI_SOURCE} no longer defines {tag} — nothing to compile. Re-apply the fix.")
    });
    let end = src[start..]
        .find("\n}\n")
        .map(|i| start + i + 3)
        .unwrap_or_else(|| panic!("#1096 (reopen): {tag} has no closing brace `\\n}}\\n`"));
    src[start..end].to_string()
}

fn compile_and_run(prelude: &str, helper: &str, main_body: &str, tag: &str) -> Vec<String> {
    let mut c = String::from(prelude);
    c.push_str(helper);
    c.push_str("\nint main(void){\n");
    c.push_str(main_body);
    c.push_str("    return 0;\n}\n");

    let dir = std::env::temp_dir().join("distroav_by_url_fleet_fallback_1096");
    fs::create_dir_all(&dir).expect("create the scratch dir");
    let cfile = dir.join(format!("{tag}.c"));
    let bin = dir.join(format!("{tag}.bin"));
    fs::write(&cfile, &c).expect("write the harness");

    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args(["-std=gnu99", "-Wall", "-Wextra", "-Wformat=2", "-Wconversion", "-Werror", "-O1"])
        .arg(&cfile)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "#1096 (reopen): could not run the C compiler `{cc}` ({e}). This gate compiles the \
                 vendored {tag} to prove it COMPILES and computes the spec; it must FAIL rather than \
                 skip when the toolchain is absent."
            )
        });
    assert!(
        out.status.success(),
        "#1096 (reopen): {tag} lifted from {NDI_SOURCE} does NOT COMPILE standalone under -Wall \
         -Wextra -Wformat=2 -Wconversion -Werror. Very likely a real compile error heading for \
         CI:\n--- cc stderr ---\n{}\n--- harness ---\n{c}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin)
        .output()
        .expect("the compiled harness failed to execute");
    assert!(
        run.status.success(),
        "#1096 (reopen): {tag} harness exited non-zero: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    String::from_utf8(run.stdout)
        .expect("harness stdout is utf-8")
        .lines()
        .map(|l| l.to_string())
        .collect()
}

#[test]
fn fleet_url_computes_the_spec_truth_table() {
    let helper = lift_helper(
        "static inline bool ndi_fleet_url_for_name(",
        "ndi_fleet_url_for_name",
    );
    // (name, port_index, expected)  expected "NONE" => false
    let vectors: &[(&str, u32, &str)] = &[
        ("CAM1 (usb)", 0, "10.77.9.61:5961"),
        ("CAM7 (usb)", 0, "10.77.9.67:5961"),
        ("CAM7 (usb)", 1, "10.77.9.67:5962"),
        ("CAM7 (usb)", 2, "10.77.9.67:5963"),
        ("CAM7 (usb)", 3, "NONE"), // port_index out of range -> false
        ("CAM8 (usb)", 0, "NONE"), // n out of contract range
        ("CAM0 (usb)", 0, "NONE"),
        ("cg", 0, "NONE"),
        ("NDI obs hudba", 0, "NONE"),
        ("CAM7", 0, "NONE"),             // missing suffix
        ("CAM7 (usb) extra", 0, "NONE"), // trailing junk
        ("CAM70 (usb)", 0, "NONE"),      // two digits
        ("", 0, "NONE"),
    ];
    let mut body = String::from("    char buf[64];\n");
    for (name, pi, _exp) in vectors {
        // C string escaping: the vectors contain no quotes/backslashes.
        body.push_str(&format!(
            "    if (ndi_fleet_url_for_name(\"{name}\", {pi}u, buf, sizeof buf)) printf(\"%s\\n\", buf); else printf(\"NONE\\n\");\n"
        ));
    }
    let prelude =
        "#include <stdio.h>\n#include <string.h>\n#include <stdbool.h>\n#include <stddef.h>\n";
    let got = compile_and_run(prelude, &helper, &body, "fleet_url");
    assert_eq!(
        got.len(),
        vectors.len(),
        "#1096 (reopen): fleet_url harness printed {} lines for {} vectors",
        got.len(),
        vectors.len()
    );
    let mut diffs = Vec::new();
    for ((name, pi, exp), g) in vectors.iter().zip(&got) {
        if g != exp {
            diffs.push(format!(
                "  ndi_fleet_url_for_name({name:?}, {pi}) -> {g:?}, expected {exp:?}"
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "#1096 (reopen): ndi_fleet_url_for_name DIVERGED:\n{}",
        diffs.join("\n")
    );
}

#[test]
fn fallback_bind_mode_computes_the_spec_truth_table() {
    let helper = lift_helper(
        "static inline int ndi_fallback_bind_mode_1096(",
        "ndi_fallback_bind_mode_1096",
    );
    // (have_last, have_fleet, no_url_cycles, K, expected)  0=BY-NAME 1=last-known 2=fleet
    let vectors: &[(bool, bool, u32, u32, i32)] = &[
        (true, true, 3, 3, 2),   // K reached + fleet -> fleet
        (true, true, 2, 3, 1),   // before K -> last-known
        (false, true, 3, 3, 2),  // K reached, no last-known -> fleet
        (false, true, 2, 3, 0),  // before K, no last-known -> BY-NAME
        (true, false, 5, 3, 1),  // no fleet URL, has last-known -> last-known even past K
        (false, false, 9, 3, 0), // nothing -> BY-NAME
        (true, true, 0, 3, 1),   // fresh blind start -> last-known first
    ];
    let mut body = String::new();
    for (hl, hf, nc, k, _e) in vectors {
        body.push_str(&format!(
            "    printf(\"%d\\n\", ndi_fallback_bind_mode_1096({}, {}, {nc}u, {k}u));\n",
            if *hl { "1" } else { "0" },
            if *hf { "1" } else { "0" },
        ));
    }
    let prelude = "#include <stdio.h>\n#include <stdbool.h>\n";
    let got = compile_and_run(prelude, &helper, &body, "fallback_mode");
    assert_eq!(
        got.len(),
        vectors.len(),
        "#1096 (reopen): fallback_mode harness printed {} lines for {} vectors",
        got.len(),
        vectors.len()
    );
    let mut diffs = Vec::new();
    for ((hl, hf, nc, k, exp), g) in vectors.iter().zip(&got) {
        let want = exp.to_string();
        if g != &want {
            diffs.push(format!(
                "  ndi_fallback_bind_mode_1096({hl},{hf},{nc},{k}) -> {g:?}, expected {want:?}"
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "#1096 (reopen): ndi_fallback_bind_mode_1096 DIVERGED:\n{}",
        diffs.join("\n")
    );
}
