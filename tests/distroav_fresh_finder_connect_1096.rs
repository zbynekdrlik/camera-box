//! #1096 — strih DistroAV receiver wedges on cambox sender restart: `recv_create_v3` succeeds but
//! the receiver, created **connect-BY-NAME**, never re-resolves a RESTARTED sender (rotated NDI
//! port) because the long-lived, per-process internal NDI finder that name-resolution consults is
//! poisoned with the sender's stale address. Only an OBS process restart (fresh SDK finder state)
//! recovers; a same-name reattach and a forced recv rebuild both re-consult the SAME poisoned
//! finder and stay at 0 frames. Distinct from #1080 (which fires only on a NULL create) — see
//! `.claude/rules/distroav-receiver-lifecycle.md`.
//!
//! The fix (`vendor/distroav/src/ndi-source.cpp` `ndi_source_thread` reset block): resolve the
//! source through a FRESH `NDIlib_find_instance_t` per reset (`find_create_v2` → bounded
//! `find_wait_for_sources` + `find_get_current_sources` → match the saved name → copy the matched
//! source's live `p_url_address` → `find_destroy`), then connect the new receiver BY-ADDRESS
//! (`source_to_connect_to.p_ndi_name = ""`, `p_url_address = <fresh url>`) — bypassing the poisoned
//! long-lived finder using the SAME finder mechanism `ndi-finder.cpp` already uses. Fallback: if the
//! fresh finder does not resolve a URL for the name, keep the current name-based connect (strictly
//! no worse than upstream). The name→URL pick is the pure `static inline
//! ndi_find_url_for_source_name(...)` helper.
//!
//! Why std-only + offline: camera-box's `# airuleset:build-ok` bypass is disabled and the vendored C
//! compiles only on CI, so per `.claude/rules/vendored-libobs-change-safety.md` (the #767/#1026
//! pattern) this file (a) SOURCE-ANCHORS the C tokens with a `fs::read_to_string` guard runnable via
//! `rustc --test` (revert protection against a future `git subtree pull`), and (b) LIFTS the pure
//! `ndi_find_url_for_source_name` helper VERBATIM, compiles it against a tiny stub, and runs it over a
//! hand-written truth table encoding the intended pick at every guard boundary — proving the SHIPPED
//! bytes COMPUTE, not just SAY, the right thing. Nothing in the Rust appliance consumes the helper, so
//! the truth table IS the spec. Per test-strictness the lift-compile FAILS LOUDLY when no C compiler
//! is present, never skips. The LIVE wedge cure is NOT offline-verifiable (vendored receive path, the
//! wedge reproduces only live) — flag UNVERIFIED for the supervisor's post-deploy rig repro.

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

// ----------------------------------------------------------------------------------------------
// Facet A — source anchors (revert protection: a `git subtree pull` re-importing stock DistroAV
// silently drops the whole patch; CI then fails loudly here. ALSO mirrored as pwsh token checks in
// BOTH windows-genlock*.yml, because the fast path hot-swaps distroav.dll un-gated).
// ----------------------------------------------------------------------------------------------

#[test]
fn url_picker_helper_present() {
    let src = squish(&vendor_file(NDI_SOURCE));
    assert!(
        src.contains("static inline const char *ndi_find_url_for_source_name("),
        "{NDI_SOURCE}: #1096 patch missing — the pure `ndi_find_url_for_source_name(...)` URL-picker \
         helper is gone. Without it the reset block cannot connect BY-ADDRESS and the strih receiver \
         stays wedged after a sender restart (poisoned name-finder). A `git subtree pull` likely \
         reverted it."
    );
}

#[test]
fn reset_block_uses_a_fresh_finder_and_connects_by_url() {
    let src = squish(&vendor_file(NDI_SOURCE));

    // A FRESH finder is created inside the receiver thread (find_create_v2 appears ONLY in
    // ndi-finder.cpp in stock DistroAV; its presence in ndi-source.cpp is the #1096 change).
    assert!(
        src.contains("ndiLib->find_create_v2("),
        "{NDI_SOURCE}: #1096 patch missing — the reset block no longer creates a FRESH \
         NDIlib_find_instance_t (ndiLib->find_create_v2). Without a fresh finder the receiver keeps \
         resolving the source name through the poisoned long-lived finder. Re-apply the #1096 fix."
    );
    // It waits for + reads the fresh finder's source list.
    assert!(
        src.contains("ndiLib->find_wait_for_sources(") && src.contains("ndiLib->find_get_current_sources("),
        "{NDI_SOURCE}: #1096 patch missing — the reset block no longer bounded-waits on + reads the \
         fresh finder (find_wait_for_sources / find_get_current_sources). Re-apply the #1096 fix."
    );
    // The pure picker is wired with the av_thread-owned source name.
    assert!(
        src.contains("ndi_find_url_for_source_name(owned_source_name,"),
        "{NDI_SOURCE}: #1096 patch missing — the reset block no longer calls \
         ndi_find_url_for_source_name(owned_source_name, ...) to pick the fresh URL. Re-apply the \
         #1096 wiring."
    );
    // The resolved URL is bound onto recv_desc (connect BY-ADDRESS).
    assert!(
        src.contains("recv_desc.source_to_connect_to.p_url_address = owned_source_url;"),
        "{NDI_SOURCE}: #1096 patch missing — the reset block no longer binds the fresh URL onto \
         recv_desc.source_to_connect_to.p_url_address, so the new receiver still connects by name \
         through the poisoned finder. Re-apply the #1096 connect-by-URL."
    );
    // The fresh finder is destroyed (no per-reset finder leak).
    assert!(
        src.contains("ndiLib->find_destroy("),
        "{NDI_SOURCE}: #1096 patch missing — the fresh finder is never find_destroy'd (per-reset \
         leak). Re-apply the #1096 fix."
    );
    // Safety: the URL must be COPIED into an owned buffer (the finder owns the source pointers and
    // find_destroy frees them). owned_source_url must be bstrdup'd AND freed on thread exit.
    assert!(
        src.contains("owned_source_url = bstrdup("),
        "{NDI_SOURCE}: #1096 — the resolved URL must be bstrdup'd into owned_source_url BEFORE \
         find_destroy (the finder owns the source pointers). Re-apply the owned-copy."
    );
    // Must appear at least TWICE: the loop-internal free before the bstrdup AND the thread-exit
    // free. Count >= 1 alone would be satisfied by the loop-internal free even if the thread-exit
    // free were dropped (a leak on every source-thread teardown).
    assert!(
        src.matches("bfree(owned_source_url);").count() >= 2,
        "{NDI_SOURCE}: #1096 — owned_source_url is not bfree'd in BOTH the reset loop AND on thread \
         exit (found < 2 frees); a missing thread-exit free leaks the URL on every teardown. \
         Re-apply the frees."
    );
    // The safe fallback: when no fresh URL is resolved, the name-based connect is kept (no worse
    // than upstream).
    // The safe fallback branch is anchored on its UNIQUE line `p_url_address = nullptr;` — the
    // #93 unconditional copy at :803 already contains `p_ndi_name = owned_source_name;`, so anchoring
    // on that would pass even if the whole else branch were deleted (leaving a stale/NULL url).
    assert!(
        src.contains("recv_desc.source_to_connect_to.p_url_address = nullptr;"),
        "{NDI_SOURCE}: #1096 — the name-based fallback branch (which sets \
         recv_desc.source_to_connect_to.p_url_address = nullptr so a stale URL from a prior reset \
         cannot bleed into a name-based connect) is gone; a fresh-finder miss must fall back to the \
         name path with a cleared url, never leave the source unconnectable."
    );
}

#[test]
fn no_connection_path_rearms_the_fresh_finder_reset() {
    // The no_connections==0 steady path must autonomously trigger the reset block's fresh finder
    // after a stale window — a GRACEFUL cambox restart (clean FIN) drops the strih receiver to
    // no_connections==0, where the #767 watchdog (no_connections>0 only) never fires and a by-URL
    // receiver cannot self-rebind. Without this, the fresh-finder cure is unreachable for the
    // ticket's own `systemctl restart camera-box` scenario when it produces a clean close.
    let src = squish(&vendor_file(NDI_SOURCE));
    assert!(
        src.contains("uint64_t no_conn_since_ns = 0;"),
        "{NDI_SOURCE}: #1096 patch missing — the no_conn_since_ns disconnect timer is gone, so the \
         no_connections==0 path can no longer arm a stale-window fresh-finder reset (a graceful \
         sender restart would leave the source black). Re-apply the #1096 no-connection recovery."
    );
    // The no_connections==0 branch must re-arm reset_ndi_receiver, gated on the same stale window +
    // genlock scope as #767. Slice from the disconnect-timer arm to the sleep-and-continue and
    // require both the window check and the reset re-arm inside it.
    let start = src
        .find("if (no_conn_since_ns == 0)")
        .expect("#1096: no_conn_since_ns arm anchor not found in the no_connections==0 path");
    let end_rel = src[start..]
        .find("std::this_thread::sleep_for(std::chrono::milliseconds(100)); continue;")
        .expect("#1096: the no_connections==0 sleep+continue anchor not found after the timer arm");
    let branch = &src[start..start + end_rel];
    assert!(
        branch.contains(">= GENLOCK_RECONNECT_STALE_NS")
            && branch.contains("genlock_source_is_active(s->obs_source)")
            && branch.contains("s->config.reset_ndi_receiver = true;"),
        "{NDI_SOURCE}: #1096 — the no_connections==0 path no longer forces a fresh-finder reset \
         after GENLOCK_RECONNECT_STALE_NS for a genlocked source, so a graceful sender restart has \
         no autonomous recovery:\n{branch}"
    );
}

// ----------------------------------------------------------------------------------------------
// Facet B — lift the pure picker, compile standalone under -Werror -Wconversion -Wformat=2, run it
// over a truth table encoding the intended pick at every guard boundary. Proves the shipped C
// COMPUTES correctly (not just that it says the right thing). Nothing in Rust consumes it, so the
// truth table IS the spec.
// ----------------------------------------------------------------------------------------------

/// Lift the `ndi_find_url_for_source_name` helper VERBATIM (never retype it — a retyped copy verifies
/// your typing, not the shipped bytes).
fn lift_url_picker() -> String {
    let src = vendor_file(NDI_SOURCE);
    let start = src
        .find("static inline const char *ndi_find_url_for_source_name(")
        .unwrap_or_else(|| {
            panic!(
                "#1096: {NDI_SOURCE} no longer defines ndi_find_url_for_source_name — there is \
                 nothing to compile/behaviour-check."
            )
        });
    let end = src[start..]
        .find("\n}\n")
        .map(|i| start + i + 3)
        .expect("#1096: ndi_find_url_for_source_name has no closing brace `\\n}\\n`");
    src[start..end].to_string()
}

/// One source cell: `(p_ndi_name, p_url_address)`, `None` => a C `NULL` pointer.
type Cell = (Option<&'static str>, Option<&'static str>);

struct Vector {
    /// requested source name (`None` => NULL passed to the helper)
    name: Option<&'static str>,
    /// the fresh finder's discovered source list
    sources: Vec<Cell>,
    /// expected returned URL (`None` => the helper must return NULL)
    expect: Option<&'static str>,
    /// what boundary this row pins (for the failure message)
    why: &'static str,
}

/// A C string literal for `Some("x")` or the token `NULL` for `None`. The rig names/URLs used here
/// contain no quotes/backslashes, so a plain wrap is safe.
fn c_str(v: Option<&str>) -> String {
    match v {
        Some(s) => format!("\"{s}\""),
        None => "NULL".to_string(),
    }
}

fn vectors() -> Vec<Vector> {
    vec![
        Vector {
            name: Some("CAM1"),
            sources: vec![(Some("CAM1"), Some("tcp://10.0.0.5:5962"))],
            expect: Some("tcp://10.0.0.5:5962"),
            why: "exact match with URL -> connect by that URL",
        },
        Vector {
            name: Some("CAM1"),
            sources: vec![(Some("CAM1"), Some(""))],
            expect: None,
            why: "match but EMPTY url -> NULL (fall back to name)",
        },
        Vector {
            name: Some("CAM1"),
            sources: vec![(Some("CAM1"), None)],
            expect: None,
            why: "match but NULL url -> NULL (fall back to name)",
        },
        Vector {
            name: Some("CAM1"),
            sources: vec![(Some("CAM2"), Some("tcp://10.0.0.6:5961"))],
            expect: None,
            why: "name not in list -> NULL (fall back to name)",
        },
        Vector {
            name: Some("CAM1"),
            sources: vec![],
            expect: None,
            why: "empty finder list -> NULL (fall back to name)",
        },
        Vector {
            name: Some(""),
            sources: vec![(Some(""), Some("tcp://x:1"))],
            expect: None,
            why: "EMPTY requested name -> NULL (never match an empty name)",
        },
        Vector {
            name: None,
            sources: vec![(Some("CAM1"), Some("tcp://x:1"))],
            expect: None,
            why: "NULL requested name -> NULL",
        },
        Vector {
            name: Some("Cam 3 (usb)"),
            sources: vec![
                (Some("Cam 1 (usb)"), Some("tcp://10.0.0.1:5961")),
                (Some("Cam 3 (usb)"), Some("tcp://10.0.0.9:5962")),
            ],
            expect: Some("tcp://10.0.0.9:5962"),
            why: "match at index 1 -> that URL (not index-0's, not a constant)",
        },
        Vector {
            name: Some("CAM1"),
            sources: vec![
                (None, Some("tcp://skip:1")),
                (Some("CAM1"), Some("tcp://10.0.0.5:7001")),
            ],
            expect: Some("tcp://10.0.0.5:7001"),
            why: "NULL-named source skipped, later name match wins (different URL value)",
        },
        Vector {
            name: Some("CAM1"),
            sources: vec![
                (Some("CAM1"), Some("tcp://first:1")),
                (Some("CAM1"), Some("tcp://second:2")),
            ],
            expect: Some("tcp://first:1"),
            why: "first name match wins over a later duplicate",
        },
    ]
}

#[test]
fn url_picker_computes_the_spec_truth_table() {
    let helper = lift_url_picker();
    let vs = vectors();

    let mut c = String::from(
        "#include <stdint.h>\n#include <stddef.h>\n#include <string.h>\n#include <stdio.h>\n\
         typedef struct { const char *p_ndi_name; const char *p_url_address; } NDIlib_source_t;\n",
    );
    c.push_str(&helper);
    c.push_str("\nint main(void){\n");
    for (i, v) in vs.iter().enumerate() {
        c.push_str("    {\n");
        let count = v.sources.len();
        let (arr_expr, count_expr) = if count == 0 {
            // a zero-length array is illegal in C -> pass NULL, 0u
            ("(const NDIlib_source_t *)0".to_string(), "0u".to_string())
        } else {
            let cells: Vec<String> = v
                .sources
                .iter()
                .map(|(n, u)| format!("{{ {}, {} }}", c_str(*n), c_str(*u)))
                .collect();
            c.push_str(&format!(
                "        NDIlib_source_t arr{i}[] = {{ {} }};\n",
                cells.join(", ")
            ));
            (format!("arr{i}"), format!("{count}u"))
        };
        c.push_str(&format!(
            "        const char *r = ndi_find_url_for_source_name({}, {}, {});\n",
            c_str(v.name),
            arr_expr,
            count_expr
        ));
        c.push_str("        printf(\"%s\\n\", r ? r : \"__NULL__\");\n");
        c.push_str("    }\n");
    }
    c.push_str("    return 0;\n}\n");

    let dir = std::env::temp_dir().join("distroav_fresh_finder_connect_1096");
    fs::create_dir_all(&dir).expect("create the scratch dir");
    let cfile = dir.join("picker.c");
    let bin = dir.join("picker.bin");
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
                "#1096: could not run the C compiler `{cc}` ({e}). This gate compiles the vendored \
                 ndi_find_url_for_source_name to prove the C both COMPILES and computes the spec; it \
                 must FAIL rather than skip when the toolchain is absent (a gate that silently passes \
                 without running is worse than none). Install a C compiler or set CC."
            )
        });
    assert!(
        out.status.success(),
        "#1096: ndi_find_url_for_source_name lifted from {NDI_SOURCE} does NOT COMPILE standalone \
         under -Wall -Wextra -Wformat=2 -Wconversion -Werror. The vendored tree is otherwise compiled \
         only by the genlock workflows, so this is very likely a real compile error heading for \
         CI:\n--- cc stderr ---\n{}\n--- harness ---\n{c}",
        String::from_utf8_lossy(&out.stderr)
    );

    let run = Command::new(&bin)
        .output()
        .expect("#1096: the compiled harness failed to execute");
    assert!(
        run.status.success(),
        "#1096: the harness exited non-zero: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8(run.stdout).expect("harness stdout is utf-8");
    let got: Vec<String> = stdout.lines().map(|l| l.to_string()).collect();
    assert_eq!(
        got.len(),
        vs.len(),
        "#1096: the harness printed {} results for {} vectors:\n{stdout}",
        got.len(),
        vs.len()
    );

    let mut diffs = Vec::new();
    for (v, g) in vs.iter().zip(&got) {
        let want = v.expect.unwrap_or("__NULL__");
        if g != want {
            diffs.push(format!(
                "  name={:?} sources={:?} -> C {:?}, expected {:?}  [{}]",
                v.name, v.sources, g, want, v.why
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "#1096: the vendored C ndi_find_url_for_source_name DIVERGED from the intended pick spec on \
         {} of {} vectors:\n{}",
        diffs.len(),
        vs.len(),
        diffs.join("\n")
    );
}
