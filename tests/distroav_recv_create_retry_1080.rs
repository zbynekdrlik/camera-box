//! #1080 — a failed `recv_create_v3` in the DistroAV receiver reset path must NOT permanently kill
//! the receiver thread (`vendor/distroav/src/ndi-source.cpp`).
//!
//! Root cause: `ndi_source_thread`'s reset block did `if (!ndi_receiver) { …ERR-407…; break; }`
//! after `recv_create_v3`. That `break` exits the `while (s->running)` loop but NEVER sets
//! `s->running = false` (only `ndi_source_thread_stop()` does), so the thread returns with
//! `s->running` still true — and `ndi_source_update()`'s `if (s->running)` then only sets a reset
//! flag the DEAD thread never reads, NEVER restarting it. The receiver is permanently, reattach-
//! proof black until a human recreates the source. Since #767 the stale-reconnect watchdog enters
//! this reset path AUTONOMOUSLY, so a transient `recv_create_v3` failure there is now an unattended
//! permanent death — exactly the failure class #767 exists to remove, one layer down.
//!
//! The fix: never break on that failure. Blank the source, back off (bounded exponential, NEVER
//! capping the retry COUNT), re-arm `reset_ndi_receiver`, and `continue` so the next loop iteration
//! re-attempts the create — the thread (and the #767 watchdog living in it) stays alive. The
//! backoff is the pure `static inline ndi_recv_create_retry_backoff_ns(unsigned)` helper.
//!
//! Why this test is std-only + runs offline: camera-box's `# airuleset:build-ok` bypass is disabled
//! and the vendored C compiles only on CI, so per `.claude/rules/vendored-libobs-change-safety.md`
//! (the #767 / #1026 pattern) this file (a) SOURCE-ANCHORS the C tokens with a std-only
//! `fs::read_to_string` guard runnable via `rustc --test` (revert protection against a future
//! `git subtree pull`), and (b) LIFTS the pure `ndi_recv_create_retry_backoff_ns` helper VERBATIM,
//! compiles it with the C toolchain against a tiny stub, and runs it over a hand-written truth
//! table that encodes the intended backoff at every boundary — proving the SHIPPED bytes COMPUTE,
//! not just SAY, the right thing. Nothing in the Rust appliance consumes the helper, so the truth
//! table IS the spec. Per the project's test-strictness rule the lift-compile FAILS LOUDLY if no C
//! compiler is present, never skips.

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

/// Collapse every run of ASCII whitespace to a single space so the anchors survive reformatting
/// (an upstream merge re-indenting a line, a clang-format wrap move).
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ----------------------------------------------------------------------------------------------
// Facet A — source anchors (revert protection: a `git subtree pull` re-importing stock DistroAV
// silently drops the whole patch; CI then fails loudly here. These are ALSO mirrored as pwsh token
// checks in BOTH windows-genlock*.yml, because the fast path hot-swaps distroav.dll un-gated).
// ----------------------------------------------------------------------------------------------

#[test]
fn backoff_helper_present() {
    let src = squish(&vendor_file(NDI_SOURCE));
    assert!(
        src.contains("static inline uint64_t ndi_recv_create_retry_backoff_ns("),
        "{NDI_SOURCE}: #1080 patch missing — the pure `ndi_recv_create_retry_backoff_ns(...)` \
         backoff helper is gone. Without it a transient recv_create_v3 failure in the reset path \
         permanently kills the receiver thread (reattach-proof black). A `git subtree pull` likely \
         reverted it."
    );
}

#[test]
fn create_failure_retries_instead_of_breaking() {
    let src = squish(&vendor_file(NDI_SOURCE));

    // The reset block must CALL the backoff helper with the consecutive-failure counter.
    assert!(
        src.contains("ndi_recv_create_retry_backoff_ns(recv_create_fail_count)"),
        "{NDI_SOURCE}: #1080 patch missing — the reset block no longer calls \
         ndi_recv_create_retry_backoff_ns(recv_create_fail_count), so a failed recv_create_v3 is \
         no longer backed off + retried. Re-apply the #1080 retry."
    );

    // The consecutive-failure counter must be both incremented (on failure) and reset (on success).
    assert!(
        src.contains("recv_create_fail_count++;"),
        "{NDI_SOURCE}: #1080 patch missing — the recv_create_fail_count counter is never \
         incremented on a create failure, so the backoff cannot escalate."
    );
    assert!(
        src.contains("recv_create_fail_count = 0;"),
        "{NDI_SOURCE}: #1080 patch missing — recv_create_fail_count is never reset to 0 after a \
         successful create, so the backoff would stay escalated forever."
    );

    // The recv-create-failure branch must RETRY (continue), never `break`. Slice the branch between
    // its unique first statement (recv_create_fail_count++;) and the on-success reset
    // (recv_create_fail_count = 0;) and require `continue;` present + `break;` absent — proving the
    // old permanent-death `break` was replaced, not merely joined by new code.
    let start = src
        .find("recv_create_fail_count++;")
        .expect("#1080: recv_create_fail_count++ anchor not found");
    let end_rel = src[start..]
        .find("recv_create_fail_count = 0;")
        .expect("#1080: recv_create_fail_count = 0 anchor not found after the ++ site");
    let branch = &src[start..start + end_rel];
    assert!(
        branch.contains("continue;"),
        "{NDI_SOURCE}: #1080 — the recv_create_v3-failure branch must `continue` (retry the loop), \
         but no `continue;` was found in it:\n{branch}"
    );
    assert!(
        !branch.contains("break;"),
        "{NDI_SOURCE}: #1080 — the recv_create_v3-failure branch still contains a `break;`, which \
         permanently kills the receiver thread (s->running stays true, ndi_source_update never \
         restarts it). It must retry, not break:\n{branch}"
    );

    // The blank-and-re-arm machinery the retry reuses must be present in the branch.
    assert!(
        branch.contains("process_empty_frame(s);")
            && branch.contains("s->config.reset_ndi_receiver = true;"),
        "{NDI_SOURCE}: #1080 — the retry branch must blank the source (process_empty_frame) and \
         re-arm reset_ndi_receiver so the next iteration re-attempts the create:\n{branch}"
    );
}

// ----------------------------------------------------------------------------------------------
// Facet B — lift the pure helper, compile it standalone under -Werror -Wconversion -Wformat=2, run
// it over a truth table that encodes the intended backoff at every boundary. Proves the shipped C
// COMPUTES correctly (not just that it says the right thing).
// ----------------------------------------------------------------------------------------------

/// Lift the `ndi_recv_create_retry_backoff_ns` helper VERBATIM from the vendored C (never retype it
/// — a retyped copy verifies your typing, not the shipped bytes).
fn lift_backoff_helper() -> String {
    let src = vendor_file(NDI_SOURCE);
    let start = src
        .find("static inline uint64_t ndi_recv_create_retry_backoff_ns(")
        .unwrap_or_else(|| {
            panic!(
                "#1080: {NDI_SOURCE} no longer defines ndi_recv_create_retry_backoff_ns — there is \
                 nothing to compile/behaviour-check."
            )
        });
    let end = src[start..]
        .find("\n}\n")
        .map(|i| start + i + 3)
        .expect("#1080: ndi_recv_create_retry_backoff_ns has no closing brace `\\n}\\n`");
    src[start..end].to_string()
}

/// `(consecutive_failures, expected_ns)`. Each row pins one boundary of the intended backoff:
/// 250 ms base, doubling, capped at 3 s; 1-based (the count INCLUDING this failure); n==0 folds to
/// the base; large n is clamped (no shift UB) and stays at the cap.
fn vectors() -> Vec<(u32, u64)> {
    vec![
        (0, 250_000_000),           // defensive: 0 folds to base
        (1, 250_000_000),           // first failure -> base 250 ms
        (2, 500_000_000),           // doubling
        (3, 1_000_000_000),
        (4, 2_000_000_000),         // just under the 3 s cap
        (5, 3_000_000_000),         // 250 ms << 4 = 4 s -> capped to 3 s (ternary cap boundary)
        (6, 3_000_000_000),         // 250 ms << 5 = 8 s -> capped (shift-clamp path)
        (100, 3_000_000_000),       // large n: shift clamped, no UB, stays capped
    ]
}

#[test]
fn backoff_computes_the_spec_truth_table() {
    let helper = lift_backoff_helper();
    let vs = vectors();

    let mut c = String::from("#include <stdint.h>\n#include <stdio.h>\n");
    c.push_str(&helper);
    c.push_str("\nint main(void){\n");
    for (n, _) in &vs {
        c.push_str(&format!(
            "    printf(\"%llu\\n\", (unsigned long long)ndi_recv_create_retry_backoff_ns({n}u));\n"
        ));
    }
    c.push_str("    return 0;\n}\n");

    let dir = std::env::temp_dir().join("distroav_recv_create_retry_1080");
    fs::create_dir_all(&dir).expect("create the scratch dir");
    let cfile = dir.join("backoff.c");
    let bin = dir.join("backoff.bin");
    fs::write(&cfile, &c).expect("write the harness");

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
                "#1080: could not run the C compiler `{cc}` ({e}). This gate compiles the vendored \
                 ndi_recv_create_retry_backoff_ns to prove the C both COMPILES and computes the \
                 spec; it must FAIL rather than skip when the toolchain is absent (a gate that \
                 silently passes without running is worse than none). Install a C compiler or set CC."
            )
        });
    assert!(
        out.status.success(),
        "#1080: ndi_recv_create_retry_backoff_ns lifted from {NDI_SOURCE} does NOT COMPILE \
         standalone under -Wall -Wextra -Wformat=2 -Wconversion -Werror. The vendored tree is \
         otherwise compiled only by the genlock workflows, so this is very likely a real compile \
         error heading for CI:\n--- cc stderr ---\n{}\n--- harness ---\n{c}",
        String::from_utf8_lossy(&out.stderr)
    );

    let run = Command::new(&bin)
        .output()
        .expect("#1080: the compiled harness failed to execute");
    assert!(
        run.status.success(),
        "#1080: the harness exited non-zero: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8(run.stdout).expect("harness stdout is utf-8");
    let got: Vec<u64> = stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.trim().parse().expect("harness printed a non-integer"))
        .collect();
    assert_eq!(
        got.len(),
        vs.len(),
        "#1080: the harness printed {} results for {} vectors",
        got.len(),
        vs.len()
    );

    let mut diffs = Vec::new();
    for ((n, want), g) in vs.iter().zip(&got) {
        if g != want {
            diffs.push(format!("  consecutive_failures={n} -> C {g} ns, expected {want} ns"));
        }
    }
    assert!(
        diffs.is_empty(),
        "#1080: the vendored C ndi_recv_create_retry_backoff_ns DIVERGED from the intended backoff \
         spec on {} of {} vectors:\n{}",
        diffs.len(),
        vs.len(),
        diffs.join("\n")
    );
}
