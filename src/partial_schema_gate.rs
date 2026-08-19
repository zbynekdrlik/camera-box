//! issue 1118 — a REPORT-ONLY leg's schema-mismatched partial must DEGRADE, not kill the merge.
//!
//! ## Why this exists
//!
//! The #208 cross-box verdict merges per-box `RecordingPartial` JSONs
//! ([`crate::probe::recording_partial`]). Each partial carries a `schema_version`, and
//! `RecordingPartial::from_json` refuses an unknown version (so an incompatible file is never
//! silently mis-read). The imag leg is REPORT-ONLY today
//! ([`crate::imag_leg_gate::gates_overall_pass`] returns `false`) — its verdict flows + is
//! surfaced but never reds a run. So its INPUT must be report-only too: a schema-mismatched imag
//! partial (issue 1118: a stale on-imag binary emitting the OLD schema after the #1112 v3->v4
//! bump) must DEGRADE — drop the leg, warn loudly, compute the verdict from strih+stream — never
//! abort the whole merge with no verdict JSON, which is exactly what the fatal `load(path)?` did
//! (E2E 32178766136: the whole run reds even though a strih+stream-only merge scores
//! `overall_pass=true`). strih/stream are the hard gate's own inputs — their partials come fresh
//! from CI each run, so a schema mismatch there is a genuine defect and stays FATAL.
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

/// Is `box_name` a REPORT-ONLY leg whose partial-input errors must never zero the whole gate?
/// DERIVED from the single source of truth ([`crate::imag_leg_gate::gates_overall_pass`]), never a
/// second hardcoded copy: the imag leg is the only report-only leg, and ONLY while its seam is
/// report-only. strih and stream are the hard gate's own inputs — their partial errors always stay
/// fatal. When a follow-up flips the imag leg blocking (`gates_overall_pass()` → `true`), this
/// automatically returns `false` for imag too, so a schema-mismatched imag partial then stays FATAL
/// like strih/stream — the degrade and the gate can never drift out of lockstep (issue 1118 review).
pub fn box_is_report_only(box_name: &str) -> bool {
    box_name == "imag" && !crate::imag_leg_gate::gates_overall_pass()
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
/// `schema_version` (a non-schema failure). DEGRADES iff: the box is report-only ([`box_is_report_only`])
/// AND a schema version WAS peeked AND it differs from `expected`. Every other failure — a hard-gate
/// box, a report-only box whose failure is NOT a clean schema mismatch (`None`), or a same-schema
/// error — stays [`PartialLoadDisposition::Fatal`].
pub fn classify_load_failure(
    box_name: &str,
    found_schema: Option<u32>,
    expected_schema: u32,
) -> PartialLoadDisposition {
    match found_schema {
        Some(found) if found != expected_schema && box_is_report_only(box_name) => {
            PartialLoadDisposition::Degrade {
                reason: format!(
                    "{box_name} partial schema_version {found} != this build's {expected_schema} \
                     (report-only leg, issue 1118): dropped — verdict computed from the remaining \
                     (strih+stream) partials; imag_leg_verified=false. Re-run once the on-{box_name} \
                     binary is redeployed (recording-verdict-on-imag.sh now version-gates the upload)."
                ),
            }
        }
        _ => PartialLoadDisposition::Fatal,
    }
}
