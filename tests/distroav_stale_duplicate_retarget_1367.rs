//! Issue 1367 slice — a genlock DistroAV receiver must reattach in SECONDS after a sender moves its
//! NDI port. Live (cg OBS, 6.10.2026 04:04:50–04:05:46): after SongPlayer's restart the NDI finder
//! held TWO records for `RESOLUME-SNV (SP-program)` on 10.77.9.201 — the stale `:5961` (now another
//! live sender) FIRST and the live `:5971` second. `ndi_find_url_for_source_name` returned the first
//! name match (`:5961`); the #1180 post-connect verify resolved `:5971`, logged the MISMATCH, threw
//! that correct URL away and forced BY-NAME; the SDK's name resolver followed the same stale record
//! and did not connect inside the 10 s stale window; the next fresh-finder pass picked `:5961`
//! again. Six cycles, ~55 s without frames, until the stale mDNS record aged out.
//!
//! The fix (`vendor/distroav/src/ndi-source.cpp`, design comment 6008662821, Approach 1):
//! - on a confirmed #1180 mismatch the verified URL becomes the NEXT reset's BY-URL RETARGET and
//!   the mismatched URL is EXCLUDED for this name;
//! - `ndi_find_url_for_source_name` gains an `exclude_url` argument and skips that URL among
//!   duplicate same-name records (never returning it, not even as the only match), at BOTH pick
//!   sites (the reset's fresh finder and the #1180 verify);
//! - the exclusion clears when a bind's identity verifies, or after `NDI_URL_EXCLUDE_TTL_NS`
//!   (120 s), so a sender that legitimately returns to that port is never locked out;
//! - a retarget that itself mismatches falls back to the unchanged #1180 BY-NAME safety net, so a
//!   wrong retarget can never chain into another retarget.
//!
//! Why std-only + offline: Tier-0 (no local cargo compile) and the vendored C++ compiles only on CI,
//! so per `.claude/rules/distroav-receiver-lifecycle.md` this file (A) SOURCE-ANCHORS the wiring
//! tokens (revert protection against a `git subtree pull`; mirrored as pwsh checks in BOTH
//! `windows-genlock*.yml`), (B) LIFTS the three pure helpers VERBATIM and runs each over a truth
//! table, and (C) REPLAYS the observed log through a C model of the reset → bind → verify loop
//! (`tests/c/distroav_stale_duplicate_model_1367.c`) built from the SHIPPED helpers (lifted
//! verbatim), proving the receiver reattaches on the FIRST
//! post-mismatch reset and never repeats the ladder — and that the same replay with the old wiring
//! (no retarget, no exclusion) reproduces the live ~55 s loop. The live cure is confirmed only by
//! the supervisor's post-deploy SongPlayer restart. The lift-compile FAILS LOUDLY without a C
//! compiler, never skips.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const NDI_SOURCE: &str = "vendor/distroav/src/ndi-source.cpp";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn repo_file(rel: &str) -> String {
    let p = repo(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// Collapse every run of ASCII whitespace to a single space so anchors survive reformatting.
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn require(src: &str, needle: &str, why: &str) {
    assert!(
        src.contains(needle),
        "{NDI_SOURCE}: issue 1367 patch missing — `{needle}` not found. {why}"
    );
}

/// Byte index of `needle` in `src`, panicking with `why` when absent.
fn index_of(src: &str, needle: &str, why: &str) -> usize {
    src.find(needle).unwrap_or_else(|| {
        panic!("{NDI_SOURCE}: issue 1367 patch missing — `{needle}` not found. {why}")
    })
}

// ----------------------------------------------------------------------------------------------
// Facet A — source anchors.
// ----------------------------------------------------------------------------------------------

#[test]
fn helpers_and_ttl_are_present() {
    let src = squish(&repo_file(NDI_SOURCE));
    require(
        &src,
        "static inline const char *ndi_find_url_for_source_name(const char *requested_name, const NDIlib_source_t *sources, uint32_t n_sources, const char *exclude_url)",
        "The URL picker must take the exclude_url argument, else every fresh-finder pass re-picks the \
         stale duplicate record listed first.",
    );
    require(
        &src,
        "static inline int ndi_identity_mismatch_action_1367(",
        "The mismatch -> retarget / BY-NAME decision helper is gone.",
    );
    require(
        &src,
        "static inline bool ndi_url_exclusion_active_1367(",
        "The exclusion-expiry helper is gone.",
    );
    require(
        &src,
        "static const uint64_t NDI_URL_EXCLUDE_TTL_NS = 120ULL * 1000ULL * 1000ULL * 1000ULL;",
        "The exclusion must be bounded to one stale-mDNS lifetime (120 s).",
    );
}

#[test]
fn both_pick_sites_pass_the_exclusion() {
    let src = squish(&repo_file(NDI_SOURCE));
    require(
        &src,
        "ndi_find_url_for_source_name(owned_source_name, fresh_sources, n_fresh, excluded_url_1367)",
        "The reset's fresh finder must skip the proven-stale duplicate.",
    );
    require(
        &src,
        "ndi_find_url_for_source_name(owned_source_name, v_sources, n_v, verify_exclude_1367)",
        "The #1180 verify must skip the proven-stale duplicate too, or a retarget bind is \
         'verified' against the stale record and ping-pongs back.",
    );
    require(
        &src,
        "const char *verify_exclude_1367 = ndi_url_exclusion_active_1367(excluded_since_ns_1367, os_gettime_ns(), NDI_URL_EXCLUDE_TTL_NS) ? excluded_url_1367 : nullptr;",
        "The verify's exclusion must honour the 120 s expiry.",
    );
}

#[test]
fn reset_block_binds_the_retarget_before_the_fresh_finder() {
    let src = squish(&repo_file(NDI_SOURCE));
    require(
        &src,
        "char *retarget_1367 = retarget_url_1367; retarget_url_1367 = nullptr; bound_via_retarget_1367 = false;",
        "The reset must CONSUME the retarget (this reset only) and re-arm the per-bind retarget flag.",
    );
    // The retarget wins over the fresh finder, and a forced BY-NAME wins over the retarget: the
    // retarget branch is the `if`, the unchanged #1180-gated fresh-finder block its `else if`.
    require(
        &src,
        "if (!force_by_name_1180 && retarget_1367 && retarget_1367[0]) { // Ownership of the retarget string moves to the bind. bfree(owned_source_url); owned_source_url = retarget_1367; retarget_1367 = nullptr; url_resolved_1096 = true; url_bind_kind_1096 = 3; bound_via_retarget_1367 = true; } else if (!force_by_name_1180 && owned_source_name && owned_source_name[0]) {",
        "The retarget must bind BY-URL (umbrella url_resolved_1096 so #1180/#1287 still apply) \
         ahead of the fresh finder.",
    );
    require(
        &src,
        "// camera-box #1367: a retarget this reset did not consume (a forced BY-NAME won) is dropped. bfree(retarget_1367);",
        "An unconsumed retarget must be freed.",
    );
    require(
        &src,
        "if (excluded_url_1367 && !ndi_url_exclusion_active_1367(excluded_since_ns_1367, os_gettime_ns(), NDI_URL_EXCLUDE_TTL_NS)) {",
        "The reset must expire the exclusion after NDI_URL_EXCLUDE_TTL_NS.",
    );
    require(
        &src,
        "if (url_bind_kind_1096 == 3) obs_log(LOG_WARNING, \"'%s' ndi_source_thread: reset_ndi_receiver: #1367 retarget BY-URL '%s' (verified; excluding '%s')\",",
        "The retarget bind must log its one clear line.",
    );
    // The retarget is consumed BEFORE the fresh finder is created in the reset block.
    let consume = index_of(
        &src,
        "char *retarget_1367 = retarget_url_1367;",
        "retarget consume",
    );
    let finder = index_of(
        &src,
        "NDIlib_find_instance_t fresh_finder = ndiLib->find_create_v2(&fresh_find_desc);",
        "the #1096 fresh finder create",
    );
    assert!(
        consume < finder,
        "{NDI_SOURCE}: issue 1367 — the retarget must be consumed BEFORE the reset's fresh finder."
    );
}

#[test]
fn verify_block_retargets_through_the_action_helper() {
    let src = squish(&repo_file(NDI_SOURCE));
    require(
        &src,
        "int action_1367 = ndi_identity_mismatch_action_1367(mismatch_1180, verify_url_1180, bound_via_retarget_1367);",
        "The verify must decide retarget vs BY-NAME through the pure helper.",
    );
    require(
        &src,
        "if (mismatch_1180) { bfree(excluded_url_1367); excluded_url_1367 = bstrdup(owned_source_url); excluded_since_ns_1367 = os_gettime_ns(); if (last_delivered_url_1096 && strcmp(last_delivered_url_1096, owned_source_url) == 0) { bfree(last_delivered_url_1096); last_delivered_url_1096 = nullptr; } }",
        "A confirmed mismatch must exclude the wrong-sender URL for this name and drop it from the \
         last-known-good fallback.",
    );
    require(
        &src,
        "bfree(retarget_url_1367); retarget_url_1367 = verify_url_1180; verify_url_1180 = nullptr; was_disconnected = true;",
        "On a retarget the verified URL must move into retarget_url_1367 (not be freed).",
    );
    // Order: the retarget action comes BEFORE the unchanged #1180 BY-NAME safety net, which is
    // still present byte-identical (dev1 watchdogs + the 1180 test grep it).
    let retarget = index_of(
        &src,
        "if (action_1367 == 1) {",
        "the retarget action branch",
    );
    let safety_net = index_of(
        &src,
        "\"genlock: #1180 BY-URL identity MISMATCH '%s' -- configured name now maps to '%s' but the receiver is bound to '%s'; forcing a fresh BY-NAME reset (sender NDI port reshuffle after an OBS restart?)\"",
        "the unchanged #1180 BY-NAME safety-net log line",
    );
    assert!(
        retarget < safety_net,
        "{NDI_SOURCE}: issue 1367 — the retarget action must be decided BEFORE the #1180 BY-NAME \
         safety net."
    );
    require(
        &src,
        "if (verify_url_1180 && verify_url_1180[0] && excluded_url_1367) {",
        "A verified identity must clear the exclusion.",
    );
    require(
        &src,
        "#1367 identity verified on '%s' -- clearing the exclusion of '%s'",
        "The exclusion clear must log.",
    );
}

#[test]
fn retarget_state_is_owned_kept_across_create_retries_and_freed() {
    let src = squish(&repo_file(NDI_SOURCE));
    for decl in [
        "char *retarget_url_1367 = nullptr;",
        "char *excluded_url_1367 = nullptr;",
        "uint64_t excluded_since_ns_1367 = 0;",
        "bool bound_via_retarget_1367 = false;",
    ] {
        require(
            &src,
            decl,
            "The per-receiver retarget/exclusion state is gone.",
        );
    }
    let keep = "if (bound_via_retarget_1367 && owned_source_url && owned_source_url[0]) { bfree(retarget_url_1367); retarget_url_1367 = bstrdup(owned_source_url); }";
    let n = src.matches(keep).count();
    assert!(
        n >= 2,
        "{NDI_SOURCE}: issue 1367 — a retarget bind whose recv_create / framesync create fails must \
         keep its verified URL for the retry (found {n} of the 2 failure branches)."
    );
    require(
        &src,
        "strcmp(owned_source_name, s->config.ndi_source_name) == 0)) { bfree(retarget_url_1367); retarget_url_1367 = nullptr; bfree(excluded_url_1367); excluded_url_1367 = nullptr; excluded_since_ns_1367 = 0; }",
        "A configured-name change must drop the retarget and the exclusion.",
    );
    require(
        &src,
        "bfree(retarget_url_1367); // camera-box #1367 retarget_url_1367 = nullptr; bfree(excluded_url_1367); excluded_url_1367 = nullptr;",
        "Both owned strings must be freed on thread exit.",
    );
}

#[test]
fn new_log_lines_are_distinct_from_existing_markers() {
    let ours = [
        "#1367 retarget BY-URL '%s' (verified; excluding '%s')",
        "#1367 exclusion of '%s' expired after",
        "#1367 identity verified on '%s' -- clearing the exclusion of '%s'",
    ];
    let existing = [
        "#1096 connect BY-URL '%s' (fresh finder; bypassing poisoned name resolver)",
        "#1096 rebind BY-URL '%s' (last-known good; fresh finder resolved none)",
        "#1096 rebind BY-URL '%s' (fleet map after finder-blind cycles",
        "#1180 connect BY-NAME '%s'",
        "#1096 connect BY-NAME '%s'",
    ];
    let src = repo_file(NDI_SOURCE);
    for o in ours {
        assert!(
            src.contains(o),
            "{NDI_SOURCE}: issue 1367 log line `{o}` missing"
        );
        for e in existing {
            assert!(
                !o.contains(e) && !e.contains(o),
                "issue 1367 log line `{o}` collides with the existing marker `{e}`"
            );
        }
    }
}

// ----------------------------------------------------------------------------------------------
// Facet B — lift the pure helpers VERBATIM, compile under -Werror -Wconversion -Wformat=2, run a
// truth table each. Nothing in Rust consumes them, so the truth table IS the spec.
// ----------------------------------------------------------------------------------------------

/// Lift a `static inline` helper VERBATIM, from its signature to the first `\n}\n`.
fn lift_fn(signature: &str) -> String {
    let src = repo_file(NDI_SOURCE);
    let start = src.find(signature).unwrap_or_else(|| {
        panic!(
            "issue 1367: {NDI_SOURCE} no longer defines `{signature}` — nothing to compile. \
             Re-apply the fix."
        )
    });
    let end = src[start..]
        .find("\n}\n")
        .map(|i| start + i + 3)
        .unwrap_or_else(|| panic!("issue 1367: `{signature}` has no closing brace `\\n}}\\n`"));
    src[start..end].to_string()
}

/// Lift a `static const` constant line VERBATIM, from its declaration to the terminating `;`.
fn lift_const(prefix: &str) -> String {
    let src = repo_file(NDI_SOURCE);
    let start = src.find(prefix).unwrap_or_else(|| {
        panic!("issue 1367: {NDI_SOURCE} no longer declares `{prefix}` — re-apply the fix.")
    });
    let end = src[start..]
        .find(";\n")
        .map(|i| start + i + 2)
        .unwrap_or_else(|| panic!("issue 1367: `{prefix}` has no terminating `;`"));
    src[start..end].to_string()
}

const PRELUDE: &str = "#include <stdint.h>\n#include <stddef.h>\n#include <stdbool.h>\n\
                       #include <string.h>\n#include <stdio.h>\n\
                       typedef struct { const char *p_ndi_name; const char *p_url_address; } NDIlib_source_t;\n";

fn compile_and_run(c: &str, tag: &str) -> Vec<String> {
    let dir = std::env::temp_dir().join("distroav_stale_duplicate_retarget_1367");
    fs::create_dir_all(&dir).expect("create the scratch dir");
    let cfile = dir.join(format!("{tag}.c"));
    let bin = dir.join(format!("{tag}.bin"));
    fs::write(&cfile, c).expect("write the harness");

    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args([
            "-std=gnu99",
            "-Wall",
            "-Wextra",
            "-Wformat=2",
            "-Wconversion",
            "-Werror",
            "-O1",
        ])
        .arg(&cfile)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "issue 1367: could not run the C compiler `{cc}` ({e}). This gate compiles the \
                 vendored helpers to prove they COMPILE and compute the spec; it must FAIL rather \
                 than skip when the toolchain is absent. Install a C compiler or set CC."
            )
        });
    assert!(
        out.status.success(),
        "issue 1367: the {tag} harness built from {NDI_SOURCE} does NOT COMPILE under -Wall -Wextra \
         -Wformat=2 -Wconversion -Werror — very likely a real compile error heading for CI:\n\
         --- cc stderr ---\n{}\n--- harness ---\n{c}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin)
        .output()
        .expect("issue 1367: the compiled harness failed to execute");
    assert!(
        run.status.success(),
        "issue 1367: the {tag} harness exited non-zero: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    String::from_utf8(run.stdout)
        .expect("harness stdout is utf-8")
        .lines()
        .map(|l| l.to_string())
        .collect()
}

const NAME: &str = "RESOLUME-SNV (SP-program)";
const STALE: &str = "10.77.9.201:5961";
const LIVE: &str = "10.77.9.201:5971";

fn c_str(v: Option<&str>) -> String {
    match v {
        Some(s) => format!("\"{s}\""),
        None => "NULL".to_string(),
    }
}

/// One picker vector: requested name, finder list `(name, url)`, exclude url, expected pick.
type PickRow = (
    Option<&'static str>,
    Vec<(Option<&'static str>, Option<&'static str>)>,
    Option<&'static str>,
    Option<&'static str>,
    &'static str,
);

fn picker_vectors() -> Vec<PickRow> {
    let n = Some(NAME);
    vec![
        (
            n,
            vec![(n, Some(STALE)), (n, Some(LIVE))],
            None,
            Some(STALE),
            "no exclusion -> today's first-match pick (the live stale duplicate first)",
        ),
        (
            n,
            vec![(n, Some(STALE)), (n, Some(LIVE))],
            Some(STALE),
            Some(LIVE),
            "THE FIX: the stale duplicate is excluded -> the next same-name record",
        ),
        (
            n,
            vec![(n, Some(LIVE)), (n, Some(STALE))],
            Some(STALE),
            Some(LIVE),
            "exclusion is by URL, not by position",
        ),
        (
            n,
            vec![(n, Some(STALE)), (n, Some(LIVE))],
            Some(""),
            Some(STALE),
            "EMPTY exclude = no exclusion",
        ),
        (
            n,
            vec![(n, Some(STALE)), (n, Some(LIVE))],
            Some(LIVE),
            Some(STALE),
            "excluding the other record skips that one instead",
        ),
        (
            n,
            vec![(n, Some(STALE))],
            Some(STALE),
            None,
            "the excluded URL is never returned, not even as the only match -> NULL (fallback ladder)",
        ),
        (
            n,
            vec![(n, Some(STALE)), (n, Some(STALE))],
            Some(STALE),
            None,
            "two identical stale records -> NULL",
        ),
        (
            n,
            vec![
                (Some("RESOLUME-SNV (cg-obs)"), Some(STALE)),
                (n, Some(STALE)),
                (Some("CAM1 (usb)"), Some("10.77.9.61:5961")),
                (n, Some(LIVE)),
            ],
            Some(STALE),
            Some(LIVE),
            "other names are skipped as before, the excluded same-name record too",
        ),
        (
            n,
            vec![(n, Some(STALE)), (n, Some(""))],
            Some(STALE),
            None,
            "the next same-name record has no usable address -> NULL (fall back to name)",
        ),
        (
            n,
            vec![(n, Some(STALE)), (n, Some(LIVE))],
            Some("10.77.9.201:596"),
            Some(STALE),
            "exact URL compare, never a prefix match",
        ),
        (
            None,
            vec![(n, Some(LIVE))],
            Some(STALE),
            None,
            "NULL requested name -> NULL",
        ),
        (
            n,
            vec![],
            Some(STALE),
            None,
            "empty finder list -> NULL",
        ),
    ]
}

#[test]
fn picker_skips_the_excluded_duplicate_truth_table() {
    let vs = picker_vectors();
    let mut c = String::from(PRELUDE);
    c.push_str(&lift_fn(
        "static inline const char *ndi_find_url_for_source_name(",
    ));
    c.push_str("\nint main(void){\n");
    for (i, (name, list, excl, _, _)) in vs.iter().enumerate() {
        c.push_str("    {\n");
        let (arr, count) = if list.is_empty() {
            ("(const NDIlib_source_t *)0".to_string(), "0u".to_string())
        } else {
            let cells: Vec<String> = list
                .iter()
                .map(|(n, u)| format!("{{ {}, {} }}", c_str(*n), c_str(*u)))
                .collect();
            c.push_str(&format!(
                "        NDIlib_source_t arr{i}[] = {{ {} }};\n",
                cells.join(", ")
            ));
            (format!("arr{i}"), format!("{}u", list.len()))
        };
        c.push_str(&format!(
            "        const char *r = ndi_find_url_for_source_name({}, {arr}, {count}, {});\n",
            c_str(*name),
            c_str(*excl)
        ));
        c.push_str("        printf(\"%s\\n\", r ? r : \"__NULL__\");\n    }\n");
    }
    c.push_str("    return 0;\n}\n");
    let got = compile_and_run(&c, "picker");
    assert_eq!(got.len(), vs.len(), "issue 1367: picker printed {got:?}");
    let diffs: Vec<String> = vs
        .iter()
        .zip(&got)
        .filter(|((_, _, _, want, _), g)| g.as_str() != want.unwrap_or("__NULL__"))
        .map(|((name, list, excl, want, why), g)| {
            format!("  name={name:?} list={list:?} exclude={excl:?} -> {g:?}, expected {want:?} [{why}]")
        })
        .collect();
    assert!(
        diffs.is_empty(),
        "issue 1367: ndi_find_url_for_source_name DIVERGED from the duplicate-aware spec:\n{}",
        diffs.join("\n")
    );
}

#[test]
fn mismatch_action_truth_table() {
    // (identity_mismatch, verified_url, bound_via_retarget, expected action, why)
    let vs: &[(bool, Option<&str>, bool, i32, &str)] = &[
        (false, Some(LIVE), false, 0, "identity OK -> keep"),
        (false, None, false, 0, "INCONCLUSIVE -> keep the feed"),
        (false, Some(LIVE), true, 0, "a verified retarget -> keep"),
        (
            true,
            Some(LIVE),
            false,
            1,
            "first mismatch -> RETARGET to the verified URL",
        ),
        (
            true,
            Some(LIVE),
            true,
            2,
            "the retarget itself mismatched -> #1180 BY-NAME safety net",
        ),
        (true, None, false, 2, "no verified URL -> BY-NAME"),
        (true, Some(""), false, 2, "empty verified URL -> BY-NAME"),
        (
            true,
            None,
            true,
            2,
            "retarget mismatched, no URL -> BY-NAME",
        ),
    ];
    let mut c = String::from(PRELUDE);
    c.push_str(&lift_fn(
        "static inline int ndi_identity_mismatch_action_1367(",
    ));
    c.push_str("\nint main(void){\n");
    for (mm, url, via, _, _) in vs {
        c.push_str(&format!(
            "    printf(\"%d\\n\", ndi_identity_mismatch_action_1367({}, {}, {}));\n",
            if *mm { "true" } else { "false" },
            c_str(*url),
            if *via { "true" } else { "false" }
        ));
    }
    c.push_str("    return 0;\n}\n");
    let got = compile_and_run(&c, "action");
    assert_eq!(got.len(), vs.len(), "issue 1367: action printed {got:?}");
    let diffs: Vec<String> = vs
        .iter()
        .zip(&got)
        .filter(|((_, _, _, want, _), g)| g.as_str() != want.to_string())
        .map(|((mm, url, via, want, why), g)| {
            format!("  mismatch={mm} verified={url:?} via_retarget={via} -> {g}, expected {want} [{why}]")
        })
        .collect();
    assert!(
        diffs.is_empty(),
        "issue 1367: ndi_identity_mismatch_action_1367 DIVERGED from the spec:\n{}",
        diffs.join("\n")
    );
}

#[test]
fn exclusion_expiry_truth_table() {
    const S: u64 = 1_000_000_000;
    let at: u64 = 1_000 * S;
    // (excluded_since_ns, now_ns, ttl_ns ("TTL" = the shipped constant), expected active, why)
    let vs: &[(u64, u64, &str, bool, &str)] = &[
        (0, at, "TTL", false, "nothing excluded"),
        (at, at, "TTL", true, "just set -> active"),
        (
            at,
            at - 1,
            "TTL",
            true,
            "clock behind the record -> still active",
        ),
        (at, at + 119 * S, "TTL", true, "119 s -> active"),
        (
            at,
            at + 120 * S - 1,
            "TTL",
            true,
            "1 ns before 120 s -> active",
        ),
        (
            at,
            at + 120 * S,
            "TTL",
            false,
            "exactly 120 s -> expired (the shipped TTL is 120 s)",
        ),
        (at, at + 600 * S, "TTL", false, "long past -> expired"),
        (at, at + 1, "0", false, "a zero ttl disables the exclusion"),
        (
            at,
            at + 5 * S,
            "10000000000ULL",
            true,
            "custom ttl 10 s, 5 s old -> active",
        ),
        (
            at,
            at + 10 * S,
            "10000000000ULL",
            false,
            "custom ttl 10 s, 10 s old -> expired",
        ),
    ];
    let mut c = String::from(PRELUDE);
    c.push_str(&lift_const("static const uint64_t NDI_URL_EXCLUDE_TTL_NS"));
    c.push('\n');
    c.push_str(&lift_fn(
        "static inline bool ndi_url_exclusion_active_1367(",
    ));
    c.push_str("\nint main(void){\n");
    for (since, now, ttl, _, _) in vs {
        let ttl_expr = if *ttl == "TTL" {
            "NDI_URL_EXCLUDE_TTL_NS".to_string()
        } else {
            ttl.to_string()
        };
        c.push_str(&format!(
            "    printf(\"%d\\n\", ndi_url_exclusion_active_1367({since}ULL, {now}ULL, {ttl_expr}) ? 1 : 0);\n"
        ));
    }
    c.push_str("    return 0;\n}\n");
    let got = compile_and_run(&c, "expiry");
    assert_eq!(got.len(), vs.len(), "issue 1367: expiry printed {got:?}");
    let diffs: Vec<String> = vs
        .iter()
        .zip(&got)
        .filter(|((_, _, _, want, _), g)| g.as_str() != if *want { "1" } else { "0" })
        .map(|((since, now, ttl, want, why), g)| {
            format!("  since={since} now={now} ttl={ttl} -> {g}, expected {want} [{why}]")
        })
        .collect();
    assert!(
        diffs.is_empty(),
        "issue 1367: ndi_url_exclusion_active_1367 DIVERGED from the spec:\n{}",
        diffs.join("\n")
    );
}

// ----------------------------------------------------------------------------------------------
// Facet C — replay sender port moves through a C model of the receiver's reset -> bind -> verify
// loop, built from the SHIPPED helpers (lifted verbatim) plus a thin wiring section that mirrors
// ndi_source_thread. Each scenario is a list of phases (what each finder lists, which URL our
// sender delivers on, which port is dead); any other URL a bind lands on is ANOTHER live sender.
// `legacy` runs the pre-1367 wiring and must reproduce the live ~55 s loop, so the replay is
// faithful. The model's C text lives in its own file (read at run time, like the sibling tests/c
// harnesses). Decision 6009040469 names the scenarios; no scenario may ever accept a wrong sender.
// ----------------------------------------------------------------------------------------------

/// The replay model: compiled AFTER the lifted helpers, never on its own.
const MODEL_C: &str = "tests/c/distroav_stale_duplicate_model_1367.c";

/// The shipped constants and helpers the model's wiring calls, lifted verbatim in this order.
const MODEL_CONSTS: &[&str] = &[
    "static const uint64_t GENLOCK_RECONNECT_STALE_NS",
    "static const uint64_t NDI_URL_EXCLUDE_TTL_NS",
];
const MODEL_HELPERS: &[&str] = &[
    "static inline const char *ndi_find_url_for_source_name(",
    "static inline bool ndi_by_url_identity_mismatch(",
    "static inline bool ndi_force_by_name_after_frameless(",
    "static inline int ndi_identity_mismatch_action_1367(",
    "static inline bool ndi_url_exclusion_active_1367(",
];

/// Parsed `RESULT` line of one model run.
#[derive(Debug)]
struct Outcome {
    attached: bool,
    reset: u32,
    t_ms: u64,
    mismatches: u32,
    retargets: u32,
    frameless_binds: u32,
    wrong_frames: u32,
    reopened: u32,
    excluded_ours: u32,
    max_exclusions: u32,
    wrong_sender: bool,
}

fn run_model() -> (String, Vec<(String, bool, Outcome)>) {
    let mut c = String::from(PRELUDE);
    for k in MODEL_CONSTS {
        c.push_str(&lift_const(k));
        c.push('\n');
    }
    for f in MODEL_HELPERS {
        c.push_str(&lift_fn(f));
        c.push('\n');
    }
    c.push_str(&repo_file(MODEL_C));
    let lines = compile_and_run(&c, "sequence");
    let mut out = Vec::new();
    for l in &lines {
        let Some(rest) = l.strip_prefix("RESULT ") else {
            continue;
        };
        let mut it = rest.split(' ');
        let tag = it.next().expect("tag").to_string();
        let kv: std::collections::HashMap<&str, &str> =
            it.filter_map(|p| p.split_once('=')).collect();
        let num = |k: &str| -> u64 {
            kv.get(k)
                .unwrap_or_else(|| panic!("RESULT line lacks {k}: {l}"))
                .parse()
                .unwrap_or_else(|e| panic!("RESULT {k} not a number ({e}): {l}"))
        };
        let legacy = num("legacy") == 1;
        out.push((
            tag,
            legacy,
            Outcome {
                attached: num("attached") == 1,
                reset: num("reset") as u32,
                t_ms: num("t_ms"),
                mismatches: num("mismatches") as u32,
                retargets: num("retargets") as u32,
                frameless_binds: num("frameless_binds") as u32,
                wrong_frames: num("wrong_frames") as u32,
                reopened: num("reopened") as u32,
                excluded_ours: num("excluded_ours") as u32,
                max_exclusions: num("max_exclusions") as u32,
                wrong_sender: num("wrong_sender") == 1,
            },
        ));
    }
    (lines.join("\n"), out)
}

fn outcome<'a>(all: &'a [(String, bool, Outcome)], tag: &str, legacy: bool) -> &'a Outcome {
    &all.iter()
        .find(|(t, l, _)| t == tag && *l == legacy)
        .unwrap_or_else(|| panic!("issue 1367: no RESULT for {tag} legacy={legacy}"))
        .2
}

#[test]
fn no_scenario_ever_accepts_a_wrong_sender() {
    let (trace, all) = run_model();
    assert_eq!(
        all.len(),
        6,
        "issue 1367: expected 6 model runs (5 scenarios + the legacy incident):\n{trace}"
    );
    for (tag, legacy, o) in &all {
        assert!(
            !o.wrong_sender && o.attached,
            "issue 1367: {tag} (legacy={legacy}) must end on OUR sender, never a wrong one — \
             got {o:?}:\n{trace}"
        );
        if !legacy {
            assert_eq!(
                o.excluded_ours, 0,
                "issue 1367: {tag} excluded the URL our sender delivers on:\n{trace}"
            );
        }
    }
}

#[test]
fn incident_ordering_attaches_without_a_wrong_frame() {
    let (trace, all) = run_model();
    let o = outcome(&all, "incident", false);
    assert!(
        o.attached && o.reset == 1 && o.wrong_frames == 0 && o.mismatches == 0,
        "issue 1367: the stale :5961 record is contested by cg-obs, so the FIRST pick must be the \
         live :5971 — no wrong frame, no mismatch — got {o:?}:\n{trace}"
    );
    assert!(
        o.t_ms < 1_000,
        "issue 1367: reattach must take well under a second of ladder time, took {} ms:\n{trace}",
        o.t_ms
    );

    // The SAME replay with the pre-1367 wiring reproduces the live loop: it attaches only after the
    // stale record aged out, after several mismatch cycles (live: 6 mismatches, ~55 s).
    let old = outcome(&all, "incident", true);
    assert!(
        old.attached && old.t_ms >= 55_000 && old.mismatches >= 5,
        "issue 1367: the legacy replay must reproduce the live ~55 s / 6-mismatch loop, else the \
         model is not faithful to the observed log — got {old:?}:\n{trace}"
    );
}

#[test]
fn reversed_ordering_keeps_the_correct_bind() {
    let (trace, all) = run_model();
    let o = outcome(&all, "reversed", false);
    assert!(
        o.attached && !o.wrong_sender && o.reset == 1 && o.mismatches == 0 && o.retargets == 0,
        "issue 1367: the reset binds the live :5971; a verify that lists the stale :5961 first must \
         still see :5971 as one of the name's uncontested records and keep the bind — no teardown, \
         no retarget — got {o:?}:\n{trace}"
    );
}

#[test]
fn dead_old_port_keeps_todays_alternation_without_an_exclusion() {
    let (trace, all) = run_model();
    let o = outcome(&all, "dead_old_port", false);
    assert!(
        o.attached && !o.wrong_sender && o.mismatches == 0 && o.max_exclusions == 0,
        "issue 1367: a dead, uncontested old port gives no evidence — never an exclusion, never a \
         lock-on — got {o:?}:\n{trace}"
    );
    assert!(
        o.frameless_binds >= 2
            && trace
                .contains("dead_old_port reset 1 t_ms=100 BYURL 10.77.9.201:5961 -> frame-less")
            && trace.contains("dead_old_port reset 2 t_ms=10200 BYNAME"),
        "issue 1367: the dead old port must keep today's BY-URL <-> BY-NAME alternation:\n{trace}"
    );
}

#[test]
fn a_sender_still_starting_is_never_excluded() {
    let (trace, all) = run_model();
    let o = outcome(&all, "still_starting", false);
    assert!(
        o.attached && !o.wrong_sender && o.excluded_ours == 0 && o.wrong_frames == 0,
        "issue 1367: the new :5971 is frame-less while the sender starts; it must never be excluded \
         and the contested :5961 never bound — got {o:?}:\n{trace}"
    );
    assert!(
        o.t_ms < 60_000 && o.t_ms - 12_000 <= 15_000,
        "issue 1367: must reattach within 15 s of the sender delivering (12 s) and before the stale \
         record ages out (60 s) — took {} ms:\n{trace}",
        o.t_ms
    );
}

#[test]
fn two_port_moves_never_reopen_the_first_stale_url() {
    let (trace, all) = run_model();
    let o = outcome(&all, "two_moves", false);
    assert!(
        o.attached && !o.wrong_sender && o.reopened == 0,
        "issue 1367: after A -> B -> C the first proven-stale A must stay excluded while B is \
         excluded too — got {o:?}:\n{trace}"
    );
    assert_eq!(
        o.max_exclusions, 2,
        "issue 1367: both stale URLs must be excluded at once (two slots):\n{trace}"
    );
}
