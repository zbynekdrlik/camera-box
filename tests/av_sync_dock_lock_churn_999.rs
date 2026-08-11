//! #999 — RollingOffsetCluster lock/unlock hysteresis (dock churn) in the C++ mirror.
//!
//! Live evidence (issue 999): every LOCKED entry on the deployed dock landed `mad_ms` right
//! against the single `CB_CLUSTER_MAX_MAD_MS` boundary used in both the acquire AND hold
//! directions, causing rapid LOCKED/UNLOCKED churn (912 transitions in one session) while
//! `matched` stayed far above its own floor throughout. `RollingOffsetCluster::push()`
//! (`camera-box-audio.hpp`) now applies a WIDER "stay locked" ceiling
//! (`max_mad_ms * CB_CLUSTER_HOLD_MULTIPLIER`) once already locked, while the strict entry
//! ceiling (acquiring a fresh lock) is unchanged — mirrors `src/av_sync_dock.rs`'s own
//! `RollingOffsetCluster` byte-for-byte (see that module's `rolling_cluster_hysteresis_*_999`
//! tests, the Rust-side proof of the identical scenarios below).
//!
//! RED (before the fix): `CB_CLUSTER_HOLD_MULTIPLIER` / `RollingOffsetCluster::locked` do not
//! exist in camera-box-audio.hpp -> this harness fails to COMPILE. GREEN (after the fix): it
//! compiles and every scenario assertion below passes.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The C++ harness: includes the REAL production header and drives `RollingOffsetCluster`
/// through the SAME scenarios `src/av_sync_dock.rs`'s own hysteresis tests already prove in Rust.
const HARNESS_CPP: &str = r##"
#include "camera-box-audio.hpp"
#include <cstdio>
#include <cmath>

using namespace camerabox;

static int g_failures = 0;

#define CHECK(cond, msg)                                                          \
    do {                                                                          \
        if (!(cond)) {                                                            \
            std::fprintf(stderr, "FAIL (%s:%d): %s\n", __FILE__, __LINE__, msg);  \
            g_failures++;                                                         \
        }                                                                         \
    } while (false)

int main()
{
    const uint64_t SEC = 1000000000ull;

    // (1) Hold ceiling keeps an already-locked cluster locked through a mad_ms excursion the
    // strict entry ceiling would reject. Bimodal +-35ms batch: exact mad_ms == 35.0 (median 0.0,
    // every deviation == 35.0), matched == the full count -- same hand-verified construction as
    // the Rust twin.
    {
        uint64_t window_ns = 100ull * SEC;
        RollingOffsetCluster c(window_ns, CB_CLUSTER_TOL_MS, CB_CLUSTER_MIN_MATCHED, CB_CLUSTER_MAX_MAD_MS);

        // Phase 1: lock TIGHT (mad ~0) with a batch well above min_matched, 1s apart.
        uint64_t t_ns = 0;
        CbAvOffset last;
        last.ok = false;
        for (size_t i = 0; i < CB_CLUSTER_MIN_MATCHED * 3; i++) {
            t_ns += SEC;
            last = c.push(t_ns, 0.0);
        }
        CHECK(last.ok, "phase 1 must lock the tight batch");
        CHECK(last.mad_ms < 1.0, "phase 1 tight batch must have ~0 mad");
        uint64_t tight_last_ns = t_ns;

        // Phase 2: while still fresh, add a bimodal +-35ms batch (one short of an even split) ON
        // TOP of the still-fresh tight batch -- total retained only GROWS (nothing evicted), so
        // matched never dips below min_matched.
        uint64_t wide_ns = tight_last_ns + 50ull * SEC;
        size_t half = CB_CLUSTER_MIN_MATCHED;
        size_t other_half_minus_one = CB_CLUSTER_MIN_MATCHED - 1;
        for (size_t k = 0; k < half + other_half_minus_one; k++) {
            wide_ns += 10000000ull; // 0.01s apart
            double off = (k < half) ? -35.0 : 35.0;
            last = c.push(wide_ns, off);
        }
        CHECK(last.ok, "must still be locked heading into the eviction step");

        // Phase 3: ONE jump that evicts the tight batch but keeps the wide batch, landing exactly
        // on an evenly-balanced min_matched-vs-min_matched bimodal +-35ms cluster.
        uint64_t jump_ns = tight_last_ns + window_ns + 20ull * SEC;
        CHECK(jump_ns - tight_last_ns > window_ns, "sanity: jump must evict the tight batch");
        CHECK(jump_ns - wide_ns <= window_ns, "sanity: jump must keep the wide batch");
        CbAvOffset est = c.push(jump_ns, 35.0);
        CHECK(est.ok, "#999: a hysteretic hold ceiling must keep an ALREADY-locked cluster locked "
                      "through a mad_ms excursion that exceeds the strict entry ceiling but stays "
                      "within the hold ceiling");
        CHECK(std::fabs(est.mad_ms - 35.0) < 1e-9, "sanity: bimodal +-35ms batch must give mad_ms==35.0");
        CHECK(est.mad_ms > CB_CLUSTER_MAX_MAD_MS, "sanity: must exceed the entry ceiling");
        CHECK(est.mad_ms <= CB_CLUSTER_MAX_MAD_MS * CB_CLUSTER_HOLD_MULTIPLIER,
              "sanity: must stay within the hold ceiling");
    }

    // (2) The SAME 35ms-mad batch must NOT be enough to ACQUIRE a fresh lock from a cold state --
    // proves entry and hold are two different ceilings. Built EXACTLY min_matched samples so
    // matched never reaches min_matched before the final, already-balanced push.
    {
        RollingOffsetCluster c = RollingOffsetCluster::dock();
        size_t half = CB_CLUSTER_MIN_MATCHED / 2;
        uint64_t t_ns = 0;
        CbAvOffset last;
        last.ok = false;
        for (size_t k = 0; k < CB_CLUSTER_MIN_MATCHED; k++) {
            t_ns += 10000000ull;
            double off = (k < half) ? -35.0 : 35.0;
            last = c.push(t_ns, off);
        }
        CHECK(!last.ok, "#999: a fresh, never-before-locked cluster must not acquire a lock from a "
                        "35ms-mad batch -- only the strict entry ceiling governs acquisition");
    }

    // (3) The hysteresis is BOUNDED -- an excursion beyond the hold ceiling still unlocks, even
    // from an already-locked state. Bimodal +-tol_ms(60) split: exact mad_ms == 60.0 (> 50.0 hold
    // ceiling).
    {
        uint64_t window_ns = 100ull * SEC;
        RollingOffsetCluster c(window_ns, CB_CLUSTER_TOL_MS, CB_CLUSTER_MIN_MATCHED, CB_CLUSTER_MAX_MAD_MS);

        uint64_t t_ns = 0;
        CbAvOffset last;
        last.ok = false;
        for (size_t i = 0; i < CB_CLUSTER_MIN_MATCHED * 3; i++) {
            t_ns += SEC;
            last = c.push(t_ns, 0.0);
        }
        CHECK(last.ok, "must lock the tight batch first");
        uint64_t tight_last_ns = t_ns;

        uint64_t wide_ns = tight_last_ns + 50ull * SEC;
        size_t half = CB_CLUSTER_MIN_MATCHED;
        size_t other_half_minus_one = CB_CLUSTER_MIN_MATCHED - 1;
        for (size_t k = 0; k < half + other_half_minus_one; k++) {
            wide_ns += 10000000ull;
            double off = (k < half) ? -CB_CLUSTER_TOL_MS : CB_CLUSTER_TOL_MS;
            last = c.push(wide_ns, off);
        }
        CHECK(last.ok, "must still be locked heading into the eviction step");

        uint64_t jump_ns = tight_last_ns + window_ns + 20ull * SEC;
        CbAvOffset result = c.push(jump_ns, CB_CLUSTER_TOL_MS);
        CHECK(!result.ok, "#999: an excursion beyond the hold ceiling must still unlock, even from "
                          "a locked state");
    }

    // (4) The hold-ceiling hysteresis widens the MAD gate only -- matched dropping below its own
    // floor must still unlock, regardless of mad. Lock tight, age everything out, push exactly
    // ONE fresh tight sample (matched=1, mad=0 trivially): must unlock.
    {
        uint64_t window_ns = 10ull * SEC;
        RollingOffsetCluster c(window_ns, CB_CLUSTER_TOL_MS, CB_CLUSTER_MIN_MATCHED, CB_CLUSTER_MAX_MAD_MS);
        uint64_t t_ns = 0;
        CbAvOffset last;
        last.ok = false;
        for (size_t i = 0; i < CB_CLUSTER_MIN_MATCHED; i++) {
            t_ns += 100000000ull; // 0.1s
            last = c.push(t_ns, 0.0);
        }
        CHECK(last.ok, "must lock before the age-out step");

        uint64_t jump_ns = t_ns + window_ns + SEC;
        CbAvOffset result = c.push(jump_ns, 0.0);
        CHECK(!result.ok, "#999: matched dropping below min_matched must unlock even with mad_ms==0.0 "
                          "(the hold-ceiling hysteresis is MAD-only, never a matched override)");
    }

    if (g_failures == 0) {
        std::printf("av_sync_dock_lock_churn_999: ALL PASS\n");
        return 0;
    }
    std::printf("av_sync_dock_lock_churn_999: %d FAILURE(S)\n", g_failures);
    return 1;
}
"##;

#[test]
fn rolling_offset_cluster_lock_unlock_hysteresis_999() {
    let src_dir = manifest_dir().join("vendor/av-sync-dock/src");
    let header = src_dir.join("camera-box-audio.hpp");
    assert!(
        header.exists(),
        "#999: missing pure audio header {} — the A/V-sync dock's cluster/lock logic must live \
         there so it stays unit-testable off-rig.",
        header.display()
    );

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let work = std::env::temp_dir().join(format!(
        "av_sync_dock_lock_churn_999_{}_{}",
        std::process::id(),
        stamp
    ));
    std::fs::create_dir_all(&work).expect("create temp workdir");
    let src = work.join("harness.cpp");
    let bin = work.join("harness");
    std::fs::write(&src, HARNESS_CPP).expect("write C++ harness");

    let cxx = std::env::var("CXX").unwrap_or_else(|_| "c++".to_string());
    let compile = Command::new(&cxx)
        .arg("-std=c++17")
        .arg("-Wall")
        .arg("-I")
        .arg(&src_dir)
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| panic!("failed to invoke C++ compiler '{cxx}': {e}"));
    assert!(
        compile.status.success(),
        "#999: camera-box-audio.hpp (RollingOffsetCluster hysteresis) failed to compile with \
         {cxx}:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&bin).output().expect("run compiled harness");
    let _ = std::fs::remove_dir_all(&work); // best-effort cleanup
    assert!(
        run.status.success(),
        "#999: RollingOffsetCluster lock/unlock hysteresis harness FAILED (exit {:?}).\nstdout: \
         {}\nstderr: {}",
        run.status.code(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
}
