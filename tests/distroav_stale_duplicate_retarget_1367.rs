//! Issue 1367 slice — a genlock DistroAV receiver must reattach in SECONDS after a sender moves its
//! NDI port, and never lock onto another sender. Live (cg OBS, 6.10.2026 04:04:50–04:05:46): after
//! SongPlayer's restart the NDI finder held TWO records for `RESOLUME-SNV (SP-program)` on
//! 10.77.9.201 — the stale `:5961` (now another live sender's port) FIRST and the live `:5971`
//! second. The first-match pick bound `:5961`, the #1180 verify forced BY-NAME, the SDK name resolver
//! followed the same stale record, and the ladder looped ~55 s until the record aged out.
//!
//! The fix (`vendor/distroav/src/ndi-source.cpp`, decision 6009040469 = Approach 1b of design
//! 6008662821) decides on EVIDENCE, never on the order a finder lists records in. Two live senders
//! cannot share one URL, so a record of our name whose URL another name also advertises is stale:
//! - the picker (both pick sites) skips excluded URLs and such CONTESTED records; none left = NULL
//!   (the existing ladder / BY-NAME);
//! - the verify is duplicate-aware: VERIFIED when the bound URL is one of the name's uncontested
//!   records in ANY position; STALE when another name advertises it; MISMATCH when it is not among
//!   the name's records or is excluded; INCONCLUSIVE when the name is not discoverable;
//! - only a STALE bind is excluded and retargeted BY-URL to the PICKER's choice from the verify
//!   list (BY-NAME when none remains); any other mismatch takes the unchanged #1180 BY-NAME path;
//! - two exclusion slots, each 120 s, all cleared by a bind verified with frames; a frame-less bind
//!   is never excluded (#1287 owns it);
//! - the state is one fixed-buffer struct with pure helpers, so `ndi_source_thread` does not grow.
//!
//! Why std-only + offline: Tier-0 (no local cargo compile) and the vendored C++ compiles only on CI,
//! so per `.claude/rules/distroav-receiver-lifecycle.md` this file (A) SOURCE-ANCHORS the wiring
//! (revert protection against a `git subtree pull`; mirrored as pwsh checks in BOTH
//! `windows-genlock*.yml`), (B) LIFTS the pure helper blocks VERBATIM and runs truth tables over
//! them, and (C) REPLAYS sender port moves through a C model of the reset → bind → verify loop
//! (`tests/c/distroav_stale_duplicate_model_1367.c`) whose every decision is a SHIPPED helper. No
//! scenario may accept a wrong sender; the legacy replay must reproduce the live ~55 s loop. The live
//! cure is confirmed only by the supervisor's post-deploy SongPlayer restart. The lift-compile FAILS
//! LOUDLY without a C compiler, never skips.

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

/// The body of the function whose definition starts with `signature`, to its first `\n}\n`.
fn body_of(src: &str, signature: &str) -> String {
    let start = index_of(src, signature, "function definition");
    let end = src[start..]
        .find("\n}\n")
        .map(|i| start + i + 3)
        .unwrap_or_else(|| panic!("issue 1367: `{signature}` has no closing brace `\\n}}\\n`"));
    src[start..end].to_string()
}

// ----------------------------------------------------------------------------------------------
// Facet A — source anchors.
// ----------------------------------------------------------------------------------------------

#[test]
fn helpers_and_state_are_present() {
    let src = squish(&repo_file(NDI_SOURCE));
    for (needle, why) in [
        (
            "static inline const char *ndi_find_url_for_source_name(const char *requested_name, const NDIlib_source_t *sources, uint32_t n_sources, const char *exclude_a, const char *exclude_b)",
            "The picker must take the two exclusion slots.",
        ),
        (
            "static inline bool ndi_url_contested_1367(",
            "The contested-record evidence check is gone.",
        ),
        (
            "static inline int ndi_identity_verdict_1367(",
            "The duplicate-aware verdict is gone.",
        ),
        (
            "struct ndi_stale_state_1367 { char excluded[NDI_URL_SLOTS_1367][NDI_URL_SLOT_LEN_1367];",
            "The stale-duplicate state struct is gone.",
        ),
        (
            "#define NDI_URL_SLOTS_1367 2",
            "Two exclusion slots (decision 6009040469).",
        ),
        (
            "static const uint64_t NDI_URL_EXCLUDE_TTL_NS = 120ULL * 1000ULL * 1000ULL * 1000ULL;",
            "Each exclusion is bounded to 120 s.",
        ),
        (
            "static inline int ndi_stale_apply_verdict_1367(",
            "The verdict -> action step is gone.",
        ),
        (
            "/* camera-box #1367 stale-duplicate state: BEGIN",
            "The lifted state block lost its BEGIN marker.",
        ),
        (
            "/* camera-box #1367 stale-duplicate state: END */",
            "The lifted state block lost its END marker.",
        ),
    ] {
        require(&src, needle, why);
    }
    assert!(
        !src.contains("ndi_identity_mismatch_action_1367"),
        "{NDI_SOURCE}: issue 1367 — the order-based Approach-1 action helper is back."
    );
}

#[test]
fn both_pick_sites_skip_excluded_and_contested_records() {
    let src = squish(&repo_file(NDI_SOURCE));
    require(
        &src,
        "ndi_find_url_for_source_name(owned_source_name, fresh_sources, n_fresh, stale_1367.excluded[0], stale_1367.excluded[1])",
        "The reset's fresh finder must pass both exclusion slots.",
    );
    let verify = squish(&body_of(
        &repo_file(NDI_SOURCE),
        "static int ndi_identity_verify_1367(",
    ));
    for needle in [
        "const char *ex0 = ndi_stale_exclusion_1367(st, 0, now); const char *ex1 = ndi_stale_exclusion_1367(st, 1, now);",
        "verdict = ndi_identity_verdict_1367(bound_url, name, v_sources, n_v, ex0, ex1);",
        "const char *pick = ndi_find_url_for_source_name(name, v_sources, n_v, ex0, ex1);",
        "int action = ndi_stale_apply_verdict_1367(st, verdict, bound_url, *verify_url_out, os_gettime_ns());",
        "ndiLib->find_destroy(verify_finder);",
    ] {
        require(
            &verify,
            needle,
            "The verify helper must decide on the duplicate-aware verdict, pick with the exclusions, \
             and apply the verdict to the state.",
        );
    }
}

#[test]
fn reset_block_binds_the_retarget_before_the_fresh_finder() {
    let src = squish(&repo_file(NDI_SOURCE));
    require(
        &src,
        "if (ndi_stale_reset_1367(&stale_1367, obs_source_name, force_by_name_1180, &owned_source_url)) { url_resolved_1096 = true; url_bind_kind_1096 = 3; } else if (!force_by_name_1180 && owned_source_name && owned_source_name[0]) {",
        "A verified retarget must bind BY-URL (umbrella url_resolved_1096, so #1180/#1287 still apply) \
         ahead of the fresh finder.",
    );
    let consume = index_of(
        &src,
        "ndi_stale_reset_1367(&stale_1367,",
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
    let step = squish(&body_of(
        &repo_file(NDI_SOURCE),
        "static bool ndi_stale_reset_1367(",
    ));
    for needle in [
        "unsigned expired = ndi_stale_begin_reset_1367(st, os_gettime_ns(), retarget);",
        "if (force_by_name || !retarget[0]) return false;",
        "st->bound_via_retarget = true;",
        "#1367 retarget BY-URL '%s' (verified; excluding '%s')",
        "#1367 exclusion of '%s' expired after",
    ] {
        require(
            &step,
            needle,
            "The reset step must expire, consume, and bind + log the retarget unless forced BY-NAME.",
        );
    }
    require(
        &src,
        "else if (url_bind_kind_1096 == 0) // 3 = the #1367 retarget, logged by its step obs_log(LOG_INFO, \"'%s' ndi_source_thread: reset_ndi_receiver: #1096 connect BY-URL '%s' (fresh finder; bypassing poisoned name resolver)\",",
        "A retarget bind must not also print the fresh-finder line.",
    );
    require(
        &src,
        "ndi_stale_forget_on_rename_1367(&stale_1367, owned_source_name, s->config.ndi_source_name); bfree(owned_source_name);",
        "A configured-name change must drop the stale-duplicate state.",
    );
    let keep = "ndi_stale_keep_retarget_1367(&stale_1367, owned_source_url);";
    let n = src.matches(keep).count();
    assert_eq!(
        n, 2,
        "{NDI_SOURCE}: issue 1367 — a retarget bind whose recv_create / framesync create fails must \
         keep its verified URL for the retry (found {n} of the 2 failure branches)."
    );
}

#[test]
fn verify_runs_in_the_helper_and_keeps_the_1180_by_name_path() {
    let src = squish(&repo_file(NDI_SOURCE));
    require(
        &src,
        "int verdict_1367 = ndi_identity_verify_1367(s, owned_source_name, owned_source_url, &stale_1367, &last_delivered_url_1096, &verify_url_1180, &was_disconnected, &force_by_name_next_reset_1180); if (verdict_1367 == NDI_VERIFY_STALE_1367) { bfree(verify_url_1180); continue; } bool mismatch_1180 = verdict_1367 == NDI_VERIFY_MISMATCH_1367; if (mismatch_1180) { obs_log(LOG_WARNING, \"genlock: #1180 BY-URL identity MISMATCH '%s' -- configured name now maps to '%s' but the receiver is bound to '%s'; forcing a fresh BY-NAME reset (sender NDI port reshuffle after an OBS restart?)\",",
        "The thread must run the verify helper, leave a STALE bind to it, and keep the unchanged \
         #1180 BY-NAME block (its log line byte-identical) for every other mismatch.",
    );
    let verify = squish(&body_of(
        &repo_file(NDI_SOURCE),
        "static int ndi_identity_verify_1367(",
    ));
    for needle in [
        "if (verdict != NDI_VERIFY_STALE_1367) return verdict;",
        "another sender advertises the bound URL, so it is a stale record -- excluding it and %s (#1367)",
        "if (*last_known && strcmp(*last_known, bound_url) == 0) { bfree(*last_known); *last_known = nullptr; }",
        "if (action == NDI_STALE_BY_NAME_1367) *force_by_name_next = true; *was_disconnected = true; pthread_mutex_lock(&s->config_mutex); s->config.reset_ndi_receiver = true; pthread_mutex_unlock(&s->config_mutex); return verdict;",
        "#1367 identity verified on '%s' -- cleared %u exclusion(s)",
        "for (unsigned w = 0; w < NDI_IDENTITY_VERIFY_MAX_WAITS && s->running; ++w) {",
    ] {
        require(
            &verify,
            needle,
            "The verify helper must handle a STALE bind completely: log, drop the last-known URL, force \
             BY-NAME when nothing uncontested remains, and arm the reset.",
        );
    }
}

#[test]
fn the_thread_keeps_the_state_in_one_struct() {
    let raw = repo_file(NDI_SOURCE);
    let thread = body_of(&raw, "void *ndi_source_thread(void *data)\n{");
    let squished = squish(&thread);
    require(
        &squished,
        "struct ndi_stale_state_1367 stale_1367 = {};",
        "The thread holds ONE stale-duplicate state struct.",
    );
    for gone in [
        "retarget_url_1367",
        "excluded_url_1367",
        "excluded_since_ns_1367",
        "bound_via_retarget_1367",
        "char *retarget_1367",
    ] {
        assert!(
            !squished.contains(gone),
            "{NDI_SOURCE}: issue 1367 — `{gone}` is a loose thread local again; the state belongs in \
             struct ndi_stale_state_1367 (decision 6009040469)."
        );
    }
    let finders = squished.matches("ndiLib->find_create_v2(").count();
    assert_eq!(
        finders, 1,
        "{NDI_SOURCE}: issue 1367 — ndi_source_thread must create only the reset's fresh finder; the \
         verify finder lives in ndi_identity_verify_1367 (found {finders})."
    );
}

#[test]
fn new_log_lines_are_distinct_from_existing_markers() {
    let ours = [
        "#1367 retarget BY-URL '%s' (verified; excluding '%s')",
        "#1367 exclusion of '%s' expired after",
        "#1367 identity verified on '%s' -- cleared %u exclusion(s)",
        "another sender advertises the bound URL, so it is a stale record -- excluding it and %s (#1367)",
    ];
    let existing = [
        "#1096 connect BY-URL '%s' (fresh finder; bypassing poisoned name resolver)",
        "#1096 rebind BY-URL '%s' (last-known good; fresh finder resolved none)",
        "#1096 rebind BY-URL '%s' (fleet map after finder-blind cycles",
        "#1180 connect BY-NAME '%s'",
        "#1096 connect BY-NAME '%s'",
        "forcing a fresh BY-NAME reset (sender NDI port reshuffle after an OBS restart?)",
    ];
    let src = repo_file(NDI_SOURCE);
    for e in existing {
        assert!(
            src.contains(e),
            "{NDI_SOURCE}: issue 1367 changed the existing log text `{e}`"
        );
    }
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
// Facet B — lift the pure helper blocks VERBATIM, compile under -Werror -Wconversion -Wformat=2, run
// truth tables. Nothing in Rust consumes them, so the truth tables ARE the spec.
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

/// The contiguous verdict block VERBATIM: the verdict enum, the contested check, the picker, the
/// #1180 equality helper and the duplicate-aware verdict.
fn lift_verdict_block() -> String {
    let src = repo_file(NDI_SOURCE);
    let start = index_of(&src, "enum ndi_identity_verdict_1367 {", "the verdict enum");
    let sig = index_of(
        &src,
        "static inline int ndi_identity_verdict_1367(",
        "the verdict helper",
    );
    assert!(
        start < sig,
        "issue 1367: the verdict enum must precede the verdict helper"
    );
    let end = src[sig..]
        .find("\n}\n")
        .map(|i| sig + i + 3)
        .expect("issue 1367: the verdict helper has no closing brace");
    src[start..end].to_string()
}

/// The state block VERBATIM, between its BEGIN and END markers.
fn lift_state_block() -> String {
    let src = repo_file(NDI_SOURCE);
    let start = index_of(
        &src,
        "/* camera-box #1367 stale-duplicate state: BEGIN",
        "the state block BEGIN marker",
    );
    let end_marker = "/* camera-box #1367 stale-duplicate state: END */";
    let end = index_of(&src, end_marker, "the state block END marker") + end_marker.len();
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
const CGOBS: &str = "RESOLUME-SNV (cg-obs)";
const URL_A: &str = "10.77.9.201:5961";
const URL_B: &str = "10.77.9.201:5971";
const URL_C: &str = "10.77.9.201:5981";

fn c_str(v: Option<&str>) -> String {
    match v {
        Some(s) => format!("\"{s}\""),
        None => "NULL".to_string(),
    }
}

/// A finder list `(name, url)` as a C array declaration named `arr{i}` (or a NULL pointer + 0).
fn c_list(i: usize, list: &[(Option<&str>, Option<&str>)], c: &mut String) -> (String, String) {
    if list.is_empty() {
        return ("(const NDIlib_source_t *)0".to_string(), "0u".to_string());
    }
    let cells: Vec<String> = list
        .iter()
        .map(|(n, u)| format!("{{ {}, {} }}", c_str(*n), c_str(*u)))
        .collect();
    c.push_str(&format!(
        "        NDIlib_source_t arr{i}[] = {{ {} }};\n",
        cells.join(", ")
    ));
    (format!("arr{i}"), format!("{}u", list.len()))
}

type Rec = (Option<&'static str>, Option<&'static str>);

/// One picker vector: requested name, finder list, the two exclusions, expected pick, why.
type PickRow = (
    Option<&'static str>,
    Vec<Rec>,
    Option<&'static str>,
    Option<&'static str>,
    Option<&'static str>,
    &'static str,
);

fn picker_vectors() -> Vec<PickRow> {
    let n = Some(NAME);
    let a = Some(URL_A);
    let b = Some(URL_B);
    vec![
        (
            n,
            vec![(n, a), (n, b)],
            None,
            None,
            a,
            "no evidence, no exclusion -> today's first match",
        ),
        (
            n,
            vec![(n, a), (n, b), (Some(CGOBS), a)],
            None,
            None,
            b,
            "THE INCIDENT: A is contested by cg-obs -> B",
        ),
        (
            n,
            vec![(Some(CGOBS), a), (n, a), (n, b)],
            None,
            None,
            b,
            "the contest is found wherever it is listed",
        ),
        (
            n,
            vec![(n, b), (n, a), (Some(CGOBS), b)],
            None,
            None,
            a,
            "contest skips by URL, not by position",
        ),
        (
            n,
            vec![(n, a), (n, b)],
            a,
            None,
            b,
            "slot A excluded -> the next record",
        ),
        (
            n,
            vec![(n, a), (n, b)],
            None,
            a,
            b,
            "the SECOND slot excludes too",
        ),
        (
            n,
            vec![(n, a), (n, b)],
            a,
            b,
            None,
            "both records excluded -> NULL (the ladder decides)",
        ),
        (
            n,
            vec![(n, a), (Some(CGOBS), a)],
            None,
            None,
            None,
            "the only record is contested -> NULL, never a wrong bind",
        ),
        (
            n,
            vec![(n, a)],
            a,
            None,
            None,
            "the only record is excluded -> NULL",
        ),
        (
            n,
            vec![(n, a), (n, b)],
            Some(""),
            Some(""),
            a,
            "EMPTY exclusions = none",
        ),
        (
            n,
            vec![(n, a), (n, b)],
            Some("10.77.9.201:596"),
            None,
            a,
            "exact URL compare, never a prefix",
        ),
        (
            n,
            vec![(n, a), (None, a), (n, b)],
            None,
            None,
            a,
            "a nameless record is no evidence",
        ),
        (
            n,
            vec![(n, a), (Some(""), a), (n, b)],
            None,
            None,
            a,
            "an empty-named record is no evidence",
        ),
        (
            n,
            vec![(n, a), (n, Some(""))],
            a,
            None,
            None,
            "next record has no address -> NULL (fall back to name)",
        ),
        (
            None,
            vec![(n, a)],
            None,
            None,
            None,
            "NULL requested name -> NULL",
        ),
        (n, vec![], None, None, None, "empty finder list -> NULL"),
    ]
}

#[test]
fn picker_skips_excluded_and_contested_records_truth_table() {
    let vs = picker_vectors();
    let mut c = String::from(PRELUDE);
    c.push_str(&lift_verdict_block());
    c.push_str("\nint main(void){\n");
    for (i, (name, list, ex_a, ex_b, _, _)) in vs.iter().enumerate() {
        c.push_str("    {\n");
        let (arr, count) = c_list(i, list, &mut c);
        c.push_str(&format!(
            "        const char *r = ndi_find_url_for_source_name({}, {arr}, {count}, {}, {});\n",
            c_str(*name),
            c_str(*ex_a),
            c_str(*ex_b)
        ));
        c.push_str("        printf(\"%s\\n\", r ? r : \"__NULL__\");\n    }\n");
    }
    c.push_str("    return 0;\n}\n");
    let got = compile_and_run(&c, "picker");
    assert_eq!(got.len(), vs.len(), "issue 1367: picker printed {got:?}");
    let diffs: Vec<String> = vs
        .iter()
        .zip(&got)
        .filter(|((_, _, _, _, want, _), g)| g.as_str() != want.unwrap_or("__NULL__"))
        .map(|((name, list, ex_a, ex_b, want, why), g)| {
            format!("  name={name:?} list={list:?} ex={ex_a:?}/{ex_b:?} -> {g:?}, expected {want:?} [{why}]")
        })
        .collect();
    assert!(
        diffs.is_empty(),
        "issue 1367: ndi_find_url_for_source_name DIVERGED from the evidence-based pick spec:\n{}",
        diffs.join("\n")
    );
}

/// One verdict vector: bound URL, name, finder list, the two exclusions, expected verdict, why.
type VerdictRow = (
    Option<&'static str>,
    Option<&'static str>,
    Vec<Rec>,
    Option<&'static str>,
    Option<&'static str>,
    &'static str,
    &'static str,
);

fn verdict_vectors() -> Vec<VerdictRow> {
    let n = Some(NAME);
    let cg = Some(CGOBS);
    let (a, b, cc) = (Some(URL_A), Some(URL_B), Some(URL_C));
    vec![
        (
            b,
            n,
            vec![(n, a), (n, b)],
            None,
            None,
            "VERIFIED",
            "THE REVERSED ORDERING: the stale record listed first keeps a correct bind",
        ),
        (
            b,
            n,
            vec![(n, b), (n, a)],
            None,
            None,
            "VERIFIED",
            "the bound URL listed first",
        ),
        (
            a,
            n,
            vec![(n, a), (n, b), (cg, a)],
            None,
            None,
            "STALE",
            "THE INCIDENT: another name advertises the bound URL",
        ),
        (
            a,
            n,
            vec![(n, b), (cg, a)],
            None,
            None,
            "STALE",
            "our stale record aged out, the new owner still advertises",
        ),
        (
            b,
            n,
            vec![(n, a), (n, b), (cg, b)],
            None,
            None,
            "STALE",
            "contested at any position",
        ),
        (
            a,
            n,
            vec![(n, a), (cg, a)],
            a,
            None,
            "STALE",
            "contested beats excluded (proven stale again)",
        ),
        (
            a,
            n,
            vec![(n, a), (n, b)],
            a,
            None,
            "MISMATCH",
            "an excluded bound URL (slot A)",
        ),
        (
            a,
            n,
            vec![(n, a), (n, b)],
            None,
            a,
            "MISMATCH",
            "an excluded bound URL (slot B)",
        ),
        (
            cc,
            n,
            vec![(n, a), (n, b)],
            None,
            None,
            "MISMATCH",
            "the bound URL is not among the name's records",
        ),
        (
            a,
            n,
            vec![(cg, a)],
            None,
            None,
            "INCONCLUSIVE",
            "the name is not discoverable -> keep the feed",
        ),
        (
            a,
            n,
            vec![(n, Some(""))],
            None,
            None,
            "INCONCLUSIVE",
            "the name has no record with an address",
        ),
        (
            a,
            n,
            vec![],
            None,
            None,
            "INCONCLUSIVE",
            "empty finder list",
        ),
        (
            Some(""),
            n,
            vec![(n, a)],
            None,
            None,
            "INCONCLUSIVE",
            "not a BY-URL bind",
        ),
        (
            None,
            n,
            vec![(n, a)],
            None,
            None,
            "INCONCLUSIVE",
            "NULL bound URL",
        ),
        (
            a,
            None,
            vec![(n, a)],
            None,
            None,
            "INCONCLUSIVE",
            "NULL configured name",
        ),
        (
            a,
            n,
            vec![(n, a), (None, a)],
            None,
            None,
            "VERIFIED",
            "a nameless record is no evidence",
        ),
        (
            a,
            n,
            vec![(n, Some("")), (n, a)],
            None,
            None,
            "VERIFIED",
            "an address-less record is skipped",
        ),
    ]
}

#[test]
fn verdict_is_duplicate_aware_truth_table() {
    let vs = verdict_vectors();
    let mut c = String::from(PRELUDE);
    c.push_str(&lift_verdict_block());
    c.push_str(
        "\nstatic const char *vname(int v){ switch (v) { case NDI_VERIFY_INCONCLUSIVE_1367: return \"INCONCLUSIVE\"; \
         case NDI_VERIFY_VERIFIED_1367: return \"VERIFIED\"; case NDI_VERIFY_STALE_1367: return \"STALE\"; \
         case NDI_VERIFY_MISMATCH_1367: return \"MISMATCH\"; default: return \"?\"; } }\n",
    );
    c.push_str("int main(void){\n");
    for (i, (bound, name, list, ex_a, ex_b, _, _)) in vs.iter().enumerate() {
        c.push_str("    {\n");
        let (arr, count) = c_list(i, list, &mut c);
        c.push_str(&format!(
            "        printf(\"%s\\n\", vname(ndi_identity_verdict_1367({}, {}, {arr}, {count}, {}, {})));\n",
            c_str(*bound),
            c_str(*name),
            c_str(*ex_a),
            c_str(*ex_b)
        ));
        c.push_str("    }\n");
    }
    c.push_str("    return 0;\n}\n");
    let got = compile_and_run(&c, "verdict");
    assert_eq!(got.len(), vs.len(), "issue 1367: verdict printed {got:?}");
    let diffs: Vec<String> = vs
        .iter()
        .zip(&got)
        .filter(|((_, _, _, _, _, want, _), g)| g.as_str() != *want)
        .map(|((bound, name, list, ex_a, ex_b, want, why), g)| {
            format!("  bound={bound:?} name={name:?} list={list:?} ex={ex_a:?}/{ex_b:?} -> {g}, expected {want} [{why}]")
        })
        .collect();
    assert!(
        diffs.is_empty(),
        "issue 1367: ndi_identity_verdict_1367 DIVERGED from the decided spec:\n{}",
        diffs.join("\n")
    );
}

/// The state helpers as one scripted sequence: each step prints one line.
const STATE_SCRIPT: &str = r#"
static void show(const char *step, const struct ndi_stale_state_1367 *st, uint64_t now)
{
	printf("%s | e0=%s e1=%s n=%u rt=%s via=%d\n", step,
	       ndi_stale_exclusion_1367(st, 0, now) ? st->excluded[0] : "-",
	       ndi_stale_exclusion_1367(st, 1, now) ? st->excluded[1] : "-",
	       (ndi_stale_exclusion_1367(st, 0, now) ? 1u : 0u) + (ndi_stale_exclusion_1367(st, 1, now) ? 1u : 0u),
	       st->retarget[0] ? st->retarget : "-", st->bound_via_retarget ? 1 : 0);
}

int main(void)
{
	const uint64_t S = 1000000000ULL, T = 1000ULL * S;
	char out[NDI_URL_SLOT_LEN_1367];
	char longurl[NDI_URL_SLOT_LEN_1367 + 8];
	memset(longurl, 'x', sizeof longurl - 1);
	longurl[sizeof longurl - 1] = '\0';
	struct ndi_stale_state_1367 st;
	memset(&st, 0, sizeof st);
	printf("exclude A -> %d\n", ndi_stale_exclude_1367(&st, URL_A, T) ? 1 : 0);
	printf("exclude B -> %d\n", ndi_stale_exclude_1367(&st, URL_B, T + 1 * S) ? 1 : 0);
	show("two slots", &st, T + 2 * S);
	printf("refresh B -> %d\n", ndi_stale_exclude_1367(&st, URL_B, T + 3 * S) ? 1 : 0);
	show("refreshed", &st, T + 3 * S);
	printf("exclude C -> %d\n", ndi_stale_exclude_1367(&st, URL_C, T + 4 * S) ? 1 : 0);
	show("oldest replaced", &st, T + 4 * S);
	printf("newest -> %s\n", ndi_stale_newest_exclusion_1367(&st));
	printf("expired mask -> %u\n", ndi_stale_begin_reset_1367(&st, T + 123 * S, out));
	show("after expiry", &st, T + 123 * S);
	printf("exclude long -> %d\n", ndi_stale_exclude_1367(&st, longurl, T + 123 * S) ? 1 : 0);
	printf("exclude empty -> %d\n", ndi_stale_exclude_1367(&st, "", T + 123 * S) ? 1 : 0);
	printf("clear -> %u\n", ndi_stale_clear_exclusions_1367(&st, T + 123 * S));
	show("cleared", &st, T + 123 * S);
	ndi_stale_exclude_1367(&st, URL_C, T + 130 * S);
	printf("verified -> %d\n", ndi_stale_apply_verdict_1367(&st, NDI_VERIFY_VERIFIED_1367, URL_B, URL_B, T + 131 * S));
	show("verified clears", &st, T + 131 * S);
	printf("stale+pick -> %d\n", ndi_stale_apply_verdict_1367(&st, NDI_VERIFY_STALE_1367, URL_A, URL_B, T + 132 * S));
	show("retarget set", &st, T + 132 * S);
	printf("begin reset mask -> %u out=%s\n", ndi_stale_begin_reset_1367(&st, T + 133 * S, out), out);
	show("retarget consumed", &st, T + 133 * S);
	ndi_stale_keep_retarget_1367(&st, URL_B);
	show("keep without retarget bind", &st, T + 133 * S);
	st.bound_via_retarget = true;
	ndi_stale_keep_retarget_1367(&st, URL_B);
	show("keep on a retarget bind", &st, T + 133 * S);
	ndi_stale_begin_reset_1367(&st, T + 134 * S, out);
	printf("stale no pick -> %d\n", ndi_stale_apply_verdict_1367(&st, NDI_VERIFY_STALE_1367, URL_B, NULL, T + 135 * S));
	show("two stale URLs", &st, T + 135 * S);
	printf("mismatch -> %d\n", ndi_stale_apply_verdict_1367(&st, NDI_VERIFY_MISMATCH_1367, URL_C, URL_A, T + 136 * S));
	show("mismatch excludes nothing", &st, T + 136 * S);
	printf("inconclusive -> %d\n", ndi_stale_apply_verdict_1367(&st, NDI_VERIFY_INCONCLUSIVE_1367, URL_C, NULL, T + 136 * S));
	ndi_stale_forget_on_rename_1367(&st, NAME, NAME);
	show("same name keeps", &st, T + 136 * S);
	ndi_stale_forget_on_rename_1367(&st, NAME, "RESOLUME-SNV (cg-obs)");
	show("renamed drops", &st, T + 136 * S);
	ndi_stale_exclude_1367(&st, URL_A, T + 137 * S);
	ndi_stale_forget_on_rename_1367(&st, NULL, NAME);
	show("first reset drops", &st, T + 137 * S);
	return 0;
}
"#;

#[test]
fn state_helpers_follow_the_decided_slots_truth_table() {
    let mut c = String::from(PRELUDE);
    c.push_str(&format!(
        "#define NAME \"{NAME}\"\n#define URL_A \"{URL_A}\"\n#define URL_B \"{URL_B}\"\n#define URL_C \"{URL_C}\"\n"
    ));
    c.push_str(&lift_verdict_block());
    c.push('\n');
    c.push_str(&lift_state_block());
    c.push('\n');
    c.push_str(STATE_SCRIPT);
    let got = compile_and_run(&c, "state");
    let a = URL_A;
    let b = URL_B;
    let cc = URL_C;
    let want = vec![
        "exclude A -> 1".to_string(),
        "exclude B -> 1".to_string(),
        format!("two slots | e0={a} e1={b} n=2 rt=- via=0"),
        "refresh B -> 1".to_string(),
        format!("refreshed | e0={a} e1={b} n=2 rt=- via=0"),
        "exclude C -> 1".to_string(),
        format!("oldest replaced | e0={cc} e1={b} n=2 rt=- via=0"),
        format!("newest -> {cc}"),
        "expired mask -> 2".to_string(),
        format!("after expiry | e0={cc} e1=- n=1 rt=- via=0"),
        "exclude long -> 0".to_string(),
        "exclude empty -> 0".to_string(),
        "clear -> 1".to_string(),
        "cleared | e0=- e1=- n=0 rt=- via=0".to_string(),
        "verified -> 0".to_string(),
        "verified clears | e0=- e1=- n=0 rt=- via=0".to_string(),
        "stale+pick -> 1".to_string(),
        format!("retarget set | e0={a} e1=- n=1 rt={b} via=0"),
        format!("begin reset mask -> 0 out={b}"),
        format!("retarget consumed | e0={a} e1=- n=1 rt=- via=0"),
        format!("keep without retarget bind | e0={a} e1=- n=1 rt=- via=0"),
        format!("keep on a retarget bind | e0={a} e1=- n=1 rt={b} via=1"),
        "stale no pick -> 2".to_string(),
        format!("two stale URLs | e0={a} e1={b} n=2 rt=- via=0"),
        "mismatch -> 2".to_string(),
        format!("mismatch excludes nothing | e0={a} e1={b} n=2 rt=- via=0"),
        "inconclusive -> 0".to_string(),
        format!("same name keeps | e0={a} e1={b} n=2 rt=- via=0"),
        "renamed drops | e0=- e1=- n=0 rt=- via=0".to_string(),
        "first reset drops | e0=- e1=- n=0 rt=- via=0".to_string(),
    ];
    assert_eq!(
        got, want,
        "issue 1367: the stale-duplicate state helpers DIVERGED from the decided slot/TTL/action spec"
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
// ndi_source_thread (Approach 1b). Each scenario is a list of phases (what each finder lists, which URL our
// sender delivers on, which port is dead); any other URL a bind lands on is ANOTHER live sender.
// `legacy` runs the pre-1367 wiring and must reproduce the live ~55 s loop, so the replay is
// faithful. The model's C text lives in its own file (read at run time, like the sibling tests/c
// harnesses). Decision 6009040469 names the scenarios; no scenario may ever accept a wrong sender.
// ----------------------------------------------------------------------------------------------

/// The replay model: compiled AFTER the lifted helpers, never on its own.
const MODEL_C: &str = "tests/c/distroav_stale_duplicate_model_1367.c";

/// The shipped code the model's wiring calls, lifted verbatim: the #767 stale window, the #1287
/// frame-less rule, the verdict block (enum, contested check, picker, #1180 equality, verdict) and
/// the stale-duplicate state block (TTL, slots, every state helper).
fn model_shipped_code() -> String {
    let mut c = lift_const("static const uint64_t GENLOCK_RECONNECT_STALE_NS");
    c.push('\n');
    c.push_str(&lift_verdict_block());
    c.push('\n');
    c.push_str(&lift_fn(
        "static inline bool ndi_force_by_name_after_frameless(",
    ));
    c.push('\n');
    c.push_str(&lift_state_block());
    c.push('\n');
    c
}

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
    c.push_str(&model_shipped_code());
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
