//! #721 (supervisor-added scope, live 2026-08-16): EVENT mode's item-8 assert correctly FLAGS the
//! leftover marker CSV `/run/rig-qpsk-markers.csv` (= `$AUDIO_MARKER_LOG`, root-owned on tmpfs),
//! but NOTHING on the EVENT path DELETED it -- so the whole EVENT-mode CONTRACT failed
//! ("[CHYBA] testovacie artefakty su vymazane") on that one leftover with the rig otherwise clean.
//!
//! Fix: a PURGE builder `event_artifact_purge_cmds` in `scripts/lib/event-assert.sh` (co-located
//! with its CHECK counterpart `event_assert_artifacts_check_cmds`), wired into `do_event` after the
//! painter/ledger stop so no painter can re-create the CSV. This file proves BOTH halves: the
//! builder emits a real `rm -f` that deletes the paths, AND `do_event` actually calls it with the
//! marker-CSV var, positioned before the item-8 assert phase.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    manifest_dir().join("scripts/lib/event-assert.sh")
}

/// Return the text from `marker` up to (not incl.) the next ')' -- the remaining call arg list.
fn args_after(s: &str, marker: &str) -> Option<String> {
    let start = s.find(marker)? + marker.len();
    let rest = &s[start..];
    let end = rest.find(')')?;
    Some(rest[..end].trim().to_string())
}

struct Run {
    exit_code: i32,
    stdout: String,
}

/// Source the event-assert lib, then run `body` (which may call the new builder). Mirrors
/// `harness_event_assert_wired_722.rs`'s own `run_sourced`.
fn run_sourced(body: &str) -> Run {
    let harness = format!("set -uo pipefail\n. {:?}\n{body}", lib());
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .output()
        .expect("failed to run bash harness");
    Run {
        exit_code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).trim().to_string(),
    }
}

/// The builder emits a `rm -f` that names every path it is given, and is best-effort (exits 0).
#[test]
fn purge_builder_emits_rm_f_for_every_path() {
    let out =
        run_sourced("event_artifact_purge_cmds /run/rig-painter.pid /run/rig-qpsk-markers.csv");
    assert_eq!(
        out.exit_code, 0,
        "the builder itself must exit 0. got=\n{}",
        out.stdout
    );
    assert!(
        out.stdout.contains("rm -f"),
        "#721: event_artifact_purge_cmds must emit a `rm -f`. got=\n{}",
        out.stdout
    );
    assert!(
        out.stdout.contains("/run/rig-qpsk-markers.csv"),
        "#721: the purge must name the marker CSV (the actual leftover). got=\n{}",
        out.stdout
    );
    assert!(
        out.stdout.contains("/run/rig-painter.pid"),
        "#721: the purge must name every cam2-side item-8 path it is given. got=\n{}",
        out.stdout
    );
}

/// The emitted command, when EXECUTED, actually DELETES the files (real behaviour, not just text),
/// and does not fail when a path is already gone (best-effort).
#[test]
fn purge_command_actually_deletes_the_files_and_is_idempotent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let f1 = tmp.path().join("rig-qpsk-markers.csv");
    let f2 = tmp.path().join("rig-painter.pid");
    fs::write(&f1, "marker-row\n").unwrap();
    fs::write(&f2, "1234\n").unwrap();
    // Build the purge command from the lib, then run it. A second run over the now-absent paths
    // must still exit 0 (idempotent best-effort).
    let body = format!(
        "cmd=\"$(event_artifact_purge_cmds {:?} {:?})\"\nbash -c \"$cmd\"\nbash -c \"$cmd\"\necho PURGED_OK",
        f1, f2
    );
    let out = run_sourced(&body);
    assert_eq!(
        out.exit_code, 0,
        "the purge must exit 0 even on a re-run. got=\n{}",
        out.stdout
    );
    assert!(
        out.stdout.contains("PURGED_OK"),
        "harness did not complete. got=\n{}",
        out.stdout
    );
    assert!(
        !f1.exists(),
        "#721: the marker CSV must be deleted by the purge"
    );
    assert!(
        !f2.exists(),
        "#721: the pidfile must be deleted by the purge"
    );
}

/// Static wiring: `do_event` must CALL the purge with the marker-CSV var, and it must run BEFORE
/// the item-8 assert phase (so `event_assert.py::artifacts_cleared_ok` sees the CSV already gone).
/// Bounded to do_event's own body (the #868 anchor pattern), never the whole file.
#[test]
fn do_event_purges_the_marker_csv_before_the_assert_phase() {
    let whole =
        fs::read_to_string(manifest_dir().join("scripts/rig-mode.sh")).expect("read rig-mode.sh");
    let body_start = whole
        .find("\ndo_event() {")
        .expect("#721: expected do_event to exist");
    let body_end = whole[body_start..]
        .find("\nmain() {")
        .map(|i| body_start + i)
        .expect("#721: expected main() to bound do_event's body");
    let body = &whole[body_start..body_end];
    assert!(
        body.contains("event_artifact_purge_cmds \"$PAINTER_PIDFILE\" \"$AUDIO_MARKER_LOG\""),
        "#721: do_event must purge the cam2-side item-8 artifacts (pidfile + marker CSV) via \
         event_artifact_purge_cmds. Got:\n{body}"
    );
    let purge_pos = body
        .find("event_artifact_purge_cmds")
        .expect("#721: expected the purge call");
    let assert_pos = body
        .find("event_mode_assert")
        .expect("#721: expected the item-8 assert phase call");
    assert!(
        purge_pos < assert_pos,
        "#721: the marker-CSV purge must run BEFORE the assert phase, so item 8 sees it gone. \
         Got:\n{body}"
    );
}

/// #721 (review hardening): the cam2-side PURGE arg-list and the cam2-side item-8 CHECK arg-list
/// must stay in LOCK-STEP. If item 8 ever checks a NEW cam2 artifact, the purge must delete it too
/// -- otherwise the CONTRACT would fail on the new leftover exactly as it did on the marker CSV.
/// Both call sites pass the same cam2 paths (after the shared "$PAINTER_PIDFILE" anchor); pin that
/// equality so a future path added to one side forces the same change on the other.
#[test]
fn purge_and_item8_check_share_the_same_cam2_paths() {
    let whole =
        fs::read_to_string(manifest_dir().join("scripts/rig-mode.sh")).expect("read rig-mode.sh");
    // The cam2-side item-8 check carries PAINTER_PIDFILE (the OTHER check call passes the dev1
    // heartbeat path); the purge carries the same anchor. Compare the args that follow it.
    let check_args = args_after(
        &whole,
        "event_assert_artifacts_check_cmds \"$PAINTER_PIDFILE\"",
    )
    .expect("#721: expected the cam2-side item-8 check call (PAINTER_PIDFILE)");
    let purge_args = args_after(&whole, "event_artifact_purge_cmds \"$PAINTER_PIDFILE\"")
        .expect("#721: expected the purge call (PAINTER_PIDFILE)");
    assert_eq!(
        check_args, purge_args,
        "#721 lock-step: the purge must delete EXACTLY the cam2 paths item 8 checks. \
         check-args={check_args:?} purge-args={purge_args:?}"
    );
}
