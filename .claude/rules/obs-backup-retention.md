---
paths:
  - "scripts/obs-backup-retention.ps1"
  - "scripts/obs-backup-retention.sh"
  - "src/obs_backup_retention.rs"
  - "tests/obs_backup_retention.rs"
---

# OBS deploy/backup directory retention — dry-run-first sweep (#789 residual B / criterion 5)

## Why

The ONE canonical fleet deploy path (`scripts/deploy-genlock-fleet.sh`) leaves two kinds of
directory behind on every box, and **neither is swept outside a deploy**:

- **dated box-backup dirs** `<stamp>-789` under the box-backup root — strih/stream `C:\obs-backup`,
  imag `/opt/obs-backup`. `$stamp` = `yyyy-MM-ddTHH-mm-ss` (win `Get-Date -Format`) /
  `%Y-%m-%dT%H-%M-%S` (imag `date`), e.g. `2026-08-21T14-30-05-789`. The deploy program
  (`build_windows_deploy_program` step 7 / `build_imag_deploy_program` step 6) prunes these to the
  newest `RETENTION_KEEP=3` — but ONLY inline during a deploy, and ONLY when `--yes` is passed.
- **per-sha stage dirs** — `stage-genlock-<sha>` under `C:\` (win) / `genlock-stage-<sha>` under
  `/tmp` (imag). NEVER pruned: the win stage grows one-per-sha forever; the imag stage is `rm -rf`'d
  only for the CURRENT sha, so older shas linger until reboot.

criterion 5 of #789 wants a STANDALONE, supervisor-invocable, dry-run-first sweep for BOTH — the same
shape as issue 1122's recordings retention (a DIFFERENT artifact: `D:\_REC` `.mkv` recordings). The
inline `--yes` retention in the deploy program is left UNTOUCHED; this standalone sweep is a
superset (adds the stage dirs, a younger-than-D union, and a deploy-free path).

## The decision (keep newest-N per kind UNION younger-than-D-days)

Canonical spec: **`src/obs_backup_retention.rs`** (pure, Tier-0, `tests/obs_backup_retention.rs`).
`scripts/obs-backup-retention.ps1` (win) and the `--local-sweep` bash decision in
`scripts/obs-backup-retention.sh` (imag) are FAITHFUL PORTS of it — keep all three in sync.

- The EXPLICIT allowlist matches ONLY: dated `YYYY-MM-DDTHH-MM-SS-789` and stage
  `stage-genlock-<hex>` / `genlock-stage-<hex>` (lowercase-hex sha). It is **NEVER a generic sweep**:
  the imag `previous/` rollback dir, an operator folder, and every unrelated top-level dir on `C:\`
  are PROTECTED.
- KEEP the newest `keep-runs` dirs of EACH kind separately (so a burst of stage dirs never evicts a
  recent dated backup) OR anything younger than `keep-days` (union); DELETE the rest.
- Defaults `keep-runs=3 / keep-days=7` (`keep-runs` mirrors the deploy program's `RETENTION_KEEP=3`).

## Runbook — DRY-RUN first, then the SUPERVISOR's reviewed --execute

Nothing is wired automatically — this is a supervisor-invoked sweep (exactly like issue 1122).
The deploy program's own inline `--yes` retention is unchanged and independent.

```bash
# 1) DRY-RUN (read-only — deploys the tool / runs the plan, prints keep/delete, deletes nothing)
scripts/obs-backup-retention.sh --host 10.77.9.202     # strih (win: scp+run the .ps1)
scripts/obs-backup-retention.sh --host 10.77.9.204     # stream (win)
scripts/obs-backup-retention.sh --imag                 # imag (bash --local-sweep over ssh, sudo)

# 2) Review the printed plan (KEEP / DELETE + SUMMARY). Confirm the DELETE set is only <stamp>-789
#    and stage-genlock-* dirs, and that `previous` / operator dirs are NOT in it.

# 3) SUPERVISOR ONLY — the first real deletion (irreversible bulk delete of prod-box dirs):
scripts/obs-backup-retention.sh --host 10.77.9.202 --execute
scripts/obs-backup-retention.sh --imag --execute
```

Tune with `--keep-runs N --keep-days D`. Win paths override with `--win-backup-root` /
`--win-stage-parent`; imag paths with `--backup-root` / `--stage-parent`. ssh passwords via
`STRIH_SSH_PW` (win, default `newlevel`) and `IMAG_IP`/`IMAG_USER`/`IMAG_PW` (imag).

## Tier-0 verification (#477/#557 block ALL local cargo, `--no-run` INCLUDED)

- **Pure decision:** copy `src/obs_backup_retention.rs` + `tests/obs_backup_retention.rs` into a
  scratch dir (define the neutral `RetentionPolicy`/`KeepReason`/`SECONDS_PER_DAY` at crate root,
  `mod obs_backup_retention { … }`, strip the test's `//!` header + adjust the two `use` lines) and
  compile standalone with `rustc --edition 2021 --test scratch.rs && ./scratch` — rustc is not
  cargo, so the Tier-0 hook does not touch it, and it runs the logic RED→GREEN with zero repo
  `target/`. Also `cargo fmt --all --check` (allowed, non-compiling — it parses the Rust).
- **The `.sh` (incl. the bash decision):** `bash -n` + `shellcheck -S warning`, then a REAL
  functional dry-run against a FIXTURE dir (`mkdir` dated/stage/foreign dirs, `touch -d` their
  mtimes, run `--local-sweep --backup-root <fix> --stage-parent <fix>`) and assert the DELETE plan
  matches the Rust decision — and that `--execute` deletes ONLY the DELETE set (foreign dirs
  survive). This is the imag decision's local test (no cargo needed).
- **The `.ps1`:** `pwsh` is not available on dev1, so verify by (a) ASCII-only
  (`grep -nP '[^\x00-\x7F]'` — a scp'd `.ps1` is read in a non-UTF-8 codepage; a non-ASCII char in a
  string BREAKS parsing, issue 1122), (b) `[0-9]`/`[0-9a-f]` never `\d` in the allowlist regex (.NET
  `\d` matches Unicode digits — MORE permissive than the Rust `is_ascii_digit()`, the wrong
  direction for a delete gate), and (c) a live DRY-RUN against a box (read-only, deletes nothing).

## Relation to issue 1122

Sibling of `recordings-retention.md`. Same dry-run-first / explicit-allowlist / supervisor-`--execute`
shape, DIFFERENT artifact: 1122 sweeps OBS **recordings** (`D:\_REC` `.mkv`, OBS-timestamp names);
this sweeps deploy **backup/stage DIRECTORIES** (`*-789` + `stage-genlock-*`). The neutral value
types (`RetentionPolicy`/`KeepReason`/`SECONDS_PER_DAY`) are shared via `recordings_retention`; the
allowlist + per-kind grouping are backup-specific.
