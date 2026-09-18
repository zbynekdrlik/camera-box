//! #1122 — E2E recordings retention decision (pure, dependency-free, Tier-0 testable).
//!
//! The camera-box E2E harness (`scripts/recording-e2e.sh`) records ONE OBS program capture per run
//! into each Windows box's LIVE OBS record directory (strih: `C:\_REC` since 17.9.2026 — the D: NVMe
//! dropped off the bus and the owner left recordings on the C: system disk, issue 1338; historically
//! `D:\_REC`; OBS filename format `%CCYY-%MM-%DD %hh-%mm-%ss` → e.g. `2026-08-19 02-23-06.mkv`).
//! `[8/8e]` only ever prints a
//! delete plan for THAT run's own file, and the `#652` preflight merely WARNs — so aborted /
//! `KEEP_RECORDINGS=1` / early-abort / failed-download runs leak forever. Live strih (2026-08-19):
//! 344 `.mkv` = ~691 GiB in `D:\_REC`, oldest back to 2025-10-27, ~15× the 50 GB working budget.
//!
//! This module is the PURE decision behind a dry-run-first retention sweep: given the record
//! directory's top-level files, KEEP the newest `keep_newest_runs` runs UNION anything younger
//! than `keep_within_days`, and DELETE only files matching the harness's OWN OBS-timestamp
//! filename allowlist. It is deliberately NOT a generic `*.mkv` sweep: a differently-named
//! operator/debug recording (proven concrete: `strih700105.mkv` seen in the strih record dir) is
//! PROTECTED and can never be deleted, no matter how old or large.
//!
//! #1276 (owner ruling 15.9.2026): production-shaped recordings are ADDITIONALLY protected by SIZE
//! -- any recording at or above `PRODUCTION_SIZE_FLOOR_BYTES` (~1 GiB) is kept regardless of age or
//! newest-N rank; only the small E2E-run files stay eligible for the DELETE set.
//!
//! PARITY: `scripts/strih-recordings-retention.ps1` is a faithful port of THIS decision (same
//! allowlist shape + newest-N ∪ younger-than-D rule). This module + `tests/recordings_retention.rs`
//! are the canonical spec — keep the PowerShell mirror in sync with them.

/// One top-level file in the record directory: name plus size and mtime (epoch seconds).
#[derive(Debug, Clone, PartialEq)]
pub struct RecordingFile {
    pub name: String,
    pub size_bytes: u64,
    pub mtime_epoch: f64,
}

/// Retention policy: keep the newest `keep_newest_runs` matching files (by mtime, newest first)
/// UNION any matching file younger than `keep_within_days`.
#[derive(Debug, Clone, Copy)]
pub struct RetentionPolicy {
    pub keep_newest_runs: usize,
    pub keep_within_days: f64,
}

/// Why a file landed in the KEEP set (surfaced in the printed plan).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepReason {
    /// Filename does NOT match the harness allowlist — a foreign/operator file, never deletable.
    ProtectedNonMatching,
    /// Among the newest-N matching runs.
    NewestRuns,
    /// Younger than the keep-within-days horizon.
    WithinDays,
    /// #1276 -- size is at or above `PRODUCTION_SIZE_FLOOR_BYTES`: a production-shaped recording,
    /// PROTECTED by SIZE regardless of age or newest-N rank (owner ruling 15.9.2026, issue 1276).
    ProductionSized,
}

/// A kept file plus the reason it was kept.
#[derive(Debug, Clone)]
pub struct KeptEntry {
    pub file: RecordingFile,
    pub reason: KeepReason,
}

/// The full retention plan: what to keep (with reasons) and what to delete.
#[derive(Debug, Clone)]
pub struct RetentionPlan {
    pub keep: Vec<KeptEntry>,
    pub delete: Vec<RecordingFile>,
}

impl RetentionPlan {
    /// Total bytes across the DELETE set only.
    pub fn bytes_to_delete(&self) -> u64 {
        self.delete.iter().map(|f| f.size_bytes).sum()
    }

    /// Total bytes across everything KEPT (matching kept + protected).
    pub fn bytes_kept(&self) -> u64 {
        self.keep.iter().map(|k| k.file.size_bytes).sum()
    }

    /// Number of files scheduled for deletion.
    pub fn delete_count(&self) -> usize {
        self.delete.len()
    }

    /// Number of protected (non-matching) files that were kept untouched.
    pub fn protected_count(&self) -> usize {
        self.keep
            .iter()
            .filter(|k| k.reason == KeepReason::ProtectedNonMatching)
            .count()
    }
}

/// Seconds in a day, for turning `keep_within_days` into an age horizon.
pub const SECONDS_PER_DAY: f64 = 86_400.0;

/// #1276 -- production-size PROTECT floor (bytes). Any matching recording whose size is at or above
/// this value is a production-shaped recording and is PROTECTED (kept, never deleted) regardless of
/// age or newest-N rank -- owner ruling 15.9.2026 (issue 1276). Calibration from the 2.9. strih
/// dry-run: E2E-run captures were 0.0-0.8 GB; real production recordings were 5.6 / 7.9 / 17.3 GB.
/// The floor is ONE GiB (1_073_741_824 B) -- comfortably above the 0.8 GB E2E max and well below the
/// 5.6 GB smallest production file, so it separates the two populations with a wide margin. Only
/// below-floor E2E-run files stay eligible for the newest-N UNION younger-than-D DELETE rule. The
/// PowerShell mirror `strih-recordings-retention.ps1` carries the byte-identical value.
pub const PRODUCTION_SIZE_FLOOR_BYTES: u64 = 1_073_741_824;

/// The EXPLICIT allowlist. Does `name` match OBS's `%CCYY-%MM-%DD %hh-%mm-%ss` FilenameFormatting
/// with a `.mkv`/`.mp4` recording extension and an OPTIONAL OBS ` (n)` dedup suffix? i.e.
/// `YYYY-MM-DD HH-MM-SS.mkv`, `YYYY-MM-DD HH-MM-SS.mp4`, `YYYY-MM-DD HH-MM-SS (2).mkv`.
///
/// Everything else — screenshots (`Screenshot …png`), sidecar `.json`, and any custom operator
/// name like `strih700105.mkv` — returns `false` and is therefore PROTECTED. Case-sensitive on the
/// extension (OBS writes lowercase); a non-digit dedup suffix or a missing space before `(` also
/// fails. Dependency-free by design (no `regex`) so it compiles on default features at the crate
/// root and stays trivially auditable.
pub fn is_harness_recording(name: &str) -> bool {
    let stem = match name
        .strip_suffix(".mkv")
        .or_else(|| name.strip_suffix(".mp4"))
    {
        Some(s) => s,
        None => return false,
    };
    // Optionally strip a trailing OBS dedup suffix ` (n)` (n = one or more ASCII digits); the
    // core that remains must be exactly the timestamp.
    let core = strip_dedup_suffix(stem).unwrap_or(stem);
    is_obs_timestamp(core)
}

/// If `s` ends with ` (n)` where n is one-or-more ASCII digits, return the part before it;
/// otherwise `None` (no recognised suffix).
fn strip_dedup_suffix(s: &str) -> Option<&str> {
    let without_close = s.strip_suffix(')')?;
    let open = without_close.rfind(" (")?;
    let digits = &without_close[open + 2..];
    if !digits.is_empty() && digits.bytes().all(|c| c.is_ascii_digit()) {
        Some(&without_close[..open])
    } else {
        None
    }
}

/// Exactly `YYYY-MM-DD HH-MM-SS` (19 chars, ASCII digits where a digit is required, literal
/// separators everywhere else). Purely structural — it does NOT validate that the month/day/time
/// are in range, since OBS itself only ever emits valid stamps and an out-of-range-but-shaped name
/// would still be a harness recording.
fn is_obs_timestamp(s: &str) -> bool {
    const MASK: &[u8] = b"DDDD-DD-DD DD-DD-DD";
    let b = s.as_bytes();
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

/// Compute the retention plan. Non-matching files are always kept (PROTECTED). Matching files are
/// sorted newest-first (mtime desc, name as a deterministic tie-break) and kept when they are in
/// the newest `keep_newest_runs` OR younger than `keep_within_days` (union); the rest are deleted.
pub fn plan(files: &[RecordingFile], policy: &RetentionPolicy, now_epoch: f64) -> RetentionPlan {
    let horizon = policy.keep_within_days * SECONDS_PER_DAY;

    let mut keep: Vec<KeptEntry> = Vec::new();
    let mut matching: Vec<&RecordingFile> = Vec::new();
    for file in files {
        if is_harness_recording(&file.name) {
            matching.push(file);
        } else {
            keep.push(KeptEntry {
                file: file.clone(),
                reason: KeepReason::ProtectedNonMatching,
            });
        }
    }

    // Newest first; stable, deterministic tie-break by name so a plan is reproducible.
    matching.sort_by(|a, b| {
        b.mtime_epoch
            .partial_cmp(&a.mtime_epoch)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.name.cmp(&b.name))
    });

    // #1276: production-shaped recordings (size at or above PRODUCTION_SIZE_FLOOR_BYTES) are
    // PROTECTED regardless of age or newest-N rank (owner ruling 15.9.2026). Only the remaining
    // below-floor E2E-run files run through the newest-N ∪ younger-than-D eligibility below, so a
    // production recording never consumes a newest-N slot meant for the small E2E captures.
    let mut below_floor: Vec<&RecordingFile> = Vec::new();
    for file in &matching {
        if file.size_bytes >= PRODUCTION_SIZE_FLOOR_BYTES {
            keep.push(KeptEntry {
                file: (*file).clone(),
                reason: KeepReason::ProductionSized,
            });
        } else {
            below_floor.push(*file);
        }
    }

    let mut delete: Vec<RecordingFile> = Vec::new();
    for (idx, file) in below_floor.iter().enumerate() {
        let within_newest = idx < policy.keep_newest_runs;
        // age >= 0 for a past mtime; a future mtime (age < 0) is treated as young → kept.
        let within_days = policy.keep_within_days > 0.0 && (now_epoch - file.mtime_epoch) < horizon;
        if within_newest {
            keep.push(KeptEntry {
                file: (*file).clone(),
                reason: KeepReason::NewestRuns,
            });
        } else if within_days {
            keep.push(KeptEntry {
                file: (*file).clone(),
                reason: KeepReason::WithinDays,
            });
        } else {
            delete.push((*file).clone());
        }
    }

    RetentionPlan { keep, delete }
}

/// #1276 — the E2E recordings-retention free-space WARNING verdict.
///
/// Owner ruling (14.9.2026, verbatim "B varovanie ma byt ked 50gb uz len ostava miesta!!!"): the
/// E2E preflight WARN must fire when the recordings VOLUME has at most `min_free_gb` of FREE space
/// left — NOT when the sum of recording files exceeds a budget. A disk with 600 GB free must not
/// warn just because old test recordings sum past 50 GB. This Rust decision is the reference spec
/// (Tier-0-tested here); the ACTUAL runtime consumer is the byte-identical python mirror
/// `bundle_state_gather.recordings_free_verdict`, which the bash preflight calls (the free-space
/// read happens on-box in python, so there is no Rust runtime call site). The DELETE-set decision
/// (`plan()`) is untouched — the owner ruled only on the warning trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreeSpaceVerdict {
    /// At least `min_free_gb` of free space — no warning.
    Ok,
    /// Below `min_free_gb` of free space left — warn (space is running low).
    Warn,
    /// Free space could not be read (`free_bytes` None) — UNKNOWN, never a false low-space WARN.
    Unknown,
}

/// Decimal gigabytes (1e9 bytes) — the same unit the existing warning and the owner's "50gb"
/// meant, so the threshold is compared in decimal GB, not GiB.
pub const BYTES_PER_GB: f64 = 1e9;

/// Verdict for the recordings volume's free space. `free_bytes` is the volume's free space (from
/// `shutil.disk_usage(record_dir).free` on the box), or `None` when it could not be read. WARN iff
/// the free space is STRICTLY below `min_free_gb` (so exactly `min_free_gb` free is still Ok — the
/// spec's "free >= threshold -> no warn"). `None` -> Unknown (never a false warn from a bad read).
pub fn free_space_verdict(free_bytes: Option<u64>, min_free_gb: f64) -> FreeSpaceVerdict {
    match free_bytes {
        None => FreeSpaceVerdict::Unknown,
        Some(fb) => {
            let free_gb = fb as f64 / BYTES_PER_GB;
            if free_gb < min_free_gb {
                FreeSpaceVerdict::Warn
            } else {
                FreeSpaceVerdict::Ok
            }
        }
    }
}
