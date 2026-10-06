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
//! `windows-genlock*.yml`), and (B) LIFTS the pure helper blocks VERBATIM and runs truth tables over
//! them. (C), the sequence replay of sender port moves through a C model whose every decision is a
//! SHIPPED helper, lives in `tests/distroav_stale_duplicate_replay_1367.rs`; the shared lift helpers
//! in `tests/support/ndi_source_lift_1367.rs`. The live cure is confirmed only by the supervisor's
//! post-deploy SongPlayer restart. The lift-compile FAILS LOUDLY without a C compiler, never skips.

#[allow(dead_code)]
#[path = "support/ndi_source_lift_1367.rs"]
mod lift;
use lift::*;

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
