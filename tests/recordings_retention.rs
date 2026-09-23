//! #1122 — E2E recordings retention DECISION (pure, Tier-0).
//!
//! These tests pin the pure retention decision behind the dry-run-first cleanup sweep
//! (`scripts/strih-recordings-retention.ps1` is a faithful port of the SAME rule). The problem
//! being fixed: the E2E harness (`scripts/recording-e2e.sh`) records one OBS program capture per
//! run into each Windows box's live OBS record directory (strih: `C:\_REC` since 17.9.2026 — the D:
//! NVMe failed, issue 1338; historically `D:\_REC`; filename format `%CCYY-%MM-%DD %hh-%mm-%ss.mkv`),
//! and `[8/8e]` only ever deletes THAT run's own file — aborted
//! / skipped / failed-download runs leak forever (strih accumulated 344 `.mkv` = ~691 GiB, ~15x
//! the 50 GB budget). The retention pass keeps the newest N runs UNION anything younger than D
//! days, and deletes ONLY files that match the harness's OWN OBS-timestamp allowlist — NEVER a
//! generic `*.mkv` sweep that could eat a differently-named operator recording.
//!
//! The single hardest invariant here (its own test below): a file whose name does NOT match the
//! allowlist — proven concrete by the real `strih700105.mkv` seen in the strih record dir beside the
//! timestamp-named runs — is PROTECTED: it can never land in the delete set, no matter how old or
//! how large.

use camera_box::recordings_retention::{
    free_space_verdict, is_harness_recording, plan, FreeSpaceVerdict, KeepReason, RecordingFile,
    RetentionPolicy, PRODUCTION_SIZE_FLOOR_BYTES, SECONDS_PER_DAY,
};

fn f(name: &str, size_bytes: u64, mtime_epoch: f64) -> RecordingFile {
    RecordingFile {
        name: name.to_string(),
        size_bytes,
        mtime_epoch,
    }
}

// ---- the EXPLICIT allowlist (safety boundary) ---------------------------------------------

#[test]
fn allowlist_accepts_obs_timestamp_recordings() {
    // The exact OBS FilenameFormatting `%CCYY-%MM-%DD %hh-%mm-%ss` + a recording extension,
    // optionally with OBS's ` (n)` dedup suffix.
    assert!(is_harness_recording("2026-08-19 02-23-06.mkv"));
    assert!(is_harness_recording("2026-08-19 02-23-06.mp4"));
    assert!(is_harness_recording("2025-10-27 12-44-37.mkv"));
    assert!(is_harness_recording("2026-08-19 02-23-06 (2).mkv"));
    assert!(is_harness_recording("2026-08-19 02-23-06 (10).mp4"));
}

#[test]
fn allowlist_rejects_foreign_and_non_recording_files() {
    // The real operator/debug file that MUST survive — a generic `*.mkv` sweep would eat it.
    assert!(!is_harness_recording("strih700105.mkv"));
    // Screenshots + sidecar JSON that also live in the record dir.
    assert!(!is_harness_recording("Screenshot 2025-10-27 12-44-37.png"));
    assert!(!is_harness_recording("verdict-700105.json"));
    // Wrong / partial timestamp shapes.
    assert!(!is_harness_recording("2026-08-19.mkv")); // no time part
    assert!(!is_harness_recording("2026-8-9 2-3-6.mkv")); // not zero-padded
    assert!(!is_harness_recording("2026-08-19 02-23-06.txt")); // wrong extension
    assert!(!is_harness_recording("2026-08-19 02-23-06 (x).mkv")); // non-digit dedup suffix
    assert!(!is_harness_recording("2026-08-19 02-23-06(2).mkv")); // missing the space before "("
    assert!(!is_harness_recording("random.mkv"));
    assert!(!is_harness_recording("")); // empty
    assert!(!is_harness_recording("2026-08-19 02-23-06.MKV")); // upper-case ext (OBS writes lower)
}

// ---- the keep/delete decision -------------------------------------------------------------

#[test]
fn newest_n_runs_are_kept_rest_deleted() {
    // Four matching runs, all older than the day-horizon; keep the newest 2, delete the oldest 2.
    let now = 1_000_000.0;
    let files = vec![
        f("2026-01-01 10-00-00.mkv", 10, now - 40.0 * SECONDS_PER_DAY),
        f("2026-01-02 10-00-00.mkv", 10, now - 30.0 * SECONDS_PER_DAY),
        f("2026-01-03 10-00-00.mkv", 10, now - 20.0 * SECONDS_PER_DAY),
        f("2026-01-04 10-00-00.mkv", 10, now - 10.0 * SECONDS_PER_DAY),
    ];
    let p = plan(
        &files,
        &RetentionPolicy {
            keep_newest_runs: 2,
            keep_within_days: 0.0,
        },
        now,
    );
    let kept: Vec<&str> = p.keep.iter().map(|k| k.file.name.as_str()).collect();
    let del: Vec<&str> = p.delete.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(
        kept,
        vec!["2026-01-04 10-00-00.mkv", "2026-01-03 10-00-00.mkv"]
    );
    assert_eq!(
        del,
        vec!["2026-01-02 10-00-00.mkv", "2026-01-01 10-00-00.mkv"]
    );
    assert!(p.keep.iter().all(|k| k.reason == KeepReason::NewestRuns));
}

#[test]
fn within_days_kept_even_beyond_newest_n() {
    // keep_newest_runs = 1 but keep_within_days = 25 → the 3 files younger than 25d survive by
    // AGE even though only 1 is inside the newest-N. Union, not intersection.
    let now = 1_000_000.0;
    let files = vec![
        f("2026-01-01 10-00-00.mkv", 10, now - 40.0 * SECONDS_PER_DAY), // old → delete
        f("2026-01-02 10-00-00.mkv", 10, now - 20.0 * SECONDS_PER_DAY), // within 25d → keep
        f("2026-01-03 10-00-00.mkv", 10, now - 5.0 * SECONDS_PER_DAY),  // within 25d → keep
        f("2026-01-04 10-00-00.mkv", 10, now - 1.0 * SECONDS_PER_DAY),  // newest + within → keep
    ];
    let p = plan(
        &files,
        &RetentionPolicy {
            keep_newest_runs: 1,
            keep_within_days: 25.0,
        },
        now,
    );
    let del: Vec<&str> = p.delete.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(del, vec!["2026-01-01 10-00-00.mkv"]);
    // The newest file is kept for the newest-N reason; the two mid ones for the age reason.
    let newest = p
        .keep
        .iter()
        .find(|k| k.file.name == "2026-01-04 10-00-00.mkv")
        .unwrap();
    assert_eq!(newest.reason, KeepReason::NewestRuns);
    let mid = p
        .keep
        .iter()
        .find(|k| k.file.name == "2026-01-02 10-00-00.mkv")
        .unwrap();
    assert_eq!(mid.reason, KeepReason::WithinDays);
}

#[test]
fn age_exactly_keep_within_days_is_deleted_strict_boundary() {
    // Locks the STRICT `<` horizon (mirrored by the .ps1's `-lt`): a matching file aged EXACTLY
    // keep_within_days, and outside the newest-N, is DELETED, not kept. Guards against an
    // accidental flip to `<=` that would silently change the retention semantics.
    let now = 1_000_000.0;
    let files = vec![
        f("2026-01-01 10-00-00.mkv", 10, now - 3.0 * SECONDS_PER_DAY), // age == 3d horizon
    ];
    let p = plan(
        &files,
        &RetentionPolicy {
            keep_newest_runs: 0,
            keep_within_days: 3.0,
        },
        now,
    );
    let del: Vec<&str> = p.delete.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(del, vec!["2026-01-01 10-00-00.mkv"]);
    // And one epsilon younger IS kept (the strict-inequality's other side).
    let younger = vec![f(
        "2026-01-01 10-00-00.mkv",
        10,
        now - 3.0 * SECONDS_PER_DAY + 1.0,
    )];
    let p2 = plan(
        &younger,
        &RetentionPolicy {
            keep_newest_runs: 0,
            keep_within_days: 3.0,
        },
        now,
    );
    assert_eq!(p2.delete.len(), 0);
    assert_eq!(p2.keep[0].reason, KeepReason::WithinDays);
}

#[test]
fn within_newest_but_old_is_still_kept() {
    // A file inside the newest-N but OLDER than the day-horizon is kept (union) with the
    // newest-runs reason — the newest-N floor never expires.
    let now = 1_000_000.0;
    let files = vec![
        f("2026-01-01 10-00-00.mkv", 10, now - 400.0 * SECONDS_PER_DAY),
        f("2026-01-02 10-00-00.mkv", 10, now - 300.0 * SECONDS_PER_DAY),
    ];
    let p = plan(
        &files,
        &RetentionPolicy {
            keep_newest_runs: 5,
            keep_within_days: 3.0,
        },
        now,
    );
    assert_eq!(p.delete.len(), 0);
    assert!(p.keep.iter().all(|k| k.reason == KeepReason::NewestRuns));
}

#[test]
fn foreign_file_never_deleted_even_when_ancient_and_huge() {
    // THE safety invariant. `strih700105.mkv` is 400 GiB and 2 years old — the exact kind of file
    // an age/size-based generic sweep would delete first — yet it is PROTECTED (non-matching name)
    // and can NEVER be in the delete set. Only the timestamp-named runs are deletable.
    let now = 1_000_000.0;
    let files = vec![
        f(
            "strih700105.mkv",
            400 * 1024 * 1024 * 1024,
            now - 700.0 * SECONDS_PER_DAY,
        ),
        f("2026-01-01 10-00-00.mkv", 10, now - 700.0 * SECONDS_PER_DAY),
    ];
    let p = plan(
        &files,
        &RetentionPolicy {
            keep_newest_runs: 0,
            keep_within_days: 0.0,
        },
        now,
    );
    let del: Vec<&str> = p.delete.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(del, vec!["2026-01-01 10-00-00.mkv"]);
    assert!(!del.contains(&"strih700105.mkv"));
    let protected = p
        .keep
        .iter()
        .find(|k| k.file.name == "strih700105.mkv")
        .expect("foreign file must be kept");
    assert_eq!(protected.reason, KeepReason::ProtectedNonMatching);
    assert_eq!(p.protected_count(), 1);
}

#[test]
fn zero_n_zero_d_deletes_all_matching_keeps_foreign() {
    let now = 1_000_000.0;
    let files = vec![
        f("2026-01-01 10-00-00.mkv", 100, now - 1.0),
        f("2026-01-02 10-00-00.mp4", 200, now - 2.0),
        f("Screenshot 2026-01-01 10-00-00.png", 5, now - 3.0),
    ];
    let p = plan(
        &files,
        &RetentionPolicy {
            keep_newest_runs: 0,
            keep_within_days: 0.0,
        },
        now,
    );
    assert_eq!(p.delete_count(), 2);
    assert_eq!(p.bytes_to_delete(), 300);
    // The screenshot is non-matching → protected, never deleted.
    assert_eq!(p.protected_count(), 1);
    assert_eq!(p.bytes_kept(), 5);
}

#[test]
fn totals_sum_only_the_delete_set() {
    let now = 1_000_000.0;
    let files = vec![
        f(
            "2026-01-01 10-00-00.mkv",
            1000,
            now - 40.0 * SECONDS_PER_DAY,
        ), // delete
        f("2026-01-02 10-00-00.mkv", 2000, now - 1.0 * SECONDS_PER_DAY), // keep (newest)
        f("keepme-operator.mkv", 9999, now - 500.0 * SECONDS_PER_DAY),   // protected
    ];
    let p = plan(
        &files,
        &RetentionPolicy {
            keep_newest_runs: 1,
            keep_within_days: 0.0,
        },
        now,
    );
    assert_eq!(p.bytes_to_delete(), 1000);
    assert_eq!(p.bytes_kept(), 2000 + 9999);
    assert_eq!(p.delete_count(), 1);
}

#[test]
fn realistic_strih_scenario_protects_production_and_frees_old_e2e_runs() {
    // Live strih shape at small scale under the #1276 size-floor rule: a foreign operator file
    // (protected by NAME), two production-shaped recordings above the ~1 GiB floor (protected by
    // SIZE even though old and beyond newest-N), and a set of small E2E runs (below the floor) of
    // which only the newest-3 UNION younger-than-2-days survive. Under the OLD rank/age-only rule
    // the big old runs would have been the first freed; the ruling inverts that -- production files
    // are never deletable, only the small E2E captures are.
    let now = 1_000_000.0;
    let gib = 1024u64 * 1024 * 1024;
    let mib = 1024u64 * 1024;
    let mut files = vec![
        // foreign operator recording -- protected by NAME.
        f("strih700105.mkv", 5 * gib, now - 300.0 * SECONDS_PER_DAY),
        // production-shaped timestamp recordings -- above the floor, protected by SIZE despite age.
        f(
            "2026-02-01 10-00-00.mkv",
            17 * gib,
            now - 200.0 * SECONDS_PER_DAY,
        ),
        f(
            "2026-02-02 10-00-00.mkv",
            8 * gib,
            now - 150.0 * SECONDS_PER_DAY,
        ),
    ];
    // 8 small E2E runs (below the floor), oldest -> newest.
    for i in 0..8u64 {
        let age_days = (8 - i) as f64 * 5.0; // 40,35,...,5 days
        files.push(f(
            &format!("2026-01-{:02} 10-00-00.mkv", i + 1),
            200 * mib,
            now - age_days * SECONDS_PER_DAY,
        ));
    }
    let p = plan(
        &files,
        &RetentionPolicy {
            keep_newest_runs: 3,
            keep_within_days: 2.0,
        },
        now,
    );

    // Foreign file protected by NAME.
    let foreign = p
        .keep
        .iter()
        .find(|k| k.file.name == "strih700105.mkv")
        .expect("foreign file must be kept");
    assert_eq!(foreign.reason, KeepReason::ProtectedNonMatching);
    // Both production-shaped files protected by SIZE (never deleted), with the production-sized reason.
    for name in ["2026-02-01 10-00-00.mkv", "2026-02-02 10-00-00.mkv"] {
        assert!(!p.delete.iter().any(|d| d.name == name));
        let k = p
            .keep
            .iter()
            .find(|k| k.file.name == name)
            .expect("production-shaped file must be kept");
        assert_eq!(k.reason, KeepReason::ProductionSized);
    }
    // The oldest small E2E runs (beyond newest-3 and older than 2d) ARE freed.
    assert!(p.delete.iter().any(|d| d.name == "2026-01-01 10-00-00.mkv"));
    assert!(p.delete.iter().any(|d| d.name == "2026-01-02 10-00-00.mkv"));
    // The newest E2E run is kept (newest-3).
    assert!(p
        .keep
        .iter()
        .any(|k| k.file.name == "2026-01-08 10-00-00.mkv" && k.reason == KeepReason::NewestRuns));
    // ONLY below-floor E2E runs are ever in the delete set -- no production/foreign bytes freed.
    assert!(p
        .delete
        .iter()
        .all(|d| d.size_bytes < PRODUCTION_SIZE_FLOOR_BYTES));
    assert_eq!(p.delete_count(), 5);
}

// ---- #1276: free-space WARNING verdict (the E2E preflight semantics owner-ruled 2026-09-14) ----
//
// Owner ruling (14.9.2026, verbatim "B varovanie ma byt ked 50gb uz len ostava miesta!!!"): the
// E2E recordings-retention WARNING must fire when the recordings VOLUME has <= 50 GB of FREE space
// left, NOT when the sum of recording files exceeds a 50 GB budget. This pure verdict is the
// canonical spec behind that preflight; `bundle_state_gather.recordings_free_verdict` is its python
// mirror (the real runtime consumer the bash preflight calls). Threshold in decimal GB (1e9 bytes),
// the same unit the owner meant by "50gb" and the existing warning used. The DELETE-set decision
// (`plan()`) is untouched — the owner ruled only on the warning trigger.

const GB_1276: u64 = 1_000_000_000;

#[test]
fn free_space_at_or_above_threshold_is_ok_no_warn() {
    // Plenty of free space -> OK. (strih live: 619 GB free -> the old file-sum warning was a false
    // alarm; the new free-space warning correctly stays quiet.)
    assert_eq!(
        free_space_verdict(Some(619 * GB_1276), 50.0),
        FreeSpaceVerdict::Ok
    );
    assert_eq!(
        free_space_verdict(Some(51 * GB_1276), 50.0),
        FreeSpaceVerdict::Ok
    );
}

#[test]
fn free_space_exactly_at_threshold_is_ok_no_warn() {
    // Boundary: exactly 50 GB free -> OK (ticket spec: free >= 50 GB -> no warn).
    assert_eq!(
        free_space_verdict(Some(50 * GB_1276), 50.0),
        FreeSpaceVerdict::Ok
    );
}

#[test]
fn free_space_below_threshold_warns() {
    // < 50 GB free -> WARN (the owner's "50 GB already only remaining" signal).
    assert_eq!(
        free_space_verdict(Some(49 * GB_1276), 50.0),
        FreeSpaceVerdict::Warn
    );
    assert_eq!(free_space_verdict(Some(0), 50.0), FreeSpaceVerdict::Warn);
}

#[test]
fn free_space_unreadable_is_unknown_never_a_false_warn() {
    // An unreadable volume (free_bytes None) -> UNKNOWN, NEVER WARN — a false low-space warning
    // from an unreadable stat is worse than staying quiet (mirrors record_dir_stats's zero-degrade).
    assert_eq!(free_space_verdict(None, 50.0), FreeSpaceVerdict::Unknown);
}

#[test]
fn free_space_threshold_is_configurable() {
    // The threshold is a parameter (RECORDINGS_FREE_MIN_GB, env-overridable at the bash layer).
    assert_eq!(
        free_space_verdict(Some(80 * GB_1276), 100.0),
        FreeSpaceVerdict::Warn
    );
    assert_eq!(
        free_space_verdict(Some(120 * GB_1276), 100.0),
        FreeSpaceVerdict::Ok
    );
}

// ---- #1276: production-shaped recordings PROTECTED by a SIZE floor ------------------------------
//
// Owner ruling (15.9.2026, issue #1276 comment 5678041040): a production recording is PROTECTED by
// SIZE — any file at or above PRODUCTION_SIZE_FLOOR_BYTES (~1 GiB) is never in the delete set,
// regardless of age or newest-N rank. Only the small E2E-run files (0.0–0.8 GB in the 2.9. dry-run)
// stay eligible for deletion; production-shaped files were 5.6 / 7.9 / 17.3 GB. No archive step, no
// age-based deletion of production files. Below-floor files keep the newest-N ∪ younger-than-D rule.

const GB_DECIMAL_1276: u64 = 1_000_000_000;

#[test]
fn production_sized_file_is_protected_even_when_ancient_and_beyond_newest_n() {
    // A 17.3 GB matching run, older than everything and outside the newest-N — under the OLD
    // rank/age-only rule it would be deleted; now it is PROTECTED with the production-sized reason.
    let now = 1_000_000.0;
    let files = vec![
        f(
            "2026-01-01 10-00-00.mkv",
            17_300 * (GB_DECIMAL_1276 / 1000), // 17.3 GB
            now - 500.0 * SECONDS_PER_DAY,
        ),
        f("2026-01-02 10-00-00.mkv", 10, now - 1.0 * SECONDS_PER_DAY), // tiny newest run
    ];
    let p = plan(
        &files,
        &RetentionPolicy {
            keep_newest_runs: 1,
            keep_within_days: 0.0,
        },
        now,
    );
    // The big production-shaped file is NEVER deleted.
    assert!(!p.delete.iter().any(|d| d.name == "2026-01-01 10-00-00.mkv"));
    let prod = p
        .keep
        .iter()
        .find(|k| k.file.name == "2026-01-01 10-00-00.mkv")
        .expect("production-sized file must be kept");
    assert_eq!(prod.reason, KeepReason::ProductionSized);
}

#[test]
fn sub_floor_e2e_run_in_the_same_position_is_deleted() {
    // A 0.8 GB E2E-run file (below the ~1 GiB floor), same age/rank position as the protected 17.3
    // GB file above — this one IS deleted, proving the floor is what protects, not age/rank.
    let now = 1_000_000.0;
    let files = vec![
        f(
            "2026-01-01 10-00-00.mkv",
            800 * (GB_DECIMAL_1276 / 1000), // 0.8 GB — the E2E-run max
            now - 500.0 * SECONDS_PER_DAY,
        ),
        f("2026-01-02 10-00-00.mkv", 10, now - 1.0 * SECONDS_PER_DAY),
    ];
    let p = plan(
        &files,
        &RetentionPolicy {
            keep_newest_runs: 1,
            keep_within_days: 0.0,
        },
        now,
    );
    assert!(p.delete.iter().any(|d| d.name == "2026-01-01 10-00-00.mkv"));
}

#[test]
fn file_exactly_at_the_floor_is_protected_inclusive_boundary() {
    // Boundary: size == PRODUCTION_SIZE_FLOOR_BYTES is PROTECTED (the ruling's "at or above").
    let now = 1_000_000.0;
    let files = vec![f(
        "2026-01-01 10-00-00.mkv",
        PRODUCTION_SIZE_FLOOR_BYTES,
        now - 500.0 * SECONDS_PER_DAY,
    )];
    let p = plan(
        &files,
        &RetentionPolicy {
            keep_newest_runs: 0,
            keep_within_days: 0.0,
        },
        now,
    );
    assert_eq!(p.delete.len(), 0);
    assert_eq!(p.keep[0].reason, KeepReason::ProductionSized);
    // One byte below the floor, same position, IS deleted (the other side of the inclusive floor).
    let below = vec![f(
        "2026-01-01 10-00-00.mkv",
        PRODUCTION_SIZE_FLOOR_BYTES - 1,
        now - 500.0 * SECONDS_PER_DAY,
    )];
    let p2 = plan(
        &below,
        &RetentionPolicy {
            keep_newest_runs: 0,
            keep_within_days: 0.0,
        },
        now,
    );
    assert_eq!(p2.delete.len(), 1);
}

#[test]
fn production_sized_never_deletable_takes_precedence_over_delete_eligibility() {
    // A production-sized file that is BOTH matching-named AND rank/age-eligible for deletion still
    // lands in keep (production-sized wins), while a sub-floor sibling in the same cohort is freed.
    let now = 1_000_000.0;
    let files = vec![
        f(
            "2026-01-01 10-00-00.mkv",
            7_900 * (GB_DECIMAL_1276 / 1000), // 7.9 GB production-shaped
            now - 100.0 * SECONDS_PER_DAY,
        ),
        f(
            "2026-01-02 10-00-00.mkv",
            200 * (GB_DECIMAL_1276 / 1000), // 0.2 GB E2E run
            now - 100.0 * SECONDS_PER_DAY,
        ),
    ];
    let p = plan(
        &files,
        &RetentionPolicy {
            keep_newest_runs: 0,
            keep_within_days: 0.0,
        },
        now,
    );
    let del: Vec<&str> = p.delete.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(del, vec!["2026-01-02 10-00-00.mkv"]);
    assert!(p.keep.iter().any(
        |k| k.file.name == "2026-01-01 10-00-00.mkv" && k.reason == KeepReason::ProductionSized
    ));
}

// ---- issue 1317 part 5: ONE shared fixture table pins the Linux (bash) executor to plan() ---------
//
// strih-lx (Linux) records E2E runs to `/srv/_REC`, and its retention executor is a bash port of
// this decision (`scripts/strih-recordings-retention.sh --local-sweep`). The parity is pinned on ONE
// table, `tests/fixtures/recordings_retention_parity.tsv`, read by THIS test against the canonical
// `plan()` AND by `tests/python/test_strih_lx_recordings_retention_1317.py` against the bash
// decision over a real fixture directory. Both sides assert the SAME expected keep/delete sets, so
// any drift in either implementation fails its own side against the shared table.

const PARITY_TABLE_1317: &str = include_str!("fixtures/recordings_retention_parity.tsv");

struct ParityCase1317 {
    id: String,
    now: f64,
    policy: RetentionPolicy,
    files: Vec<RecordingFile>,
    keep: Vec<(String, String)>,
    delete: Vec<String>,
}

fn bad_1317(lineno: usize, line: &str) -> ! {
    panic!("parity table line {}: malformed: {line:?}", lineno + 1)
}

fn parse_parity_table_1317() -> Vec<ParityCase1317> {
    let mut cases = Vec::new();
    let mut cur: Option<ParityCase1317> = None;
    for (lineno, line) in PARITY_TABLE_1317.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        match fields[0] {
            "case" => {
                assert!(cur.is_none(), "line {}: case inside a case", lineno + 1);
                if fields.len() != 5 {
                    bad_1317(lineno, line);
                }
                cur = Some(ParityCase1317 {
                    id: fields[1].to_string(),
                    now: fields[2].parse().unwrap_or_else(|_| bad_1317(lineno, line)),
                    policy: RetentionPolicy {
                        keep_newest_runs: fields[3]
                            .parse()
                            .unwrap_or_else(|_| bad_1317(lineno, line)),
                        keep_within_days: fields[4]
                            .parse()
                            .unwrap_or_else(|_| bad_1317(lineno, line)),
                    },
                    files: Vec::new(),
                    keep: Vec::new(),
                    delete: Vec::new(),
                });
            }
            "file" => {
                let c = cur.as_mut().unwrap_or_else(|| bad_1317(lineno, line));
                if fields.len() != 4 {
                    bad_1317(lineno, line);
                }
                let age: f64 = fields[1].parse().unwrap_or_else(|_| bad_1317(lineno, line));
                let size: u64 = fields[2].parse().unwrap_or_else(|_| bad_1317(lineno, line));
                c.files.push(f(fields[3], size, c.now - age));
            }
            "keep" => {
                let c = cur.as_mut().unwrap_or_else(|| bad_1317(lineno, line));
                if fields.len() != 3 {
                    bad_1317(lineno, line);
                }
                c.keep.push((fields[1].to_string(), fields[2].to_string()));
            }
            "delete" => {
                let c = cur.as_mut().unwrap_or_else(|| bad_1317(lineno, line));
                if fields.len() != 2 {
                    bad_1317(lineno, line);
                }
                c.delete.push(fields[1].to_string());
            }
            "end" => cases.push(cur.take().unwrap_or_else(|| bad_1317(lineno, line))),
            _ => bad_1317(lineno, line),
        }
    }
    assert!(cur.is_none(), "parity table ends inside a case");
    cases
}

/// The reason token the bash executor (and the `.ps1`) print for each `KeepReason`.
fn reason_token_1317(r: KeepReason) -> &'static str {
    match r {
        KeepReason::ProtectedNonMatching => "protected",
        KeepReason::ProductionSized => "production-sized",
        KeepReason::NewestRuns => "newest-run",
        KeepReason::WithinDays => "within-days",
    }
}

#[test]
fn shared_parity_table_matches_the_canonical_plan_1317() {
    let cases = parse_parity_table_1317();
    assert!(
        cases.len() >= 10,
        "the shared parity table lost cases ({})",
        cases.len()
    );
    for c in &cases {
        let p = plan(&c.files, &c.policy, c.now);
        let mut got_keep: Vec<(String, String)> = p
            .keep
            .iter()
            .map(|k| (reason_token_1317(k.reason).to_string(), k.file.name.clone()))
            .collect();
        got_keep.sort();
        let mut want_keep = c.keep.clone();
        want_keep.sort();
        assert_eq!(got_keep, want_keep, "case {}: KEEP set", c.id);
        let got_delete: Vec<String> = p.delete.iter().map(|d| d.name.clone()).collect();
        assert_eq!(
            got_delete, c.delete,
            "case {}: DELETE list (plan order)",
            c.id
        );
        // Every input file lands in exactly one of the two sets.
        assert_eq!(
            p.keep.len() + p.delete.len(),
            c.files.len(),
            "case {}: a file was dropped or duplicated",
            c.id
        );
    }
}
