//! issue 1118 -> #1142 — the imag leg's schema-mismatched partial must DEGRADE (drop it), not kill
//! the merge; #1142 then makes the DROPPED leg RED the run instead of silently passing.
//!
//! ## Why this exists
//!
//! The #208 cross-box verdict merges per-box `RecordingPartial` JSONs
//! ([`crate::probe::recording_partial`]). Each partial carries a `schema_version`, and
//! `RecordingPartial::from_json` refuses an unknown version (so an incompatible file is never
//! silently mis-read). A schema-mismatched imag partial (issue 1118: a stale on-imag binary
//! emitting the OLD schema after the #1112 v3->v4 bump) must DEGRADE — drop the imag leg, warn
//! loudly, compute the verdict from strih+stream — never abort the whole merge with no verdict JSON,
//! which is exactly what the fatal `load(path)?` did (E2E 32178766136: the whole run reds with NO
//! verdict even though a strih+stream-only merge would have scored a real verdict). strih/stream are
//! the hard gate's own inputs — their partials come fresh from CI each run, so a schema mismatch
//! there is a genuine defect and stays FATAL.
//!
//! ## #1142 — DEGRADE is decoupled from the imag GATE flip (owner mandate 2026-08-19)
//!
//! Before #1142 the imag leg was WHOLESALE report-only, and this degrade was DERIVED from
//! [`crate::imag_leg_gate::gates_overall_pass`] (`false`) on the theory "when imag flips blocking, a
//! schema mismatch should become fatal too, so the two never drift". #1142 SPLIT the imag leg —
//! presence/verification BLOCKS, per-frame content stays report-only — and the owner mandate is
//! explicit that a schema-degraded imag leg "smie ostať degrade, ale musí RED-ovať, nie ticho
//! prejsť" (may stay a degrade, but must RED, not silently pass). So the degrade is now DECOUPLED
//! from the gate: [`box_degrades_on_schema_mismatch`] is unconditionally `true` for imag (the
//! merge still drops the leg + writes a real verdict instead of hard-dying), and the DROPPED leg's
//! `imag_leg_verified=false` is what REDs the run via the now-BLOCKING `imag_leg_verified` fold in
//! `recording-verdict.rs`. Degrading (a RED verdict) still beats aborting (no verdict at all).
//!
//! ## Why this lives at the crate root (default features), not in `probe`
//!
//! Same reasoning as [`crate::imag_leg_gate`] / [`crate::optical_floor`]: the whole `probe` module
//! is `#[cfg(feature = "probe")]`, so `recording-verdict.rs` (which does the merge) is CI-only and
//! has no local type-check (CLAUDE.md Tier-0 / airuleset #477). This is the PURE decision — no
//! probe deps — so it unit-tests Tier-0 (default features). `recording-verdict.rs::run_merge` only
//! CALLS it; it never re-derives the classification. The `expected` schema version is PASSED IN by
//! the (probe-gated) caller ([`crate::probe::recording_partial::PARTIAL_SCHEMA_VERSION`]) so this
//! module never imports the probe-gated constant.

use serde::Deserialize;

/// Just the `schema_version` field — the minimal peek shape (everything else in the real partial
/// is probe-gated). `#[serde(default)]` is intentionally NOT used: a partial with no numeric
/// `schema_version` fails to deserialize into this, which [`peek_schema_version`] maps to `None`
/// (a NON-schema failure — the caller then keeps it fatal).
#[derive(Deserialize)]
struct SchemaProbe {
    schema_version: u32,
}

/// Peek the `schema_version` from a recording-partial JSON string WITHOUT parsing the whole
/// (probe-gated) `RecordingPartial`. Pure (no I/O, no probe deps) → Tier-0 testable. Returns `None`
/// when the text is not valid JSON, or has no numeric `schema_version` field — the caller treats a
/// `None` as a non-schema (fatal-class) load failure, never a clean schema mismatch.
pub fn peek_schema_version(json: &str) -> Option<u32> {
    serde_json::from_str::<SchemaProbe>(json)
        .ok()
        .map(|p| p.schema_version)
}

/// #1142 — does a schema-mismatched partial for `box_name` DEGRADE (drop the leg, keep merging the
/// rest) rather than hard-abort the whole merge? `true` ONLY for the imag leg: its on-box binary can
/// legitimately be stale after a `PARTIAL_SCHEMA_VERSION` bump (the issue 1118 landmine), so a
/// schema mismatch there degrades to a dropped leg — and the dropped leg then REDs the run via the
/// now-BLOCKING `imag_leg_verified` fold (#1142), so degrading is NOT "swallow the error", it is
/// "produce a RED verdict instead of a fatal no-verdict crash". strih and stream are the hard gate's
/// own inputs — their partials come fresh from CI each run, so a schema mismatch there is a genuine
/// defect and stays FATAL.
///
/// DECOUPLED from [`crate::imag_leg_gate::gates_overall_pass`] as of #1142 (owner mandate): the imag
/// PRESENCE seam is now BLOCKING, but a schema-degraded imag leg must still DEGRADE (not go fatal) —
/// the RED comes from `imag_leg_verified=false`, not from aborting the merge. So this is a
/// deliberate, single hardcoded `== "imag"`, no longer tied to the gate flip.
pub fn box_degrades_on_schema_mismatch(box_name: &str) -> bool {
    box_name == "imag"
}

/// What to do with a partial whose `load` FAILED: DEGRADE (drop this leg, keep merging the rest,
/// record the reason) or stay FATAL (abort the whole merge)?
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartialLoadDisposition {
    /// Abort the merge — the box is a hard-gate input, or the failure is not a clean forward-compat
    /// schema mismatch on a report-only leg (unreadable / corrupt JSON, or a same-schema error).
    Fatal,
    /// Drop this leg's partial and keep merging from the remaining boxes; `reason` is a mineable
    /// one-line explanation surfaced in the verdict JSON + a loud stderr warning.
    Degrade { reason: String },
}

/// Classify a partial-load failure. `found_schema` is the version peeked from the file
/// ([`peek_schema_version`]) — `None` when the file was not even valid JSON with a numeric
/// `schema_version` (a non-schema failure). DEGRADES iff: the box degrades on a schema mismatch
/// ([`box_degrades_on_schema_mismatch`] — imag only) AND a schema version WAS peeked AND it differs
/// from `expected`. Every other failure — a hard-gate box (strih/stream), the imag box whose
/// failure is NOT a clean schema mismatch (`None`), or a same-schema error — stays
/// [`PartialLoadDisposition::Fatal`].
pub fn classify_load_failure(
    box_name: &str,
    found_schema: Option<u32>,
    expected_schema: u32,
) -> PartialLoadDisposition {
    match found_schema {
        Some(found) if found != expected_schema && box_degrades_on_schema_mismatch(box_name) => {
            PartialLoadDisposition::Degrade {
                reason: format!(
                    "{box_name} partial schema_version {found} != this build's {expected_schema} \
                     (issue 1118): dropped — verdict computed from the remaining (strih+stream) \
                     partials; imag_leg_verified=false, which #1142 makes BLOCKING so this run REDs \
                     (unless imag is operator-offline-acked). Re-run once the on-{box_name} binary \
                     is redeployed (recording-verdict-on-imag.sh now version-gates the upload)."
                ),
            }
        }
        _ => PartialLoadDisposition::Fatal,
    }
}
