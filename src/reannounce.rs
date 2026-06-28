//! #297 — NDI sender re-announce trigger.
//!
//! OBS/DistroAV NDI discovery is mDNS-based and unreliable on this LAN: a camera box that
//! booted before its network was up, or whose link flapped, announces its NDI sender on a
//! dead/old network and then never appears in the OBS source dropdown — even though the
//! source is reachable by explicit name (observed live 2026-06-28: the finder returned only
//! {CAM2} while CAM1/CAM3/CAM4 were up and emitting). The cure on the SENDER side is to
//! re-register the NDI sender (which re-runs the mDNS announce on the CURRENT network)
//! whenever the host's usable network changes.
//!
//! This module holds the PURE trigger decision + the network signature it keys on, kept
//! cross-platform and unit-tested. The actual interface read (`getifaddrs`) and the sender
//! re-create live in `crate::ndi` (Linux-only — the appliance target).

use std::time::Duration;

/// How often the capture loop polls the network signature for a re-announce check. A change
/// is acted on within this interval, so a freshly-booted box is rediscovered "within
/// seconds" (issue #297) without re-reading interfaces on every captured frame. Bounded to a
/// few seconds: small enough for fast rediscovery, large enough that the `getifaddrs` read is
/// negligible against a 60 fps capture loop.
pub const REANNOUNCE_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// A signature of the host's usable (up, non-loopback, IPv4) network addresses.
///
/// Equality is order- and duplicate-independent: it is the SET of addresses that matters, not
/// the order the OS enumerates them. A change in this set (an address appeared / disappeared /
/// changed) is exactly the "the network became usable / flapped" event that warrants a
/// re-announce; a stable set must compare equal so a steady-state stream is never needlessly
/// re-created.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetworkSignature {
    /// Sorted + deduped canonical address list.
    addrs: Vec<String>,
}

impl NetworkSignature {
    /// Build a canonical signature from an iterator of address strings (e.g. "10.77.9.61").
    /// Empty / whitespace-only entries are dropped; the result is sorted + deduped so equality
    /// is order- and duplicate-independent.
    pub fn from_addrs<I, S>(addrs: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut addrs: Vec<String> = addrs
            .into_iter()
            .map(Into::into)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        addrs.sort();
        addrs.dedup();
        Self { addrs }
    }

    /// True when there is no usable address (network down / not up yet).
    pub fn is_empty(&self) -> bool {
        self.addrs.is_empty()
    }

    /// The canonical (sorted, deduped) address list.
    pub fn addrs(&self) -> &[String] {
        &self.addrs
    }
}

/// Decide whether the NDI sender should be re-announced (destroyed + re-created so the NDI
/// runtime re-registers it via mDNS on the CURRENT network).
///
/// STUB (#297 RED): currently returns `false` unconditionally — i.e. the sender NEVER
/// re-announces, which is exactly the bug (a box whose network came up after start stays
/// invisible to the OBS NDI finder forever). The GREEN commit implements the real decision.
pub fn should_reannounce(_announced: &NetworkSignature, _current: &NetworkSignature) -> bool {
    false
}
