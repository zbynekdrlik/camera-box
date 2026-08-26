//! #789 (residual B / criterion 5) — OBS deploy/backup directory retention decision (pure, Tier-0).
//!
//! The ONE canonical fleet deploy path (`scripts/deploy-genlock-fleet.sh`) leaves two kinds of
//! directory behind on every box, and neither is swept outside a deploy:
//!
//!   * a DATED box-backup dir per deploy — `<stamp>-789` under the box-backup root (strih/stream
//!     `C:\obs-backup`, imag `/opt/obs-backup`); `$stamp` is `yyyy-MM-ddTHH-mm-ss` (win
//!     `Get-Date -Format`) / `%Y-%m-%dT%H-%M-%S` (imag `date`), e.g. `2026-08-21T14-30-05-789`.
//!     The deploy program prunes these to the newest `RETENTION_KEEP=3` — but ONLY inline during a
//!     deploy, and ONLY when `--yes` is passed.
//!   * a STAGE dir per deployed sha — `stage-genlock-<sha>` under `C:\` (win) / `genlock-stage-<sha>`
//!     under `/tmp` (imag). These are NEVER pruned: the win stage grows one-per-sha forever, the
//!     imag stage is `rm -rf`'d only for the CURRENT sha so older shas linger until reboot.
//!
//! criterion 5 of #789 ("deploy-*/obs-backup-* adresáre na retenciu, zvyšok preč") wants a
//! STANDALONE, supervisor-invocable, dry-run-first sweep for BOTH — the same shape as #1122's
//! recordings retention. This module is the PURE decision behind it: keep the newest
//! `keep_newest_runs` dirs PER KIND, UNION anything younger than `keep_within_days`, and DELETE
//! only dirs whose name matches the deploy's OWN naming allowlist. It is deliberately NOT a generic
//! sweep: a differently-named dir — the imag `previous/` rollback dir, an operator's own folder — is
//! PROTECTED and can never be deleted, no matter how old.
//!
//! It reuses the neutral value types from `recordings_retention` (`RetentionPolicy`, `KeepReason`,
//! `SECONDS_PER_DAY`) so the retention vocabulary stays shared; the allowlist + per-kind grouping
//! are backup-specific. PARITY: `scripts/obs-backup-retention.ps1` (strih/stream) and the imag leg
//! of `scripts/obs-backup-retention.sh` are faithful ports of THIS decision — this module +
//! `tests/obs_backup_retention.rs` are the canonical spec; keep the mirrors in sync.

use crate::recordings_retention::{KeepReason, RetentionPolicy, SECONDS_PER_DAY};

/// Which deploy artifact a matching directory is — retention keeps the newest-N of EACH kind
/// separately (so a burst of stage dirs never evicts a recent dated backup, or vice versa).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupKind {
    /// A dated box-backup dir `<stamp>-789` under the box-backup root.
    DatedBackup,
    /// A per-sha genlock staging dir (`stage-genlock-<sha>` / `genlock-stage-<sha>`).
    Stage,
}

/// One top-level directory under a swept parent: name plus its recursive byte total (reporting
/// only — never part of the keep/delete decision) and mtime (epoch seconds).
#[derive(Debug, Clone, PartialEq)]
pub struct BackupDir {
    pub name: String,
    pub size_bytes: u64,
    pub mtime_epoch: f64,
}

/// A kept directory plus the reason it was kept.
#[derive(Debug, Clone)]
pub struct BackupKept {
    pub dir: BackupDir,
    pub reason: KeepReason,
}

/// The full retention plan: what to keep (with reasons) and what to delete.
#[derive(Debug, Clone)]
pub struct BackupPlan {
    pub keep: Vec<BackupKept>,
    pub delete: Vec<BackupDir>,
}

impl BackupPlan {
    /// Total bytes across the DELETE set only.
    pub fn bytes_to_delete(&self) -> u64 {
        self.delete.iter().map(|d| d.size_bytes).sum()
    }

    /// Total bytes across everything KEPT (matching kept + protected).
    pub fn bytes_kept(&self) -> u64 {
        self.keep.iter().map(|k| k.dir.size_bytes).sum()
    }

    /// Number of directories scheduled for deletion.
    pub fn delete_count(&self) -> usize {
        self.delete.len()
    }

    /// Number of protected (non-matching) directories kept untouched.
    pub fn protected_count(&self) -> usize {
        self.keep
            .iter()
            .filter(|k| k.reason == KeepReason::ProtectedNonMatching)
            .count()
    }
}

/// Does `name` look like the deploy's DATED box-backup dir, exactly `<stamp>-789` where `<stamp>`
/// is `YYYY-MM-DDTHH-MM-SS` (ISO-ish, the shape win `Get-Date -Format 'yyyy-MM-ddTHH-mm-ss'` and
/// imag `date +%Y-%m-%dT%H-%M-%S` both emit)? Purely structural (digits where a digit is required,
/// literal separators everywhere else) — it does NOT range-check the fields, since the deploy only
/// ever emits valid stamps. Stricter than the deploy's own inline `*-789` glob, so a foreign
/// `something-789` dir stays PROTECTED.
pub fn is_dated_backup(name: &str) -> bool {
    // DDDD-DD-DDTDD-DD-DD-789
    const MASK: &[u8] = b"DDDD-DD-DDTDD-DD-DD-789";
    let b = name.as_bytes();
    if b.len() != MASK.len() {
        return false;
    }
    for (c, m) in b.iter().zip(MASK) {
        match m {
            b'D' => {
                if !c.is_ascii_digit() {
                    return false;
                }
            }
            _ => {
                if c != m {
                    return false;
                }
            }
        }
    }
    true
}

/// Does `name` look like a per-sha genlock STAGE dir — `stage-genlock-<sha>` (win, under `C:\`) or
/// `genlock-stage-<sha>` (imag, under `/tmp`) — where `<sha>` is a non-empty lowercase-hex git sha?
/// Lowercase-hex only (git writes lowercase), so an unrelated `stage-genlock-notes` dir is
/// PROTECTED.
pub fn is_stage_dir(name: &str) -> bool {
    let sha = match name
        .strip_prefix("stage-genlock-")
        .or_else(|| name.strip_prefix("genlock-stage-"))
    {
        Some(s) => s,
        None => return false,
    };
    !sha.is_empty() && sha.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

/// Classify a directory name against the deploy allowlist. `None` = PROTECTED (never deletable).
pub fn classify_backup(name: &str) -> Option<BackupKind> {
    if is_dated_backup(name) {
        Some(BackupKind::DatedBackup)
    } else if is_stage_dir(name) {
        Some(BackupKind::Stage)
    } else {
        None
    }
}

/// Compute the retention plan. Non-matching dirs are always kept (PROTECTED). Matching dirs are
/// grouped BY KIND; within each kind they are sorted newest-first (mtime desc, name as a
/// deterministic tie-break) and kept when they are in the newest `keep_newest_runs` OR younger than
/// `keep_within_days` (union); the rest are deleted. Keeping newest-N per kind means a burst of one
/// kind never evicts a recent dir of the other.
pub fn plan(dirs: &[BackupDir], policy: &RetentionPolicy, now_epoch: f64) -> BackupPlan {
    let horizon = policy.keep_within_days * SECONDS_PER_DAY;

    let mut keep: Vec<BackupKept> = Vec::new();
    let mut dated: Vec<&BackupDir> = Vec::new();
    let mut stage: Vec<&BackupDir> = Vec::new();
    for dir in dirs {
        match classify_backup(&dir.name) {
            Some(BackupKind::DatedBackup) => dated.push(dir),
            Some(BackupKind::Stage) => stage.push(dir),
            None => keep.push(BackupKept {
                dir: dir.clone(),
                reason: KeepReason::ProtectedNonMatching,
            }),
        }
    }

    let mut delete: Vec<BackupDir> = Vec::new();
    for group in [&mut dated, &mut stage] {
        // Newest first; stable, deterministic tie-break by name so a plan is reproducible.
        group.sort_by(|a, b| {
            b.mtime_epoch
                .partial_cmp(&a.mtime_epoch)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.name.cmp(&b.name))
        });
        for (idx, dir) in group.iter().enumerate() {
            let within_newest = idx < policy.keep_newest_runs;
            // age >= 0 for a past mtime; a future mtime (age < 0) is treated as young -> kept.
            let within_days =
                policy.keep_within_days > 0.0 && (now_epoch - dir.mtime_epoch) < horizon;
            if within_newest {
                keep.push(BackupKept {
                    dir: (*dir).clone(),
                    reason: KeepReason::NewestRuns,
                });
            } else if within_days {
                keep.push(BackupKept {
                    dir: (*dir).clone(),
                    reason: KeepReason::WithinDays,
                });
            } else {
                delete.push((*dir).clone());
            }
        }
    }

    BackupPlan { keep, delete }
}
