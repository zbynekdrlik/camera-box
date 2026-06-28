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

/// Interface-name prefixes that are NOT the camera's discoverable LAN NIC: loopback,
/// container/VM bridges, virtual ethernet, VPN/overlay tunnels. Their addresses must NOT enter
/// the signature — a `docker0` / `virbr0` / `tailscale0` / `veth*` address appearing or
/// flapping would otherwise flip the signature and needlessly re-create the sender, dropping
/// the LIVE program feed for a change that has nothing to do with LAN discoverability. Matched
/// case-insensitively as a prefix. (Loopback is already excluded by `IFF_LOOPBACK` at the IO
/// layer; `"lo"` here is belt-and-braces.)
const NON_LAN_IFACE_PREFIXES: &[&str] = &[
    "lo",
    "docker",
    "veth",
    "virbr",
    "br-",
    "tailscale",
    "tun",
    "tap",
    "wg",
    "zt",
    "cni",
    "flannel",
    "kube",
    "cali",
    "vboxnet",
    "vmnet",
];

/// True when `name` is a real LAN NIC whose IPv4 address belongs in the discovery signature
/// (i.e. NOT loopback / a bridge / virtual / VPN interface). The Linux IO layer
/// (`crate::ndi::current_network_signature`) filters `getifaddrs` results through this so the
/// re-announce trigger keys on the LAN address only — matching the intent ("the LAN address
/// settled / flapped") and never re-announcing because an unrelated virtual interface moved.
///
/// This is a DENYLIST of known virtual prefixes (the cam boxes have a single physical NIC, so a
/// denylist matches reality without risking a real NIC being excluded). A plain `br0` (a bridge
/// that itself carries the LAN IP) is deliberately NOT filtered — it is the LAN. If deployments
/// ever diversify, tighten this to an allowlist (`en*`/`eth*`/`wl*`).
pub fn is_discoverable_interface(name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase();
    if n.is_empty() {
        return false;
    }
    !NON_LAN_IFACE_PREFIXES.iter().any(|p| n.starts_with(p))
}

/// Decide whether the NDI sender should be re-announced (destroyed + re-created so the NDI
/// runtime re-registers it via mDNS on the CURRENT network).
///
/// Re-announce iff the current usable-network signature is NON-EMPTY **and** either it DIFFERS
/// from the signature the sender was last announced with, **or** the network was observed DOWN
/// since the last announce (a link bounce that returned the same address still drops the mDNS
/// registration). The guards are load-bearing:
///
/// - **Empty current → never.** While the network is down there is nothing to announce on;
///   re-creating the sender onto no network is pointless churn. Wait for an address to appear
///   (an address change, or — if it returns the same — `saw_down_since_announce`, triggers it).
/// - **Equal and no down event → never.** A stable network must NOT re-create the sender,
///   because a re-create forces every connected receiver (OBS) to drop + reconnect the feed.
///   Only a real change, or a recovery from an outage that already disrupted the feed, is worth
///   that.
pub fn should_reannounce(
    announced: &NetworkSignature,
    current: &NetworkSignature,
    saw_down_since_announce: bool,
) -> bool {
    if current.is_empty() {
        return false;
    }
    announced != current || saw_down_since_announce
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(addrs: &[&str]) -> NetworkSignature {
        NetworkSignature::from_addrs(addrs.iter().copied())
    }

    #[test]
    fn seeded_with_live_network_does_not_fire() {
        // #297(3): the sender is created when the network is ALREADY up — the trigger is seeded
        // with that live signature, so the first stable poll must NOT spuriously re-create the
        // sender (which would needlessly drop the program feed on every clean boot).
        let st = ReannounceState::new(sig(&["10.77.9.62"]));
        assert!(
            !st.should_reannounce(&sig(&["10.77.9.62"])),
            "a sender seeded with the live network must not re-announce on a stable poll"
        );
    }

    #[test]
    fn converges_after_successful_announce() {
        // THE #297 infinite-loop regression: boot race — the sender was created before the
        // network came up (empty seed). The first poll once an address appears MUST fire, and
        // after the re-announce SUCCEEDS the trigger must advance so the next STABLE poll does
        // NOT fire again. The shipped bug left the trigger at the empty baseline forever, so it
        // re-fired every poll (2s WARN spam) and never converged.
        let mut st = ReannounceState::new(NetworkSignature::default()); // booted before net up
        let up = sig(&["10.77.9.130", "10.77.9.62"]);
        assert!(
            st.should_reannounce(&up),
            "an address appearing after start MUST fire the re-announce (boot race)"
        );
        st.record_reannounce_attempt(up.clone(), true); // re-create succeeded (destroy-first)
        assert!(
            !st.should_reannounce(&up),
            "after a successful re-announce a stable poll MUST NOT re-fire (no infinite loop)"
        );
    }

    #[test]
    fn retries_after_failed_create_then_converges() {
        // The now-rare null create AFTER the destroy: the old handle is gone, the new one failed,
        // so the sender is absent. The trigger state must be LEFT UNCHANGED so the very next poll
        // RETRIES the create (recover ASAP) — it must not silently advance and strand the box
        // with no sender. Once a retry succeeds, it converges.
        let mut st = ReannounceState::new(NetworkSignature::default());
        let up = sig(&["10.77.9.62"]);
        assert!(st.should_reannounce(&up), "first poll fires");
        st.record_reannounce_attempt(up.clone(), false); // create returned null after destroy
        assert!(
            st.should_reannounce(&up),
            "a FAILED re-create must leave the trigger firing so the next poll retries"
        );
        st.record_reannounce_attempt(up.clone(), true); // retry succeeded
        assert!(
            !st.should_reannounce(&up),
            "once the retry succeeds the trigger converges"
        );
    }

    #[test]
    fn recovers_from_outage_then_converges() {
        // A link bounce that returned the SAME address: the mDNS registration was dropped during
        // the outage, so the trigger must fire once (saw_down) even though the address set is
        // unchanged, then converge after the re-announce succeeds.
        let mut st = ReannounceState::new(sig(&["10.77.9.62"]));
        st.mark_down(); // network observed down this poll
        let back = sig(&["10.77.9.62"]); // same address returned
        assert!(
            st.should_reannounce(&back),
            "recovery from an outage MUST re-announce even on the same address"
        );
        st.record_reannounce_attempt(back.clone(), true);
        assert!(
            !st.should_reannounce(&back),
            "after recovery the down flag is cleared and a stable poll does not re-fire"
        );
    }
}
