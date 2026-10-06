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
//! table, and (C) REPLAYS the observed log through a C model of the reset → bind → verify loop built
//! from the SHIPPED helpers (lifted verbatim), proving the receiver reattaches on the FIRST
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

fn vendor_file(rel: &str) -> String {
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
    let src = squish(&vendor_file(NDI_SOURCE));
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
    let src = squish(&vendor_file(NDI_SOURCE));
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
    let src = squish(&vendor_file(NDI_SOURCE));
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
    let src = squish(&vendor_file(NDI_SOURCE));
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
    let src = squish(&vendor_file(NDI_SOURCE));
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
    let src = vendor_file(NDI_SOURCE);
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
    let src = vendor_file(NDI_SOURCE);
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
    let src = vendor_file(NDI_SOURCE);
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
// Facet C — replay the observed log through a C model of the receiver's reset -> bind -> verify
// loop, built from the SHIPPED helpers (lifted verbatim): the duplicate-aware picker, the #1180
// verdict, the action decision, the exclusion expiry, the #1287 frame-less rule and the two
// constants. `legacy` runs the SAME loop with the old wiring (no retarget, no exclusion, every
// mismatch forced BY-NAME) and must reproduce the live ~55 s loop, so the replay is faithful.
// ----------------------------------------------------------------------------------------------

const MODEL: &str = r#"
#define MS_NS 1000000ULL
#define S_NS 1000000000ULL
#define NAME "RESOLUME-SNV (SP-program)"
#define STALE "10.77.9.201:5961" /* the stale record; another LIVE sender owns this port now */
#define LIVE "10.77.9.201:5971"  /* our sender's new port */
#define OTHER "10.77.9.201:5981" /* a third live sender */

typedef struct {
	const char *tag;
	NDIlib_source_t reset_before[4]; uint32_t n_reset_before; /* the reset's fresh finder while the stale record lives */
	NDIlib_source_t reset_after[4]; uint32_t n_reset_after;   /* every finder after it aged out */
	NDIlib_source_t verify_first[4]; uint32_t n_verify_first; /* the first #1180 verify finder */
	NDIlib_source_t verify_later[4]; uint32_t n_verify_later; /* every later verify finder */
	uint64_t stale_ages_out_ns;   /* episode-relative */
	uint64_t live_delivers_from_ns; /* episode-relative: our sender delivers frames from here */
} scenario_t;

static bool same(const char *a, const char *b) { return a && b && strcmp(a, b) == 0; }

static unsigned long long ms(uint64_t ns) { return (unsigned long long)(ns / MS_NS); }

static void result(const char *tag, bool legacy, int attached, unsigned reset, uint64_t rel,
		   unsigned mismatches, unsigned retargets, unsigned by_name, unsigned stale_after,
		   unsigned chained, int wrong_sender, const char *excluded)
{
	printf("RESULT %s legacy=%d attached=%d reset=%u t_ms=%llu mismatches=%u retargets=%u by_name_resets=%u stale_binds_after_mismatch=%u chained_retargets=%u wrong_sender=%d excluded_after=%s\n",
	       tag, legacy ? 1 : 0, attached, reset, ms(rel), mismatches, retargets, by_name, stale_after,
	       chained, wrong_sender, excluded[0] ? excluded : "-");
}

static void run(const scenario_t *sc, bool legacy)
{
	const uint64_t t0 = 5ULL * S_NS; /* the monotonic clock is never 0 (0 = "nothing excluded") */
	uint64_t t = t0;
	char retarget[64] = "";
	char excluded[64] = "";
	uint64_t excluded_since = 0;
	bool force_by_name = false, prev_retarget = false, seen_mismatch = false;
	unsigned mismatches = 0, retargets = 0, by_name = 0, stale_after = 0, chained = 0, verifies = 0;
	for (unsigned reset = 1; reset <= 24; ++reset) {
		t += 100ULL * MS_NS; /* the reset block itself */
		uint64_t rel = t - t0;
		bool stale_listed = rel < sc->stale_ages_out_ns;
		const NDIlib_source_t *rl = stale_listed ? sc->reset_before : sc->reset_after;
		uint32_t nrl = stale_listed ? sc->n_reset_before : sc->n_reset_after;
		/* reset block: consume the force flag + the retarget, expire the exclusion */
		bool forced = force_by_name;
		force_by_name = false;
		char take[64];
		snprintf(take, sizeof take, "%s", retarget);
		retarget[0] = '\0';
		if (excluded[0] && !ndi_url_exclusion_active_1367(excluded_since, t, NDI_URL_EXCLUDE_TTL_NS)) {
			excluded[0] = '\0';
			excluded_since = 0;
		}
		const char *excl = (!legacy && excluded[0]) ? excluded : NULL;
		bool via_retarget = false;
		const char *bound = NULL;
		const char *mode = "BYNAME";
		if (!forced && !legacy && take[0]) {
			bound = take;
			mode = "RETARGET";
			via_retarget = true;
			retargets++;
		} else if (!forced) {
			bound = ndi_find_url_for_source_name(NAME, rl, nrl, excl);
			mode = bound ? "BYURL" : "BYNAME";
		}
		if (via_retarget && prev_retarget)
			chained++;
		prev_retarget = via_retarget;
		if (!bound) {
			/* BY-NAME: the SDK resolver follows the stale record while it is listed (live: no
			 * connection inside the stale window); once it aged out it reaches our sender. */
			by_name++;
			if (!stale_listed && rel >= sc->live_delivers_from_ns) {
				printf("%s reset %u t_ms=%llu BYNAME -> frames from " LIVE "\n", sc->tag, reset, ms(rel));
				result(sc->tag, legacy, 1, reset, rel, mismatches, retargets, by_name, stale_after, chained, 0, excluded);
				return;
			}
			printf("%s reset %u t_ms=%llu BYNAME -> no connection inside the stale window\n", sc->tag, reset, ms(rel));
			t += GENLOCK_RECONNECT_STALE_NS;
			force_by_name = ndi_force_by_name_after_frameless(false, false);
			continue;
		}
		if (seen_mismatch && same(bound, STALE))
			stale_after++;
		bool delivers = !same(bound, LIVE) || rel >= sc->live_delivers_from_ns;
		if (!delivers) {
			printf("%s reset %u t_ms=%llu %s %s -> frame-less (sender not delivering yet)\n", sc->tag, reset, ms(rel), mode, bound);
			t += GENLOCK_RECONNECT_STALE_NS;
			force_by_name = ndi_force_by_name_after_frameless(true, false);
			continue;
		}
		/* frames flow -> the one-shot #1180 identity verify (its own fresh finder) */
		t += 30ULL * MS_NS;
		const NDIlib_source_t *vl;
		uint32_t nvl;
		if (!stale_listed) {
			vl = sc->reset_after;
			nvl = sc->n_reset_after;
		} else if (verifies == 0) {
			vl = sc->verify_first;
			nvl = sc->n_verify_first;
		} else {
			vl = sc->verify_later;
			nvl = sc->n_verify_later;
		}
		verifies++;
		const char *vexcl = (!legacy && excluded[0] &&
				     ndi_url_exclusion_active_1367(excluded_since, t, NDI_URL_EXCLUDE_TTL_NS))
					    ? excluded
					    : NULL;
		const char *v = ndi_find_url_for_source_name(NAME, vl, nvl, vexcl);
		bool mm = ndi_by_url_identity_mismatch(bound, v);
		int action = legacy ? (mm ? 2 : 0) : ndi_identity_mismatch_action_1367(mm, v, via_retarget);
		if (mm) {
			mismatches++;
			seen_mismatch = true;
			if (!legacy) {
				snprintf(excluded, sizeof excluded, "%s", bound);
				excluded_since = t;
			}
		}
		if (action == 1) {
			snprintf(retarget, sizeof retarget, "%s", v);
			printf("%s reset %u t_ms=%llu %s %s -> MISMATCH (name maps to %s) -> RETARGET\n", sc->tag, reset, ms(rel), mode, bound, v);
			continue;
		}
		if (mm) {
			force_by_name = true;
			printf("%s reset %u t_ms=%llu %s %s -> MISMATCH (name maps to %s) -> BYNAME\n", sc->tag, reset, ms(rel), mode, bound, v);
			continue;
		}
		if (v && v[0] && !legacy) {
			excluded[0] = '\0';
			excluded_since = 0;
		}
		if (same(bound, LIVE)) {
			printf("%s reset %u t_ms=%llu %s %s -> frames, identity verified\n", sc->tag, reset, ms(t - t0), mode, bound);
			result(sc->tag, legacy, 1, reset, t - t0, mismatches, retargets, by_name, stale_after, chained, 0, excluded);
			return;
		}
		printf("%s reset %u t_ms=%llu %s %s -> WRONG SENDER ACCEPTED\n", sc->tag, reset, ms(rel), mode, bound);
		result(sc->tag, legacy, 0, reset, t - t0, mismatches, retargets, by_name, stale_after, chained, 1, excluded);
		return;
	}
	result(sc->tag, legacy, 0, 24, t - t0, mismatches, retargets, by_name, stale_after, chained, 0, excluded);
}

int main(void)
{
	/* A: the observed cg OBS log 6.10.2026 04:04:50 -- the reset finder lists the stale :5961
	 * first, every #1180 verify resolved :5971, the stale record aged out after ~55 s. */
	static const scenario_t observed = {
		"observed",
		{ { NAME, STALE }, { NAME, LIVE } }, 2,
		{ { NAME, LIVE } }, 1,
		{ { NAME, LIVE }, { NAME, STALE } }, 2,
		{ { NAME, LIVE }, { NAME, STALE } }, 2,
		55ULL * S_NS, 0,
	};
	/* A2: worst case -- after the first verify every finder lists the stale record first. */
	static const scenario_t stale_first = {
		"stale_first",
		{ { NAME, STALE }, { NAME, LIVE } }, 2,
		{ { NAME, LIVE } }, 1,
		{ { NAME, LIVE }, { NAME, STALE } }, 2,
		{ { NAME, STALE }, { NAME, LIVE } }, 2,
		55ULL * S_NS, 0,
	};
	/* B: strih-lx CG-obs shape -- the new :5971 record is listed before the sender delivers on it
	 * (it delivers from 12 s), the stale record lives 60 s. */
	static const scenario_t not_yet = {
		"not_yet_delivering",
		{ { NAME, STALE }, { NAME, LIVE } }, 2,
		{ { NAME, LIVE } }, 1,
		{ { NAME, LIVE }, { NAME, STALE } }, 2,
		{ { NAME, STALE }, { NAME, LIVE } }, 2,
		60ULL * S_NS, 12ULL * S_NS,
	};
	/* C: a WRONG retarget -- the first verify resolves a third live sender. */
	static const scenario_t wrong_retarget = {
		"wrong_retarget",
		{ { NAME, STALE }, { NAME, LIVE } }, 2,
		{ { NAME, LIVE } }, 1,
		{ { NAME, OTHER }, { NAME, LIVE } }, 2,
		{ { NAME, LIVE }, { NAME, OTHER } }, 2,
		55ULL * S_NS, 0,
	};
	run(&observed, false);
	run(&observed, true);
	run(&stale_first, false);
	run(&not_yet, false);
	run(&wrong_retarget, false);
	return 0;
}
"#;

/// Parsed `RESULT` line of one model run.
#[derive(Debug)]
struct Outcome {
    attached: bool,
    reset: u32,
    t_ms: u64,
    mismatches: u32,
    retargets: u32,
    by_name_resets: u32,
    stale_binds_after_mismatch: u32,
    chained_retargets: u32,
    wrong_sender: bool,
    excluded_after: String,
}

fn run_model() -> (Vec<String>, Vec<(String, bool, Outcome)>) {
    let mut c = String::from(PRELUDE);
    for k in [
        "static const uint64_t GENLOCK_RECONNECT_STALE_NS",
        "static const uint64_t NDI_URL_EXCLUDE_TTL_NS",
    ] {
        c.push_str(&lift_const(k));
        c.push('\n');
    }
    for f in [
        "static inline const char *ndi_find_url_for_source_name(",
        "static inline bool ndi_by_url_identity_mismatch(",
        "static inline bool ndi_force_by_name_after_frameless(",
        "static inline int ndi_identity_mismatch_action_1367(",
        "static inline bool ndi_url_exclusion_active_1367(",
    ] {
        c.push_str(&lift_fn(f));
        c.push('\n');
    }
    c.push_str(MODEL);
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
                by_name_resets: num("by_name_resets") as u32,
                stale_binds_after_mismatch: num("stale_binds_after_mismatch") as u32,
                chained_retargets: num("chained_retargets") as u32,
                wrong_sender: num("wrong_sender") == 1,
                excluded_after: kv["excluded_after"].to_string(),
            },
        ));
    }
    (lines, out)
}

fn outcome<'a>(all: &'a [(String, bool, Outcome)], tag: &str, legacy: bool) -> &'a Outcome {
    &all.iter()
        .find(|(t, l, _)| t == tag && *l == legacy)
        .unwrap_or_else(|| panic!("issue 1367: no RESULT for {tag} legacy={legacy}"))
        .2
}

#[test]
fn replay_observed_log_reattaches_on_the_first_post_mismatch_reset() {
    let (trace, all) = run_model();
    let trace = trace.join("\n");
    let new = outcome(&all, "observed", false);
    assert!(
        new.attached && !new.wrong_sender,
        "issue 1367: the observed replay never reattached to our sender:\n{trace}"
    );
    assert_eq!(
        new.reset, 2,
        "issue 1367: the receiver must reattach on the FIRST post-mismatch reset (reset 2), not \
         after another ladder cycle:\n{trace}"
    );
    assert_eq!(
        (new.mismatches, new.retargets, new.by_name_resets),
        (1, 1, 0),
        "issue 1367: exactly one mismatch, one retarget and no BY-NAME round trip:\n{trace}"
    );
    assert!(
        new.t_ms < 1_000,
        "issue 1367: reattach must take well under a second of ladder time, took {} ms:\n{trace}",
        new.t_ms
    );
    assert_eq!(
        new.stale_binds_after_mismatch, 0,
        "issue 1367: the proven-stale record was bound again after the mismatch:\n{trace}"
    );
    assert_eq!(
        new.excluded_after, "-",
        "issue 1367: a verified identity must clear the exclusion:\n{trace}"
    );

    // The SAME replay with the old wiring reproduces the live loop: it attaches only after the
    // stale record aged out, after several mismatch cycles (live: 6 mismatches, ~55 s).
    let old = outcome(&all, "observed", true);
    assert!(
        old.attached && old.t_ms >= 55_000 && old.mismatches >= 5,
        "issue 1367: the legacy replay must reproduce the live ~55 s / 6-mismatch loop, else the \
         model is not faithful to the observed log — got {old:?}:\n{trace}"
    );
}

#[test]
fn replay_stale_record_listed_first_everywhere_still_reattaches_once() {
    let (trace, all) = run_model();
    let trace = trace.join("\n");
    let o = outcome(&all, "stale_first", false);
    assert!(
        o.attached && !o.wrong_sender && o.reset == 2 && o.by_name_resets == 0,
        "issue 1367: with the stale record first in EVERY later finder (incl. the verify), the \
         exclusion must still land the retarget and verify it on reset 2 — got {o:?}:\n{trace}"
    );
}

#[test]
fn replay_live_record_before_the_sender_delivers() {
    let (trace, all) = run_model();
    let trace = trace.join("\n");
    let o = outcome(&all, "not_yet_delivering", false);
    assert!(
        o.attached && !o.wrong_sender,
        "issue 1367: never reattached:\n{trace}"
    );
    assert_eq!(
        o.stale_binds_after_mismatch, 0,
        "issue 1367: a later fresh-finder pass picked the proven-stale record again:\n{trace}"
    );
    assert!(
        o.t_ms < 60_000 && o.t_ms - 12_000 <= 15_000,
        "issue 1367: must reattach within 15 s of the sender delivering (12 s) and before the stale \
         record ages out (60 s) — took {} ms:\n{trace}",
        o.t_ms
    );
}

#[test]
fn a_wrong_retarget_falls_back_to_by_name_never_chains() {
    let (trace, all) = run_model();
    let trace = trace.join("\n");
    let o = outcome(&all, "wrong_retarget", false);
    assert_eq!(
        o.chained_retargets, 0,
        "issue 1367: a retarget that mismatched must take the #1180 BY-NAME safety net, never a \
         second retarget in a row:\n{trace}"
    );
    assert!(
        trace.contains(&format!(
            "wrong_retarget reset 2 t_ms=230 RETARGET 10.77.9.201:5981 -> MISMATCH (name maps to {LIVE}) -> BYNAME"
        )),
        "issue 1367: the wrong retarget's mismatch must force BY-NAME:\n{trace}"
    );
    assert!(
        o.attached && !o.wrong_sender,
        "issue 1367: the wrong-retarget episode must still end on our sender — got {o:?}:\n{trace}"
    );
}
