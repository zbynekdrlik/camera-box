//! #356 — cross-recording cam1 loss reconciliation for the recording-verdict MERGE.
//!
//! On the 60→30 mixed topology with a high-latency (~450 ms) `strih→stream` hop, cam1's
//! contiguity is read from the CLEAN upstream STRIH recording (#133). The small cam1 burn QR is
//! softened through the deep buffer + the extra NDI hop + HEVC re-encode, so some cam1 ids are
//! UNREADABLE in the strih recording even though the frame was DELIVERED. Those ids are (wrongly)
//! classified `RealDrop` and inflate the merge headline (`recording-verdict --merge-partials`),
//! reading a near-zero-loss run as a catastrophic REAL-DROP fail (the #356 residual cam1
//! over-count that remained after the #360/#361 strih/cam2 fix).
//!
//! This module is the PURE reconciliation kernel: given the cam1 ids classified REAL DROP from the
//! strih recording and the set of cam1 ids actually DECODED in the DOWNSTREAM stream recording,
//! it returns the subset that was PROVEN delivered downstream (present in the stream recording) and
//! must therefore be re-classified BURN-UNREADABLE (a strih-recording readability gap), NOT a
//! chain loss. This mirrors the #226 spirit (a proven-present-but-unreadable id is BURN-UNREADABLE,
//! not a loss) but reconciles ACROSS recordings.
//!
//! STRICT-TEST SAFETY (the #1 invariant — a genuine frame loss must NEVER be masked):
//! - An id ABSENT from the downstream set is NEVER returned — it stays REAL DROP. Presence in the
//!   (decimated 30 fps, softened) stream recording is POSITIVE proof of delivery; absence is NOT
//!   proof of loss (the 60→30 stream hop decimates away ~half of cam1's 60 fps ids), so an absent
//!   id is treated conservatively and kept charged as REAL DROP.
//! - The result can only MOVE an id from REAL-DROP to BURN-UNREADABLE. The caller never touches the
//!   node's `missing_ids` (which drive `is_contiguous`/`is_zero`), so a downgraded id STILL makes
//!   the burn-id sequence non-contiguous — this reconciliation can NEVER create a false ZERO.

use std::collections::BTreeSet;

/// The cam1 REAL-DROP ids (read from the upstream strih recording) that are PROVEN delivered
/// downstream — i.e. present in the downstream stream recording's decoded cam1 ids — and so must be
/// re-classified BURN-UNREADABLE rather than REAL DROP.
///
/// - `real_drop_ids`: the cam1 ids currently classified REAL DROP from the strih recording.
/// - `downstream_cam1_ids`: every cam1 id DECODED in the downstream stream recording.
///
/// Returns exactly `real_drop_ids ∩ downstream_cam1_ids`. An id absent from `downstream_cam1_ids`
/// is never returned (SAFETY: a genuine loss — absent from both recordings — is never masked; a
/// decimated-away id we cannot prove delivered is likewise kept conservatively as REAL DROP).
pub fn cam1_real_drops_proven_delivered_downstream<I>(
    real_drop_ids: I,
    downstream_cam1_ids: &BTreeSet<u32>,
) -> BTreeSet<u32>
where
    I: IntoIterator<Item = u32>,
{
    // STUB (RED — #356): reconciliation not yet implemented. Returns nothing to downgrade, matching
    // the pre-#356 behaviour (every strih REAL DROP stays REAL DROP). Replaced by the real
    // intersection in the GREEN commit.
    let _ = (real_drop_ids.into_iter().count(), downstream_cam1_ids.len());
    BTreeSet::new()
}

#[cfg(test)]
mod tests {
    use super::cam1_real_drops_proven_delivered_downstream;
    use std::collections::BTreeSet;

    fn set(v: &[u32]) -> BTreeSet<u32> {
        v.iter().copied().collect()
    }

    /// #356 CORE: a cam1 REAL-DROP id that IS present in the downstream stream recording was
    /// delivered → it must be returned for re-classification to BURN-UNREADABLE.
    #[test]
    fn real_drop_present_downstream_is_flagged_for_burn_unreadable() {
        let real_drops = [5005u32, 5100];
        // 5005 AND 5100 both decoded in the downstream stream recording ⇒ both delivered.
        let downstream = set(&[5000, 5005, 5006, 5100, 5200]);
        let got = cam1_real_drops_proven_delivered_downstream(real_drops, &downstream);
        assert_eq!(
            got,
            set(&[5005, 5100]),
            "both real-drop ids present downstream must be flagged as delivered (BURN-UNREADABLE)"
        );
    }

    /// #356 SAFETY (the #1 invariant — must FAIL if the fix is too aggressive): a cam1 REAL-DROP id
    /// ABSENT from the downstream stream recording is a GENUINE loss (absent from both recordings)
    /// and MUST NOT be flagged — it stays REAL DROP. A false ZERO is never created.
    #[test]
    fn real_drop_absent_downstream_stays_real_drop() {
        let real_drops = [5005u32];
        // 5005 ABSENT downstream (genuinely lost, OR decimated away — unprovable ⇒ conservative).
        let downstream = set(&[5000, 5001, 5006, 5007]);
        let got = cam1_real_drops_proven_delivered_downstream(real_drops, &downstream);
        assert!(
            got.is_empty(),
            "an id absent from the downstream recording must NEVER be flagged for downgrade — a \
             genuine loss must stay REAL DROP (no false ZERO)"
        );
    }

    /// Mixed: exactly the PROVEN-delivered subset is returned; the absent ones stay REAL DROP.
    #[test]
    fn only_the_present_subset_is_flagged() {
        let real_drops = [5005u32, 5006, 5007, 5008];
        // Only 5006 + 5008 decoded downstream; 5005 + 5007 absent (and a foreign 9999 is irrelevant).
        let downstream = set(&[5006, 5008, 9999]);
        let got = cam1_real_drops_proven_delivered_downstream(real_drops, &downstream);
        assert_eq!(
            got,
            set(&[5006, 5008]),
            "exactly real_drops ∩ downstream is flagged; 5005/5007 stay REAL DROP"
        );
    }

    /// Empty inputs reconcile to nothing (no panic, no spurious downgrade).
    #[test]
    fn empty_inputs_flag_nothing() {
        let empty: BTreeSet<u32> = BTreeSet::new();
        assert!(cam1_real_drops_proven_delivered_downstream([], &set(&[1, 2, 3])).is_empty());
        assert!(cam1_real_drops_proven_delivered_downstream([1u32, 2], &empty).is_empty());
    }
}
