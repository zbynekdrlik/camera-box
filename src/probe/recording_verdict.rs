//! #107 — hard-fail loss verdict from the recorded OBS program-output file.
//!
//! STUB (RED phase). The real engine lands in the GREEN commit.

use crate::probe::recording::RecordingFrame;
use serde::Serialize;

/// One camera frame's resolved Vernier tick, decoupled from ffmpeg/image so the
/// verdict engine is pure and unit-testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameTick {
    pub frame_index: u64,
    pub tick: Option<u32>,
}

impl FrameTick {
    pub fn from_recording_frames(frames: &[RecordingFrame]) -> Vec<FrameTick> {
        frames
            .iter()
            .map(|f| FrameTick {
                frame_index: f.frame_index,
                tick: f.tick,
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct VerdictConfig {
    pub capture_fps: f64,
    pub min_secs: f64,
    pub refresh_hz: f64,
}

impl Default for VerdictConfig {
    fn default() -> Self {
        VerdictConfig {
            capture_fps: 30.0,
            min_secs: 300.0,
            refresh_hz: 60.0,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RecordingVerdict {
    pub total_frames: usize,
    pub analyzed_secs: f64,
    pub duration_ok: bool,
    pub avg_step: f64,
    pub beat_balanced: bool,
    pub undecodable_frames: Vec<u64>,
    pub real_copy_frames: Vec<u64>,
    pub real_gap_frames: Vec<u64>,
}

impl RecordingVerdict {
    pub fn is_pass(&self) -> bool {
        false
    }
}

pub fn verdict(_frames: &[FrameTick], _cfg: &VerdictConfig) -> RecordingVerdict {
    RecordingVerdict {
        total_frames: 0,
        analyzed_secs: 0.0,
        duration_ok: false,
        avg_step: 0.0,
        beat_balanced: false,
        undecodable_frames: Vec::new(),
        real_copy_frames: Vec::new(),
        real_gap_frames: Vec::new(),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct StrihStreamVerdict {
    pub compared_frames: usize,
    pub divergent_frames: Vec<u64>,
}

impl StrihStreamVerdict {
    pub fn is_pass(&self) -> bool {
        false
    }
}

pub fn strih_stream_verdict(
    _strih: &[FrameTick],
    _stream: &[FrameTick],
    _cfg: &VerdictConfig,
) -> StrihStreamVerdict {
    StrihStreamVerdict {
        compared_frames: 0,
        divergent_frames: Vec::new(),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CamStrihAssessment {
    pub claims_zero_loss: bool,
    pub unknown_ticks: Vec<u32>,
    pub limitation: String,
}

pub fn cam_strih_assessment(
    _strih: &[FrameTick],
    _painter_ticks: &[u32],
    _cfg: &VerdictConfig,
) -> CamStrihAssessment {
    CamStrihAssessment {
        claims_zero_loss: false,
        unknown_ticks: Vec::new(),
        limitation: String::new(),
    }
}
