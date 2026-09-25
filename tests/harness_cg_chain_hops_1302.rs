//! Issue 1302 slice 2 — the CG_CHAIN=1 wiring that lets the strih/stream `cg_chain` hops be judged:
//! the `--cg-chain-burns` flag on the strih/stream `--extract-partial` calls (and the merge), and the
//! recorded CG window fed to the merge as `--cg-window`.
//!
//! Tier-0 (no rig, no network): the lib's pure builders are called directly under the caller's
//! real `set -euo pipefail`, and `scripts/recording-e2e.sh` is checked by substring anchors only.
//! CG_CHAIN unset must leave every argv byte-identical to before.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_script() -> PathBuf {
    manifest_dir().join("scripts/lib/cg-chain-e2e.sh")
}

fn recording_e2e_text() -> String {
    let p = manifest_dir().join("scripts/recording-e2e.sh");
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Source the lib under `set -euo pipefail` and run `snippet`. Returns (exit_ok, stdout, stderr).
fn run(snippet: &str) -> (bool, String, String) {
    let script = format!(
        "set -euo pipefail\n. \"{}\"\n{}",
        lib_script().display(),
        snippet
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run bash");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    )
}

#[test]
fn extract_flag_is_empty_unless_cg_chain_is_on() {
    for env in ["unset CG_CHAIN", "CG_CHAIN=0", "CG_CHAIN="] {
        let (ok, out, err) = run(&format!(
            "{env}\nf=\"$(cg_chain_extract_burn_flag)\"\nprintf '[%s]' \"$f\""
        ));
        assert!(ok, "{env}: {err}");
        assert_eq!(out, "[]", "{env}: no flag on a normal run");
    }
    let (ok, out, err) = run("CG_CHAIN=1\nprintf '[%s]' \"$(cg_chain_extract_burn_flag)\"");
    assert!(ok, "{err}");
    assert_eq!(out, "[--cg-chain-burns]");
}

#[test]
fn extract_flag_expansion_adds_no_argv_word_on_a_normal_run() {
    // The harness splices the flag as ${CG_CHAIN_BURN_FLAG:+"$CG_CHAIN_BURN_FLAG"}: an empty flag
    // must add NO argv word (byte-identical extract command), a set flag exactly one.
    let snippet = "CG_CHAIN_BURN_FLAG=\"$(cg_chain_extract_burn_flag)\"\n\
                   set -- a ${CG_CHAIN_BURN_FLAG:+\"$CG_CHAIN_BURN_FLAG\"} b\n\
                   printf '%s|' \"$#\" \"$@\"";
    let (ok, out, err) = run(&format!("unset CG_CHAIN\n{snippet}"));
    assert!(ok, "{err}");
    assert_eq!(out, "2|a|b|");
    let (ok, out, err) = run(&format!("CG_CHAIN=1\n{snippet}"));
    assert!(ok, "{err}");
    assert_eq!(out, "3|a|--cg-chain-burns|b|");
}

#[test]
fn merge_args_are_untouched_unless_cg_chain_is_on() {
    let (ok, out, err) = run(
        "unset CG_CHAIN\nMERGE_ARGS=(--merge-partials x)\ncg_chain_merge_args_append\n\
         printf '%s|' \"${MERGE_ARGS[@]}\"",
    );
    assert!(ok, "{err}");
    assert_eq!(out, "--merge-partials|x|");
}

#[test]
fn merge_args_carry_the_flag_and_the_run_window_when_it_exists() {
    let dir = std::env::temp_dir().join(format!("cg-hops-1302-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("mkdir");
    let d = dir.display();
    // No window recorded this run: only the burn flag.
    let (ok, out, err) = run(&format!(
        "CG_CHAIN=1 CG_CHAIN_STATE_DIR='{d}' RUN_ID=77\nMERGE_ARGS=(a)\ncg_chain_merge_args_append\n\
         printf '%s|' \"${{MERGE_ARGS[@]}}\""
    ));
    assert!(ok, "{err}");
    assert_eq!(out, "a|--cg-chain-burns|");
    // The window this run recorded (cg_chain_window_file) is fed as --cg-window.
    fs::write(dir.join("cg-window-77.json"), "{}").expect("write window");
    let (ok, out, err) = run(&format!(
        "CG_CHAIN=1 CG_CHAIN_STATE_DIR='{d}' RUN_ID=77\nMERGE_ARGS=(a)\ncg_chain_merge_args_append\n\
         printf '%s|' \"${{MERGE_ARGS[@]}}\""
    ));
    assert!(ok, "{err}");
    assert_eq!(
        out,
        format!("a|--cg-chain-burns|--cg-window|{d}/cg-window-77.json|")
    );
    // Another run's window is never picked up (the RUN_ID key).
    let (ok, out, err) = run(&format!(
        "CG_CHAIN=1 CG_CHAIN_STATE_DIR='{d}' RUN_ID=78\nMERGE_ARGS=(a)\ncg_chain_merge_args_append\n\
         printf '%s|' \"${{MERGE_ARGS[@]}}\""
    ));
    assert!(ok, "{err}");
    assert_eq!(out, "a|--cg-chain-burns|");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn recording_e2e_threads_the_flag_into_all_three_extract_calls() {
    let s = recording_e2e_text();
    assert_eq!(
        s.matches("CG_CHAIN_BURN_FLAG=\"$(cg_chain_extract_burn_flag)\"")
            .count(),
        1,
        "the flag is resolved once"
    );
    assert_eq!(
        s.matches("${CG_CHAIN_BURN_FLAG:+\"$CG_CHAIN_BURN_FLAG\"}")
            .count(),
        3,
        "strih-lx, Windows strih and stream extract calls each carry the flag"
    );
    let set_at = s
        .find("CG_CHAIN_BURN_FLAG=\"$(cg_chain_extract_burn_flag)\"")
        .expect("flag resolved");
    let first_use = s
        .find("${CG_CHAIN_BURN_FLAG:+\"$CG_CHAIN_BURN_FLAG\"}")
        .expect("flag used");
    assert!(set_at < first_use, "resolved before the extract calls");
}

#[test]
fn recording_e2e_appends_the_cg_merge_args_before_the_merge_runs() {
    let s = recording_e2e_text();
    let cg_arg = s
        .find("MERGE_ARGS+=(--cg \"$CG_RECORDING\")")
        .expect("the --cg merge arg");
    let append = s
        .find("cg_chain_merge_args_append")
        .expect("the cg merge args append");
    let printed = s
        .find("printf '      %q ' \"$VERDICT_BIN\" \"${MERGE_ARGS[@]}\"")
        .expect("the merge command print");
    assert!(
        cg_arg < append && append < printed,
        "appended after --cg, before the merge"
    );
}
