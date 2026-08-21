//! #789 (residual B / criterion 5) — OBS deploy/backup directory retention DECISION (pure, Tier-0).
//!
//! These tests pin the pure retention decision behind the standalone, dry-run-first sweep of the
//! deploy/backup directories the ONE fleet deploy path (`scripts/deploy-genlock-fleet.sh`) leaves
//! behind (`scripts/obs-backup-retention.ps1` + the imag leg of `scripts/obs-backup-retention.sh`
//! are faithful ports of the SAME rule). Two kinds accumulate and are NOT swept outside a deploy:
//! dated box-backup dirs `<stamp>-789` (win `C:\obs-backup`, imag `/opt/obs-backup`) and per-sha
//! stage dirs (`stage-genlock-<sha>` under `C:\`, `genlock-stage-<sha>` under `/tmp`). The sweep
//! keeps the newest N of EACH kind UNION anything younger than D days, and deletes ONLY dirs that
//! match the deploy's OWN naming allowlist.
//!
//! The single hardest invariant here (its own test below): a dir whose name does NOT match the
//! allowlist — the imag `previous/` rollback dir, an operator's own folder — is PROTECTED: it can
//! never land in the delete set, no matter how old.

use camera_box::obs_backup_retention::{
    classify_backup, is_dated_backup, is_stage_dir, plan, BackupDir, BackupKind, BackupPlan,
};
use camera_box::recordings_retention::{KeepReason, RetentionPolicy, SECONDS_PER_DAY};

fn d(name: &str, size_bytes: u64, mtime_epoch: f64) -> BackupDir {
    BackupDir {
        name: name.to_string(),
        size_bytes,
        mtime_epoch,
    }
}

// ---- the EXPLICIT allowlist (safety boundary) ---------------------------------------------

#[test]
fn allowlist_accepts_dated_backup_dirs() {
    // `<stamp>-789`, stamp = yyyy-MM-ddTHH-mm-ss (win Get-Date / imag date).
    assert!(is_dated_backup("2026-08-21T14-30-05-789"));
    assert!(is_dated_backup("2025-10-27T12-44-37-789"));
    assert_eq!(
        classify_backup("2026-08-21T14-30-05-789"),
        Some(BackupKind::DatedBackup)
    );
}

#[test]
fn allowlist_accepts_stage_dirs_both_shapes() {
    assert!(is_stage_dir("stage-genlock-a85f04d9c")); // win, under C:\
    assert!(is_stage_dir("genlock-stage-db544603a")); // imag, under /tmp
    assert!(is_stage_dir("stage-genlock-abc1234")); // short sha
    assert_eq!(
        classify_backup("stage-genlock-a85f04d9c"),
        Some(BackupKind::Stage)
    );
    assert_eq!(
        classify_backup("genlock-stage-db544603a"),
        Some(BackupKind::Stage)
    );
}

#[test]
fn allowlist_rejects_foreign_and_malformed_names() {
    // The imag rollback dir — a fixed, non-dated dir that MUST survive.
    assert!(!is_dated_backup("previous"));
    assert!(!is_stage_dir("previous"));
    assert_eq!(classify_backup("previous"), None);
    // An operator's own folder.
    assert_eq!(classify_backup("manual-obs-backup"), None);
    // Dated shape but wrong in one spot.
    assert!(!is_dated_backup("2026-08-21 14-30-05-789")); // space instead of T
    assert!(!is_dated_backup("2026-08-21T14-30-05")); // no -789 suffix
    assert!(!is_dated_backup("2026-08-21T14-30-05-788")); // wrong suffix
    assert!(!is_dated_backup("2026-8-1T4-3-5-789")); // not zero-padded (wrong length)
    assert!(!is_dated_backup("something-789")); // bare *-789 (the deploy's loose glob) is NOT enough
                                                // Stage shape but wrong sha.
    assert!(!is_stage_dir("stage-genlock-")); // empty sha
    assert!(!is_stage_dir("stage-genlock-NOTES")); // uppercase / non-hex
    assert!(!is_stage_dir("stage-genlock-xyz")); // non-hex letters
    assert!(!is_stage_dir("stage-genlock-a85f04d9c.zip")); // trailing non-hex
    assert!(!is_stage_dir("genlock-stage")); // no sha, no dash
}

// ---- the protected invariant (the whole point of an allowlist) ----------------------------

#[test]
fn non_matching_dirs_are_never_deleted() {
    // Foreign dirs, ancient and huge, can never be in the delete set regardless of policy.
    let now = 1_000_000.0;
    let dirs = vec![
        d("previous", 5_000_000, now - 999.0 * SECONDS_PER_DAY),
        d(
            "manual-obs-backup",
            9_000_000,
            now - 999.0 * SECONDS_PER_DAY,
        ),
        d("2020-01-01T00-00-00-789", 10, now - 999.0 * SECONDS_PER_DAY), // matching, will delete
    ];
    let p = plan(
        &dirs,
        &RetentionPolicy {
            keep_newest_runs: 0,
            keep_within_days: 0.0,
        },
        now,
    );
    let del: Vec<&str> = p.delete.iter().map(|x| x.name.as_str()).collect();
    assert_eq!(del, vec!["2020-01-01T00-00-00-789"]);
    assert!(!p.delete.iter().any(|x| x.name == "previous"));
    assert!(!p.delete.iter().any(|x| x.name == "manual-obs-backup"));
    assert_eq!(p.protected_count(), 2);
    // Both protected dirs are kept with the protected reason.
    for name in ["previous", "manual-obs-backup"] {
        let k = p.keep.iter().find(|k| k.dir.name == name).unwrap();
        assert_eq!(k.reason, KeepReason::ProtectedNonMatching);
    }
}

// ---- per-kind newest-N (a burst of one kind never evicts the other) -----------------------

#[test]
fn newest_n_is_applied_per_kind() {
    // keep_newest_runs = 2, no age horizon. Three dated + three stage. The two newest of EACH kind
    // survive; the oldest of EACH kind is deleted. A pile of stage dirs must NOT evict a dated one.
    let now = 1_000_000.0;
    let dirs = vec![
        d("2026-01-01T10-00-00-789", 10, now - 50.0 * SECONDS_PER_DAY), // dated, oldest -> delete
        d("2026-01-02T10-00-00-789", 10, now - 40.0 * SECONDS_PER_DAY), // dated -> keep
        d("2026-01-03T10-00-00-789", 10, now - 30.0 * SECONDS_PER_DAY), // dated, newest -> keep
        d("stage-genlock-aaa1111", 10, now - 6.0 * SECONDS_PER_DAY),    // stage, oldest -> delete
        d("stage-genlock-bbb2222", 10, now - 5.0 * SECONDS_PER_DAY),    // stage -> keep
        d("stage-genlock-ccc3333", 10, now - 4.0 * SECONDS_PER_DAY),    // stage, newest -> keep
    ];
    let p = plan(
        &dirs,
        &RetentionPolicy {
            keep_newest_runs: 2,
            keep_within_days: 0.0,
        },
        now,
    );
    let mut del: Vec<&str> = p.delete.iter().map(|x| x.name.as_str()).collect();
    del.sort_unstable();
    assert_eq!(
        del,
        vec!["2026-01-01T10-00-00-789", "stage-genlock-aaa1111"]
    );
    assert_eq!(p.delete_count(), 2);
    // The dated backup is kept even though three stage dirs are newer than it -> per-kind, not pooled.
    assert!(p
        .keep
        .iter()
        .any(|k| k.dir.name == "2026-01-02T10-00-00-789" && k.reason == KeepReason::NewestRuns));
}

// ---- younger-than-D union -----------------------------------------------------------------

#[test]
fn within_days_kept_even_beyond_newest_n() {
    // keep_newest_runs = 1 but keep_within_days = 25 -> mid dirs beyond the newest-1 survive by AGE.
    let now = 1_000_000.0;
    let dirs = vec![
        d("2026-01-01T10-00-00-789", 10, now - 40.0 * SECONDS_PER_DAY), // old -> delete
        d("2026-01-02T10-00-00-789", 10, now - 20.0 * SECONDS_PER_DAY), // within 25d -> keep (age)
        d("2026-01-03T10-00-00-789", 10, now - 1.0 * SECONDS_PER_DAY),  // newest + within -> keep
    ];
    let p = plan(
        &dirs,
        &RetentionPolicy {
            keep_newest_runs: 1,
            keep_within_days: 25.0,
        },
        now,
    );
    let del: Vec<&str> = p.delete.iter().map(|x| x.name.as_str()).collect();
    assert_eq!(del, vec!["2026-01-01T10-00-00-789"]);
    let newest = p
        .keep
        .iter()
        .find(|k| k.dir.name == "2026-01-03T10-00-00-789")
        .unwrap();
    assert_eq!(newest.reason, KeepReason::NewestRuns);
    let mid = p
        .keep
        .iter()
        .find(|k| k.dir.name == "2026-01-02T10-00-00-789")
        .unwrap();
    assert_eq!(mid.reason, KeepReason::WithinDays);
}

#[test]
fn age_exactly_keep_within_days_is_deleted_strict_boundary() {
    // Locks the STRICT `<` horizon (mirrored by `-lt` in the .ps1 / bash): a matching dir aged
    // EXACTLY keep_within_days, outside the newest-N, is DELETED.
    let now = 1_000_000.0;
    let dirs = vec![d(
        "2026-01-01T10-00-00-789",
        10,
        now - 3.0 * SECONDS_PER_DAY,
    )];
    let p = plan(
        &dirs,
        &RetentionPolicy {
            keep_newest_runs: 0,
            keep_within_days: 3.0,
        },
        now,
    );
    assert_eq!(p.delete_count(), 1);
    assert_eq!(p.keep.len(), 0);
}

// ---- reporting helpers --------------------------------------------------------------------

#[test]
fn byte_accounting_splits_delete_and_kept() {
    let now = 1_000_000.0;
    let dirs = vec![
        d("2026-01-01T10-00-00-789", 100, now - 50.0 * SECONDS_PER_DAY), // delete
        d("2026-01-02T10-00-00-789", 200, now - 1.0 * SECONDS_PER_DAY),  // keep (newest)
        d("previous", 400, now - 999.0 * SECONDS_PER_DAY),               // protected keep
    ];
    let p: BackupPlan = plan(
        &dirs,
        &RetentionPolicy {
            keep_newest_runs: 1,
            keep_within_days: 0.0,
        },
        now,
    );
    assert_eq!(p.bytes_to_delete(), 100);
    assert_eq!(p.bytes_kept(), 600); // 200 kept + 400 protected
    assert_eq!(p.protected_count(), 1);
}

#[test]
fn future_mtime_is_treated_as_young_and_kept() {
    // A dir with a future mtime (clock skew) has negative age -> kept by the age horizon, never
    // deleted, even outside the newest-N.
    let now = 1_000_000.0;
    let dirs = vec![d(
        "2099-01-01T10-00-00-789",
        10,
        now + 10.0 * SECONDS_PER_DAY,
    )];
    let p = plan(
        &dirs,
        &RetentionPolicy {
            keep_newest_runs: 0,
            keep_within_days: 1.0,
        },
        now,
    );
    assert_eq!(p.delete_count(), 0);
    assert_eq!(p.keep.len(), 1);
    assert_eq!(p.keep[0].reason, KeepReason::WithinDays);
}
