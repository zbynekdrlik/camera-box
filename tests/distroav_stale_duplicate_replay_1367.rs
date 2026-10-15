//! Issue 1367 slice — the SEQUENCE REPLAY half of the stale-duplicate gate (the anchors and truth
//! tables live in `tests/distroav_stale_duplicate_retarget_1367.rs`; the shared lift helpers in
//! `tests/support/ndi_source_lift_1367.rs`). Sender port moves replay through a C model of the
//! receiver's reset → bind → verify loop (`tests/c/distroav_stale_duplicate_model_1367.c`) whose every
//! decision is a SHIPPED helper lifted verbatim from `vendor/distroav/src/ndi-source.cpp`. Decision
//! 6009040469 names the five scenarios; no scenario may ever accept a wrong sender, and the `legacy`
//! run must reproduce the live ~55 s loop so the model stays faithful. Std-only and offline (Tier-0);
//! the C compile FAILS LOUDLY without a compiler, never skips.

#[allow(dead_code)]
#[path = "support/ndi_source_lift_1367.rs"]
mod lift;
use lift::*;

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
