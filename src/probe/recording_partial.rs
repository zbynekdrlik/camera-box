//! #208 — the per-box decode-in-place PARTIAL: the SMALL JSON contract that carries what the
//! cross-box verdict merge needs from ONE recording, so a recording is NEVER copied box-to-box
//! (nor to dev1) — only this small JSON moves.
//!
//! WHY (#208, refines #193): the verdict needs the STRIH recording (cam1 contiguity #133 +
//! cam→strih) AND the STREAM recording (the full chain). The old on-stream flow ran a SINGLE
//! fused verdict on the stream box, which forced the ~700 MB strih `.mkv` to be copied
//! strih→stream first. Instead, each box decodes its OWN recording IN PLACE
//! (`recording-verdict --extract-partial <box>`) into this partial; dev1 combines the partials
//! (`recording-verdict --merge-partials strih=… stream=…`) into the identical full verdict.
//!
//! ## JSON schema (`schema_version = 2`)
//!
//! ```json
//! {
//!   "schema_version": 2,
//!   "box": "strih",                 // which box decoded this — "strih" | "stream"
//!   "recording": "strih-1234.mkv",  // basename of the local recording (provenance only)
//!   "expected_burns": [911001, 911002],  // node-burn run_ids this extract decoded for (#207)
//!   "frames": [                     // one entry per analyzed recorded frame, in file order
//!     { "frame_index": 0,
//!       "payloads": [ { "run_id": 7, "frame_id": 100, "gen_ts_ns": 1700000000000000000 },
//!                     { "run_id": 911002, "frame_id": 1670, "gen_ts_ns": 1700000000000000123 } ],
//!       "tick": 100 }
//!     // …
//!   ],
//!   "colour": null                  // #377 per-recording NodeColourSummary (Some only after a
//!                                   // --colour-gate extract; absent/null on delivery-only runs)
//! }
//! ```
//!
//! Each `frames[]` entry is a [`RecordingFrame`]: a decoded-id record — `frame_index`, every
//! CRC-valid QR `payload` (`run_id` / `frame_id` / `gen_ts_ns`), and the optical cam2 `tick`.
//! It carries NO pixels and NO video; it is "ids + timestamps", orders of magnitude smaller
//! than the recording it was decoded from. The merge re-runs the EXACT verdict logic on these
//! frames, so the merged verdict is equivalent to the fused single-process output.

use crate::colour_verify::NodeColourSummary;
use crate::probe::recording::RecordingFrame;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// The partial JSON schema version. Bump on any breaking change to the wire shape; [`load`]
/// refuses an unknown version rather than silently mis-reading an incompatible file.
///
/// v2 (#377) adds the optional `colour` field — the per-recording [`NodeColourSummary`] the
/// box computed on-host during `--colour-gate` extract, carried through to the dev1 merge. extract
/// and merge always run from the SAME binary build, so a mixed v1/v2 read never occurs in practice.
pub const PARTIAL_SCHEMA_VERSION: u32 = 2;

/// One box's decode-in-place result — the small JSON the cross-box merge consumes (#208).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingPartial {
    /// Wire-format version (see [`PARTIAL_SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// Which box decoded this recording: `"strih"` or `"stream"`. The merge enforces that a
    /// partial only fills the slot matching its box (a strih partial can never be merged as the
    /// stream input) — the box-to-box guard at the data level.
    #[serde(rename = "box")]
    pub box_name: String,
    /// Basename of the local recording this was decoded from (provenance only — the merge never
    /// reads the recording).
    pub recording: String,
    /// The node-burn run_ids this extract decoded for (the #207 fast-then-robust gate set).
    pub expected_burns: Vec<u32>,
    /// One entry per analyzed recorded frame, in file (capture) order. Ids + timestamps only —
    /// no pixels (see [`RecordingFrame`]).
    pub frames: Vec<RecordingFrame>,
    /// #377 — the per-camera COLOUR verdict of THIS box's recording, computed ON the box during
    /// `--colour-gate` extract (the colour gate is fused/on-host: the recording is only here). The
    /// merge applies it to the node(s) this recording backs: the stream recording's summary covers
    /// strih and stream, the strih recording's summary covers cam1 (mirrors the fused node-to-recording
    /// mapping). `None` when `--colour-gate` was off (delivery-only runs), so old behaviour is unchanged.
    #[serde(default)]
    pub colour: Option<NodeColourSummary>,
}

impl RecordingPartial {
    /// Build a partial from a box's decoded frames. `recording` is reduced to its basename
    /// (provenance only).
    pub fn from_frames(
        box_name: &str,
        recording: &Path,
        expected_burns: &[u32],
        frames: Vec<RecordingFrame>,
    ) -> Self {
        let recording = recording
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| recording.to_string_lossy().into_owned());
        RecordingPartial {
            schema_version: PARTIAL_SCHEMA_VERSION,
            box_name: box_name.to_string(),
            recording,
            expected_burns: expected_burns.to_vec(),
            frames,
            colour: None,
        }
    }

    /// Attach the per-recording colour summary (#377) — set by `--extract-partial` when
    /// `--colour-gate` is on, after sampling THIS box's recording. Builder so `from_frames` stays a
    /// pure ids+timestamps constructor and the colour I/O lives in the probe-gated caller.
    pub fn with_colour(mut self, colour: Option<NodeColourSummary>) -> Self {
        self.colour = colour;
        self
    }

    /// Serialize to compact JSON (NOT pretty — the file moves over the LAN; keep it lean).
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string(self).context("serialize recording partial")
    }

    /// Parse from JSON, refusing an unknown `schema_version`.
    pub fn from_json(s: &str) -> Result<Self> {
        let p: RecordingPartial =
            serde_json::from_str(s).context("parse recording partial JSON")?;
        anyhow::ensure!(
            p.schema_version == PARTIAL_SCHEMA_VERSION,
            "recording partial schema_version {} is not supported (this build expects {})",
            p.schema_version,
            PARTIAL_SCHEMA_VERSION
        );
        Ok(p)
    }

    /// Write the partial to `path` as compact JSON.
    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, self.to_json()?)
            .with_context(|| format!("write recording partial {}", path.display()))
    }

    /// Read + parse a partial from `path` (schema-checked).
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read recording partial {}", path.display()))?;
        Self::from_json(&text)
            .with_context(|| format!("parse recording partial {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::payload::Payload;

    fn frame(frame_index: u64, payloads: &[(u32, u32, i64)]) -> RecordingFrame {
        let payloads: Vec<Payload> = payloads
            .iter()
            .map(|&(run_id, frame_id, gen_ts_ns)| Payload {
                run_id,
                frame_id,
                gen_ts_ns,
            })
            .collect();
        let tick = payloads.iter().map(|p| p.frame_id).max();
        RecordingFrame {
            frame_index,
            payloads,
            tick,
        }
    }

    #[test]
    fn roundtrip_is_lossless() {
        let frames = vec![
            frame(
                0,
                &[
                    (7, 100, 1_700_000_000_000_000_000),
                    (911002, 1670, 1_700_000_000_000_000_123),
                ],
            ),
            frame(1, &[(7, 101, 1_700_000_000_033_000_000)]),
            frame(2, &[]),
        ];
        let p = RecordingPartial::from_frames(
            "strih",
            Path::new("/tmp/strih-9.mkv"),
            &[911001, 911002],
            frames.clone(),
        );
        let restored = RecordingPartial::from_json(&p.to_json().unwrap()).unwrap();
        assert_eq!(
            restored, p,
            "partial JSON roundtrip must be byte-for-byte lossless"
        );
        assert_eq!(
            restored.frames, frames,
            "the decoded frames must survive the roundtrip exactly"
        );
        assert_eq!(restored.box_name, "strih");
        assert_eq!(
            restored.recording, "strih-9.mkv",
            "recording is reduced to its basename"
        );
        assert_eq!(restored.expected_burns, vec![911001, 911002]);
    }

    #[test]
    fn colour_summary_survives_the_partial_roundtrip_377() {
        // #377 — the per-recording colour summary the box computed on-host must round-trip through
        // the partial JSON so the dev1 merge can apply it (the colour gate is fused/on-host).
        let colour = NodeColourSummary {
            patch_wrong_counts: vec![0, 2, 0, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            patch_checked_counts: vec![6; 13],
            frames_sampled: 6,
        };
        let p = RecordingPartial::from_frames(
            "stream",
            Path::new("stream-9.mp4"),
            &[911001, 911002, 911004],
            vec![],
        )
        .with_colour(Some(colour.clone()));
        let restored = RecordingPartial::from_json(&p.to_json().unwrap()).unwrap();
        assert_eq!(
            restored.colour,
            Some(colour),
            "the carried colour summary must survive the JSON roundtrip exactly"
        );
    }

    #[test]
    fn colour_defaults_to_none_when_absent_in_json() {
        // A delivery-only partial (no --colour-gate at extract) has no colour; an older/clean JSON
        // without the field must deserialize to None via #[serde(default)], never error.
        let p = RecordingPartial::from_frames("strih", Path::new("strih-1.mkv"), &[], vec![]);
        assert_eq!(p.colour, None, "from_frames defaults colour to None");
        let j = r#"{"schema_version":2,"box":"strih","recording":"x.mkv","expected_burns":[],"frames":[]}"#;
        let restored = RecordingPartial::from_json(j).unwrap();
        assert_eq!(restored.colour, None, "absent colour field ⇒ None");
    }

    #[test]
    fn box_field_is_named_box_in_json() {
        let p = RecordingPartial::from_frames("stream", Path::new("stream-1.mp4"), &[], vec![]);
        let j = p.to_json().unwrap();
        assert!(
            j.contains("\"box\":\"stream\""),
            "JSON key is `box`, not `box_name`: {j}"
        );
    }

    #[test]
    fn unknown_schema_version_is_refused() {
        // A future/incompatible version must error, never silently mis-read.
        let j = r#"{"schema_version":999,"box":"strih","recording":"x.mkv","expected_burns":[],"frames":[]}"#;
        let err = RecordingPartial::from_json(j).unwrap_err();
        assert!(
            err.to_string().contains("schema_version"),
            "unknown schema_version must be refused: {err}"
        );
    }
}
