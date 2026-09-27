//! issue 1380 — development programs the stream OBS's own `Development` scene (the production
//! scene `PRO` nested inside it as a scene source), never `PRO` itself; EVENT mode puts `PRO` back.
//!
//! STATIC-ANCHOR tests (the repo's pattern for `recording-e2e.sh` / `rig-mode.sh`, see the
//! project CLAUDE.md GOTCHA on shared textual anchors). The seeder decision, the `switch`
//! skip-if-already-on-program, the nested rendered-input resolution, the bash lib call shapes and
//! the EVENT-contract item are exercised as LOGIC in
//! `tests/python/test_stream_dev_scene_1380.py` and
//! `tests/python/test_event_assert_stream_program_1380.py`.

use std::fs;
use std::path::PathBuf;

fn read(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// The body of a bash function `name() { ... }` (up to its closing `\n}\n`).
fn fn_body<'a>(s: &'a str, name: &str) -> &'a str {
    let def = s
        .find(&format!("{name}() {{"))
        .unwrap_or_else(|| panic!("{name} must be defined"));
    let end = s[def..].find("\n}\n").map(|i| def + i).unwrap_or(s.len());
    &s[def..end]
}

#[test]
fn the_lib_declares_the_two_scene_names_once() {
    let lib = read("scripts/lib/stream-dev-scene.sh");
    assert!(lib.contains("\nSTREAM_DEV_SCENE_DEFAULT=\"Development\"\n"));
    assert!(lib.contains("\nSTREAM_PRODUCTION_SCENE_DEFAULT=\"PRO\"\n"));
    assert!(lib.contains("stream_dev_scene_ensure() {"));
    assert!(lib.contains("stream_program_scene_read() {"));
}

#[test]
fn recording_e2e_records_the_development_scene_seeded_before_4_8() {
    let s = read("scripts/recording-e2e.sh");
    assert!(
        s.contains(". \"$HERE/lib/stream-dev-scene.sh\""),
        "recording-e2e.sh must source the stream development-scene lib"
    );
    assert!(
        s.contains(r#"STREAM_PROG_SCENE="${STREAM_PROG_SCENE:-$STREAM_DEV_SCENE_DEFAULT}""#),
        "the E2E stream program scene must default to the development scene"
    );
    assert!(
        !s.contains(":-PRO}"),
        "no stream program default may name the production scene PRO"
    );
    let seed = s
        .find("stream_dev_scene_ensure \"$HERE\" \"$STREAM\"")
        .expect("recording-e2e.sh must seed the development scene on the stream box");
    let strih_route = s
        .find("STRIH_OUT=$(python3 \"$HERE/obs_phase2.py\" prod-scene")
        .expect("the [4/8] strih prod-scene route must still exist");
    let stream_route = s
        .find("_stream_prod_scene_args=(prod-scene --host \"$STREAM\"")
        .expect("the [4/8] stream prod-scene route must still exist");
    assert!(
        seed < strih_route && seed < stream_route,
        "the seeder must run before [4/8] touches any program scene"
    );
    let seed_line = &s[seed..seed + s[seed..].find('\n').unwrap()];
    assert!(
        seed_line.contains("\"$STREAM_PROG_SCENE\"")
            && seed_line.contains("\"$STREAM_PRODUCTION_SCENE_DEFAULT\""),
        "the seeder must nest the production scene in $STREAM_PROG_SCENE: {seed_line}"
    );
}

#[test]
fn rig_mode_programs_only_the_development_scene_on_stream() {
    let s = read("scripts/rig-mode.sh");
    assert!(s.contains(". \"$RIG_MODE_DIR/lib/stream-dev-scene.sh\""));
    assert!(s.contains(r#"STREAM_PROG_SCENE="${STREAM_PROG_SCENE:-$STREAM_DEV_SCENE_DEFAULT}""#));
    assert!(!s.contains(":-PRO}"));
    // Owner hard rule 27.9.2026: our tooling never programs PRO -- no EVENT restore, no variable
    // that names the production scene as a program target.
    assert!(!s.contains("STREAM_EVENT_SCENE"));
    assert!(!s.contains("restore_stream_program_production"));
    // every stream `switch` targets the development scene
    let n_stream_switches = s
        .matches("obs_phase2.py\" switch --host \"$STREAM_IP\" --program-scene ")
        .count();
    let n_dev_switches = s
        .matches(
            "obs_phase2.py\" switch --host \"$STREAM_IP\" --program-scene \"$STREAM_PROG_SCENE\"",
        )
        .count();
    assert_eq!(n_stream_switches, n_dev_switches);
    assert!(n_dev_switches >= 1);
}

#[test]
fn rig_mode_test_never_recreates_the_stream_probe_scene_or_input() {
    let s = read("scripts/rig-mode.sh");
    assert!(
        !s.contains("obs_phase2.py\" setup"),
        "rig-mode.sh must not run obs_phase2.py setup (it re-creates PHASE2-PROBE and \
         phase2-probe-src on the stream box, which the owner removed on 27.9.2026)"
    );
    assert!(!s.contains("STREAM_PROBE_UPSTREAM"));
    assert!(!s.contains("--program-scene \"PHASE2-PROBE\""));
}

#[test]
fn rig_mode_test_seeds_then_switches_to_the_development_scene() {
    let s = read("scripts/rig-mode.sh");
    let body = fn_body(&s, "verify_stream_program_dev");
    let seed = body
        .find("stream_dev_scene_ensure \"$here\" \"$STREAM_IP\"")
        .expect("verify_stream_program_dev must seed the development scene");
    let switch = body
        .find("obs_phase2.py\" switch --host \"$STREAM_IP\" --program-scene \"$STREAM_PROG_SCENE\"")
        .expect("verify_stream_program_dev must switch the stream program to $STREAM_PROG_SCENE");
    assert!(seed < switch, "the seeder must run before the switch");
    assert!(body.contains("\"$STREAM_PRODUCTION_SCENE_DEFAULT\""));
    // The development program is production content (the prod scene nested), so gap 2 and the
    // park prove it with the SAME #677 floor prod-scene uses, not the #312 bright-QR floor.
    assert!(
        body.contains("--prod-floor"),
        "gap 2 must use the prod floor: {body}"
    );
    let park = fn_body(&s, "park_stream_program_dev");
    assert!(
        park.contains("--prod-floor"),
        "the park must use the prod floor: {park}"
    );
}

#[test]
fn rig_mode_event_never_switches_the_stream_program() {
    let s = read("scripts/rig-mode.sh");
    let ev = fn_body(&s, "do_event");
    assert!(
        !ev.contains("--program-scene") && !ev.contains("switch --host \"$STREAM_IP\""),
        "EVENT mode must not touch the stream program at all (the owner cuts to PRO himself)"
    );
    assert!(!ev.contains("_stream_event_rc"));
}

#[test]
fn rig_mode_event_contract_reports_the_stream_program_scene_only() {
    let s = read("scripts/rig-mode.sh");
    let body = fn_body(&s, "event_mode_assert");
    assert!(body.contains("stream_program_scene_read \"$here\" \"$STREAM_IP\""));
    assert!(body.contains("report-only"));
    assert!(!body.contains("--arg stream_program_scene"));
    assert!(!body.contains("stream_dev_scene"));
    assert!(!body.contains("stream_production_scene"));
}

#[test]
fn the_rules_quote_the_owner_hard_rule() {
    for rel in [
        ".claude/rules/stream-development-scene.md",
        ".claude/rules/strih-autorecord-coupling.md",
        ".claude/rules/event-assert-none-tolerance.md",
    ] {
        let r = read(rel);
        assert!(
            r.contains("nemas ti nikdy v stream obs davat do programu scenu PRO"),
            "{rel} must quote the owner's hard rule verbatim"
        );
    }
}

#[test]
fn autorecord_rule_states_development_does_not_arm_companion() {
    let r = read(".claude/rules/strih-autorecord-coupling.md");
    assert!(r.contains("issue 1380"));
    assert!(r.contains("`Development`"));
}
