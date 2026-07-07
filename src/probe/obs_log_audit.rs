//! #81 OBS log audit — surface a GPU device-removed (TDR) crash from an OBS log.
//!
//! When a stream OBS output goes silently dead (#81), the cause is recorded in its
//! own log as a DXGI device-removed crash:
//!   `device_texture_create (D3D11): Failed to create 2D texture (887A0005)`
//!   `  Device Removed Reason: 887A0007`
//! 887A0005 = DXGI_ERROR_DEVICE_REMOVED, 887A0007 = DXGI_ERROR_DEVICE_RESET — a GPU
//! TDR. Once it fires OBS cannot create textures, so the compositor emits nothing
//! and the NDI Main Output black-holes. On stream.lan 2026-06-16 this signature
//! appeared 6071× and OBS never recovered.
//!
//! `audit_obs_log` parses raw OBS log text and reports a dead-GPU diagnosis (with an
//! occurrence count, the first timestamp, and the DXGI codes seen) so the harness
//! can print "downstream OBS dead? (GPU device-removed)" WITH EVIDENCE instead of a
//! silent zero-frames result. Pure / unit-tested against real log fixtures — no rig.
//!
//! #89: the code list + line matcher live in the crate-root `crate::dxgi_device_lost` module
//! (no probe deps) so the default-feature watchdog/self-heal pipeline can reuse the EXACT SAME
//! signature match this harness-side audit uses — never a second drifting copy.

use crate::dxgi_device_lost::{line_is_device_lost, DXGI_DEVICE_LOST_CODES};

/// Result of auditing an OBS log for the GPU device-removed signature.
#[derive(Debug, Clone)]
pub struct ObsLogAudit {
    /// True when the dead-GPU signature was found at least once.
    pub device_removed: bool,
    /// Number of log lines carrying a device-lost signature (a `device_texture_create
    /// … Failed to create 2D texture (887A000x)` OR a `Device Removed Reason:
    /// 887A000x` line). In the wedged log this climbs into the thousands.
    pub device_removed_count: usize,
    /// The `HH:MM:SS.mmm` timestamp prefix of the first device-lost line that
    /// carries a parseable OBS timestamp, for triage. (A device-lost line with no
    /// timestamp prefix still counts toward `device_removed_count` but is skipped
    /// here, so this is the first *timestamped* occurrence.) `None` when no
    /// signature carried a parseable timestamp.
    pub first_timestamp: Option<String>,
    /// The distinct DXGI codes observed (e.g. `["887A0005", "887A0007"]`), in first-
    /// seen order. Empty when no signature was found.
    pub codes: Vec<String>,
}

impl ObsLogAudit {
    /// A one-line, operator-facing diagnosis: either the dead-GPU verdict with
    /// evidence, or a clean no-fault line.
    pub fn diagnosis(&self) -> String {
        if self.device_removed {
            format!(
                "DEAD GPU: OBS device-removed (DXGI device-lost {:?}) detected {} time(s), \
                 first at {} — the GPU was reset/removed (TDR), the compositor cannot \
                 create textures so the NDI Main Output emits nothing. A full PC reboot \
                 of the stream box is typically required (an OBS-only restart often does \
                 NOT clear a wedged GPU). (#81)",
                self.codes,
                self.device_removed_count,
                self.first_timestamp.as_deref().unwrap_or("?"),
            )
        } else {
            "GPU healthy: no OBS device-removed / 887A000x signature found in the log (#81)"
                .to_string()
        }
    }
}

/// Extract the leading `HH:MM:SS.mmm` timestamp from an OBS log line, if present.
/// OBS prefixes every line with `HH:MM:SS.mmm: `; return the part before the first
/// `": "` only when it is EXACTLY that strict shape (`\d\d:\d\d:\d\d.\d\d\d`), so a
/// message that merely happens to contain a colon-space can never be mistaken for a
/// timestamp. Returns `None` for a line without that prefix.
fn timestamp_prefix(line: &str) -> Option<String> {
    let ts = line.split_once(": ").map(|(ts, _)| ts)?;
    if is_obs_timestamp(ts) {
        Some(ts.to_string())
    } else {
        None
    }
}

/// True iff `ts` is exactly an OBS timestamp: `HH:MM:SS.mmm` (two digits, `:`, two
/// digits, `:`, two digits, `.`, three digits) — no trimming, no extra chars.
fn is_obs_timestamp(ts: &str) -> bool {
    let b = ts.as_bytes();
    b.len() == 12
        && b[0].is_ascii_digit()
        && b[1].is_ascii_digit()
        && b[2] == b':'
        && b[3].is_ascii_digit()
        && b[4].is_ascii_digit()
        && b[5] == b':'
        && b[6].is_ascii_digit()
        && b[7].is_ascii_digit()
        && b[8] == b'.'
        && b[9].is_ascii_digit()
        && b[10].is_ascii_digit()
        && b[11].is_ascii_digit()
}

/// Audit raw OBS log text for the GPU device-removed (TDR) signature (#81).
///
/// Counts every line carrying a DXGI device-lost code, records the first such line's
/// timestamp, and collects the distinct codes seen. Pure: feed it the log text (from
/// a live read or a fixture); no rig required.
pub fn audit_obs_log(log: &str) -> ObsLogAudit {
    let mut count = 0usize;
    let mut first_timestamp: Option<String> = None;
    let mut codes: Vec<String> = Vec::new();
    for line in log.lines() {
        if !line_is_device_lost(line) {
            continue;
        }
        count += 1;
        if first_timestamp.is_none() {
            first_timestamp = timestamp_prefix(line);
        }
        for c in DXGI_DEVICE_LOST_CODES {
            if line.contains(c) && !codes.iter().any(|seen| seen == c) {
                codes.push((*c).to_string());
            }
        }
    }
    ObsLogAudit {
        device_removed: count > 0,
        device_removed_count: count,
        first_timestamp,
        codes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_device_removed_with_evidence() {
        let log = "\
03:33:39.533: device_texture_create (D3D11): Failed to create 2D texture (887A0005)
03:33:39.533:   Device Removed Reason: 887A0007
03:33:44.265: device_texture_create (D3D11): Failed to create 2D texture (887A0005)
";
        let a = audit_obs_log(log);
        assert!(a.device_removed);
        assert_eq!(a.device_removed_count, 3);
        assert_eq!(a.first_timestamp.as_deref(), Some("03:33:39.533"));
        assert!(a.codes.contains(&"887A0005".to_string()));
        assert!(a.codes.contains(&"887A0007".to_string()));
        assert!(a.diagnosis().to_lowercase().contains("dead gpu"));
    }

    #[test]
    fn clean_log_is_healthy() {
        let log = "00:02:02.000: genlock: render tick ENABLED\n00:02:03.000: all good\n";
        let a = audit_obs_log(log);
        assert!(!a.device_removed);
        assert_eq!(a.device_removed_count, 0);
        assert!(a.first_timestamp.is_none());
        assert!(a.codes.is_empty());
        assert!(a.diagnosis().to_lowercase().contains("healthy"));
    }

    #[test]
    fn device_reset_reason_line_only() {
        // Some builds log only the reason line, not the texture-create failure.
        let log = "10:00:00.000:   Device Removed Reason: 887A0007\n";
        let a = audit_obs_log(log);
        assert!(a.device_removed);
        assert_eq!(a.device_removed_count, 1);
        assert_eq!(a.codes, vec!["887A0007".to_string()]);
    }

    #[test]
    fn timestamp_prefix_ignores_non_timestamped_line() {
        // A code on a line with no OBS timestamp prefix still counts but yields no ts.
        let log = "some preamble 887A0005 without a timestamp\n";
        let a = audit_obs_log(log);
        assert!(a.device_removed);
        assert!(a.first_timestamp.is_none());
    }

    #[test]
    fn strict_timestamp_rejects_colon_space_message() {
        // A device-lost line whose message contains a ': ' but no real OBS
        // timestamp prefix must NOT yield a bogus first_timestamp (the strict
        // HH:MM:SS.mmm shape rejects it), while still being counted.
        let log = "module: failed 887A0005 with reason: device lost\n";
        let a = audit_obs_log(log);
        assert!(a.device_removed);
        assert_eq!(a.device_removed_count, 1);
        assert!(
            a.first_timestamp.is_none(),
            "a colon-space message must not be parsed as a timestamp, got {:?}",
            a.first_timestamp
        );
    }

    #[test]
    fn first_timestamp_skips_leading_untimestamped_line() {
        // The first device-lost line has no timestamp; the second does — the
        // reported first_timestamp must be the second's (the first TIMESTAMPED
        // occurrence), matching the field doc.
        let log = "preamble 887A0005 no ts\n12:34:56.789: device-lost 887A0007\n";
        let a = audit_obs_log(log);
        assert_eq!(a.device_removed_count, 2);
        assert_eq!(a.first_timestamp.as_deref(), Some("12:34:56.789"));
    }
}
