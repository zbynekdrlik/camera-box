//! #1097 — a failed `framesync_create` in the DistroAV receiver reset path must NOT permanently kill
//! the receiver thread (`vendor/distroav/src/ndi-source.cpp`). This is the exact sibling of #1080's
//! `recv_create_v3` break, one branch further down the SAME reset block.
//!
//! Root cause: `ndi_source_thread`'s reset block did `if (!ndi_frame_sync) { …ERR-408…; break; }`
//! after `ndi_frame_sync = ndiLib->framesync_create(ndi_receiver)`, under `if
//! (snap_framesync_enabled)`. That `break` exits the `while (s->running)` loop but NEVER sets
//! `s->running = false` (only `ndi_source_thread_stop()` does), so the thread returns with
//! `s->running` still true — and `ndi_source_update()`'s `if (s->running)` then only sets a reset
//! flag the DEAD thread never reads, NEVER restarting it. The receiver is permanently, reattach-
//! proof black until a human recreates the source. Since #767 the stale-reconnect watchdog enters
//! this reset path AUTONOMOUSLY, so a transient `framesync_create` failure there is an unattended
//! permanent death — exactly the class #1080 removed one branch up.
//!
//! Currently DORMANT: `GENLOCK_FORCED_SETTINGS` pins `{PROP_FRAMESYNC, true, 0, false}`, so
//! `snap_framesync_enabled` is always false on this appliance and the branch never executes. The fix
//! is kept correct-but-latent so a future framesync-on config can never silently wedge here.
//!
//! The fix mirrors #1080's retry-in-place: never break on that failure. Blank the source
//! (`process_empty_frame`), give the reconnect a fresh #767 stale window (`was_disconnected = true`),
//! back off (the SHARED pure `ndi_recv_create_retry_backoff_ns(unsigned)` helper — generic bounded
//! exponential, retry COUNT never capped), re-arm `reset_ndi_receiver` under `config_mutex`, and
//! `continue` so the next iteration's reset block (recv_destroy + framesync_destroy(nullptr))
//! cleans up and re-creates. It uses its OWN `framesync_create_fail_count`, NOT the shared
//! `recv_create_fail_count`, so (a) the live-tested #1080 recv_create retry path stays byte-for-byte
//! unchanged and (b) a pure-framesync-failure loop still escalates its backoff (a successful
//! recv_create each iteration would otherwise reset a shared counter).
//!
//! Why this gate is FACET-A ONLY (source anchors), no lift-compile/truth-table facet: #1097 reuses
//! the SAME `ndi_recv_create_retry_backoff_ns` helper whose backoff math is already lifted +
//! truth-tabled by `tests/distroav_recv_create_retry_1080.rs`; re-lifting it here would be pure
//! redundancy. The #1097-unique risk is the CONTROL-FLOW revert (break reappearing on a `git subtree
//! pull` re-importing stock DistroAV), which a source anchor catches. Per
//! `.claude/rules/distroav-receiver-lifecycle.md` these anchors are ALSO mirrored as pwsh token
//! checks in BOTH `windows-genlock*.yml` (the fast path hot-swaps distroav.dll un-gated). Runs
//! offline via `rustc --test`; std-only.

use std::fs;
use std::path::PathBuf;

const NDI_SOURCE: &str = "vendor/distroav/src/ndi-source.cpp";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn vendor_file(rel: &str) -> String {
    let p = repo(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// Collapse every run of ASCII whitespace to a single space so anchors survive reformatting
/// (an upstream merge re-indenting a line, a clang-format wrap move).
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ----------------------------------------------------------------------------------------------
// Facet A — source anchors (revert protection: a `git subtree pull` re-importing stock DistroAV
// silently drops the whole patch; CI then fails loudly here. Mirrored as pwsh token checks in BOTH
// windows-genlock*.yml because the fast path hot-swaps distroav.dll un-gated).
// ----------------------------------------------------------------------------------------------

#[test]
fn framesync_create_fail_counter_present() {
    let src = squish(&vendor_file(NDI_SOURCE));
    // The dedicated counter must be DECLARED (not the shared recv_create_fail_count).
    assert!(
        src.contains("unsigned framesync_create_fail_count = 0;"),
        "{NDI_SOURCE}: #1097 patch missing — the dedicated `unsigned framesync_create_fail_count = \
         0;` retry counter is gone. Without it a transient framesync_create failure in the reset \
         path permanently kills the receiver thread (reattach-proof black). A `git subtree pull` \
         likely reverted it."
    );
    // The framesync retry must CALL the SHARED #1080 backoff helper WITH the framesync counter.
    assert!(
        src.contains("ndi_recv_create_retry_backoff_ns(framesync_create_fail_count)"),
        "{NDI_SOURCE}: #1097 patch missing — the framesync branch no longer calls \
         ndi_recv_create_retry_backoff_ns(framesync_create_fail_count), so a failed framesync_create \
         is no longer backed off + retried. Re-apply the #1097 retry."
    );
    // The counter must be incremented on failure.
    assert!(
        src.contains("framesync_create_fail_count++;"),
        "{NDI_SOURCE}: #1097 patch missing — framesync_create_fail_count is never incremented on a \
         framesync-create failure, so the backoff cannot escalate."
    );
    // Must appear at least TWICE: the declaration AND the on-success reset. `.contains(...)` alone
    // is satisfied by the declaration, so it could not detect removal of the on-success reset.
    assert!(
        src.matches("framesync_create_fail_count = 0;").count() >= 2,
        "{NDI_SOURCE}: #1097 patch missing — framesync_create_fail_count is never reset to 0 after a \
         successful framesync create (only the declaration is present), so the backoff would stay \
         escalated forever."
    );
}

#[test]
fn framesync_create_failure_retries_instead_of_breaking() {
    let src = squish(&vendor_file(NDI_SOURCE));

    // Slice the framesync-failure branch between its unique first statement
    // (framesync_create_fail_count++;) and the on-success reset (framesync_create_fail_count = 0;),
    // then require `continue;` present + `break;` absent — proving the old permanent-death `break`
    // was REPLACED by the retry, not merely joined by new code. (The declaration
    // `unsigned framesync_create_fail_count = 0;` sits far earlier in the file, so the `= 0;` found
    // AFTER the `++` site is the on-success reset, exactly as the #1080 test slices its own branch.)
    let start = src
        .find("framesync_create_fail_count++;")
        .expect("#1097: framesync_create_fail_count++ anchor not found");
    let end_rel = src[start..]
        .find("framesync_create_fail_count = 0;")
        .expect("#1097: framesync_create_fail_count = 0 anchor not found after the ++ site");
    let branch = &src[start..start + end_rel];

    assert!(
        branch.contains("continue;"),
        "{NDI_SOURCE}: #1097 — the framesync_create-failure branch must `continue` (retry the loop), \
         but no `continue;` was found in it:\n{branch}"
    );
    assert!(
        !branch.contains("break;"),
        "{NDI_SOURCE}: #1097 — the framesync_create-failure branch still contains a `break;`, which \
         permanently kills the receiver thread (s->running stays true, ndi_source_update never \
         restarts it). It must retry, not break:\n{branch}"
    );
    // The blank-and-re-arm machinery the retry reuses must be present in the branch.
    assert!(
        branch.contains("process_empty_frame(s);")
            && branch.contains("s->config.reset_ndi_receiver = true;")
            && branch.contains("was_disconnected = true;"),
        "{NDI_SOURCE}: #1097 — the retry branch must blank the source (process_empty_frame), give a \
         fresh #767 stale window (was_disconnected = true), and re-arm reset_ndi_receiver so the \
         next iteration re-attempts the create:\n{branch}"
    );
}
