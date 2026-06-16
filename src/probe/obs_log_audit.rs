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

/// The DXGI device-loss codes we treat as a dead-GPU signature. Both forms appear
/// in the wedged stream log: the texture-create failure code (887A0005) and the
/// device-removed reason code (887A0007). Either alone is enough to black-hole the
/// output, so seeing any of them is a dead-GPU diagnosis.
const DXGI_DEVICE_LOST_CODES: &[&str] = &[
    "887A0005", // DXGI_ERROR_DEVICE_REMOVED
    "887A0006", // DXGI_ERROR_DRIVER_INTERNAL_ERROR
    "887A0007", // DXGI_ERROR_DEVICE_RESET
];

/// Result of auditing an OBS log for the GPU device-removed signature.
#[derive(Debug, Clone)]
pub struct ObsLogAudit {
    /// True when the dead-GPU signature was found at least once.
    pub device_removed: bool,
    /// Number of log lines carrying a device-lost signature (a `device_texture_create
    /// … Failed to create 2D texture (887A000x)` OR a `Device Removed Reason:
    /// 887A000x` line). In the wedged log this climbs into the thousands.
    pub device_removed_count: usize,
    /// The timestamp prefix (`HH:MM:SS.mmm`) of the FIRST device-lost line, for
    /// triage. `None` when no signature was found.
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

/// True when a log line carries a DXGI device-lost signature. Matches the OBS
/// texture-create-failure line, the device-removed-reason line, and any other line
/// quoting one of the device-lost codes — so a build that logs only one of the two
/// forms is still caught.
fn line_is_device_lost(line: &str) -> bool {
    DXGI_DEVICE_LOST_CODES.iter().any(|c| line.contains(c))
}

/// Extract the leading `HH:MM:SS.mmm` timestamp from an OBS log line, if present.
/// OBS prefixes every line with `HH:MM:SS.mmm: `; return the part before the first
/// `": "`. Returns `None` for a line without that prefix.
fn timestamp_prefix(line: &str) -> Option<String> {
    let ts = line.split_once(": ").map(|(ts, _)| ts.trim())?;
    // Sanity: an OBS timestamp looks like "03:33:39.533" — has two ':' and one '.'.
    if ts.matches(':').count() == 2 && ts.contains('.') {
        Some(ts.to_string())
    } else {
        None
    }
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
}
