//! Regression guard for #91 (terminal stream box self-discovery abort blocks the
//! strih→stream hop measurement).
//!
//! Symptom: `scripts/multitap-e2e.sh` aborted during `[3/5] OBS setup` on the
//! **stream** box with:
//!   "[obs] 10.77.9.204: own Main Output 'stream' did not resolve to a full NDI
//!    name (the next hop would ingest a dead name); aborting before touching the
//!    program scene."
//!
//! Root cause: `obs_phase2.py setup()` resolved the box's OWN Main Output name by
//! polling **that box's own OBS** NDI discovery list, then ABORTED if it never
//! resolved to a full `MACHINE (name)`. On the TERMINAL box (stream) that abort is
//! WRONG: NDI suppresses self/loopback discovery of an output on the same machine,
//! so the stream box's own OBS never lists its own `STREAM-SNV (stream)` output and
//! the resolve never completes — yet there is NO downstream OBS hop to protect
//! (dev1 taps the stream output directly, and dev1 *can* discover it). The abort
//! exists only to stop a NEXT OBS box from ingesting a dead bare name; the terminal
//! box has no such next hop.
//!
//! Fix: the harness marks the terminal box (stream — the last setup call, whose
//! output feeds only dev1's tap, not another OBS hop) so `obs_phase2.py setup`
//! SKIPS the own-output abort for it (warning + emitting whatever name resolved),
//! while the NON-terminal box (strih, whose output feeds the next OBS hop) KEEPS
//! the protective abort unchanged.
//!
//! These tests read the scripts statically (they do NOT run python / touch OBS).
//! RED on the pre-#91 unconditional abort; GREEN once the terminal box is exempt.

use std::fs;

fn obs_py() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/obs_phase2.py");
    fs::read_to_string(path).expect("read scripts/obs_phase2.py")
}

fn harness_sh() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/multitap-e2e.sh");
    fs::read_to_string(path).expect("read scripts/multitap-e2e.sh")
}

/// `setup()` must accept a terminal-box flag so the caller can declare a box whose
/// output feeds no downstream OBS hop (only dev1's tap). Without the flag there is
/// no way to distinguish the strih box (next-hop protection legitimate) from the
/// stream box (terminal — abort spurious).
#[test]
fn setup_accepts_a_terminal_flag() {
    let py = obs_py();
    assert!(
        py.contains("--terminal"),
        "#91: obs_phase2.py setup must accept a --terminal flag so the harness can mark \
         the box whose Main Output feeds NO downstream OBS hop (the stream box, tapped \
         directly by dev1). Without it the spurious own-output abort can't be skipped \
         only for the terminal box."
    );
}

/// The own-Main-Output resolution abort MUST be conditional on the box NOT being
/// terminal. On a terminal box the box's own OBS cannot self-discover its own
/// output (NDI loopback suppression), so the abort would always fire — yet there is
/// no next OBS hop to protect. The abort must therefore be gated by the terminal
/// flag, not unconditional.
#[test]
fn own_output_abort_is_skipped_for_terminal_box() {
    let py = obs_py();
    let su = py.find("def setup(").expect("setup() not found");
    let end = py[su..].find("\ndef ").map(|i| su + i).unwrap_or(py.len());
    let body = &py[su..end];

    // The own-output abort message must still exist (we keep protecting the next
    // OBS hop for NON-terminal boxes). Anchor on its UNIQUE phrase — "the next hop
    // would ingest a dead name" only appears in the own-output SystemExit, never in
    // the ingest-source abort nor the pre-existing comments — so we target the real
    // abort statement, not an unrelated mention of "own Main Output".
    let abort_idx = body
        .find("the next hop would ingest a dead name")
        .expect("#91: the own-Main-Output abort message must remain for non-terminal boxes");

    // ... but it must be guarded by the terminal flag: somewhere before the abort,
    // setup() branches on `terminal` (e.g. `if not a.terminal:` / `if a.terminal:`)
    // so the abort is skipped on the terminal box.
    let before_abort = &body[..abort_idx];
    assert!(
        before_abort.contains("terminal"),
        "#91: the own-Main-Output abort (`{}`) must be gated by the terminal flag so it \
         is SKIPPED for the terminal box (stream), whose own OBS cannot self-discover its \
         own output and which has no downstream OBS hop to protect. It must not fire \
         unconditionally.",
        "own Main Output ... aborting"
    );
}

/// The ingest-source abort (the upstream the box itself ingests) must stay
/// unconditional — that source is a real remote NDI feed the box must connect to,
/// so a dead ingest name is always a fatal misconfiguration regardless of terminal
/// status. Only the OWN-output abort is relaxed for the terminal box.
#[test]
fn ingest_source_abort_stays_unconditional() {
    let py = obs_py();
    let su = py.find("def setup(").expect("setup() not found");
    let end = py[su..].find("\ndef ").map(|i| su + i).unwrap_or(py.len());
    let body = &py[su..end];
    assert!(
        body.contains("ingest source")
            && body.contains("aborting before touching the program scene"),
        "#91: the ingest-source resolution abort must remain (a box that cannot resolve \
         the upstream it ingests is always broken, terminal or not)."
    );
}

/// The harness must mark the STREAM box (the terminal box — its output is tapped by
/// dev1, not ingested by another OBS hop) with the terminal flag, and must NOT mark
/// the STRIH box (whose output feeds the next OBS hop).
#[test]
fn harness_marks_only_the_stream_box_terminal() {
    let sh = harness_sh();

    // Find the two setup invocations.
    let strih_call = sh
        .find("setup --host \"$STRIH\"")
        .expect("strih OBS setup call not found in multitap-e2e.sh");
    let stream_call = sh
        .find("setup --host \"$STREAM\"")
        .expect("stream OBS setup call not found in multitap-e2e.sh");

    // The strih setup line (whose output feeds the next OBS hop) must NOT pass
    // --terminal: its own-output abort legitimately protects the stream box.
    let strih_line_end = sh[strih_call..]
        .find('\n')
        .map(|i| strih_call + i)
        .unwrap_or(sh.len());
    let strih_line = &sh[strih_call..strih_line_end];
    assert!(
        !strih_line.contains("--terminal"),
        "#91: the STRIH setup call must NOT be marked --terminal — its Main Output feeds \
         the next OBS hop (stream), so the own-output resolution abort legitimately \
         protects that hop. strih line: {strih_line}"
    );

    // The stream setup line (terminal box, tapped directly by dev1) MUST pass
    // --terminal so the spurious self-discovery abort is skipped.
    let stream_line_end = sh[stream_call..]
        .find('\n')
        .map(|i| stream_call + i)
        .unwrap_or(sh.len());
    let stream_line = &sh[stream_call..stream_line_end];
    assert!(
        stream_line.contains("--terminal"),
        "#91: the STREAM setup call must be marked --terminal — its Main Output is tapped \
         directly by dev1 (no downstream OBS hop), and its own OBS cannot self-discover \
         its own output, so the own-output abort is spurious and must be skipped. stream \
         line: {stream_line}"
    );
}
