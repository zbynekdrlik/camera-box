//! #297 — NDI sender re-announce trigger.
//!
//! These tests pin the PURE re-announce decision (`should_reannounce`), the network signature
//! it keys on, and the LAN-interface filter. The bug being fixed: the camera-box NDI sender is
//! created ONCE at startup and never re-registers, so a box whose network came up after start
//! (or flapped) announces on a dead/old network and never appears in the OBS NDI source
//! dropdown. The trigger must re-announce precisely when the usable LAN network CHANGES (or
//! recovers from an outage) — and must NOT re-announce on a stable network (a re-create drops
//! every connected receiver) nor while the network is still down (nothing to announce on yet).

use camera_box::reannounce::{
    is_discoverable_interface, should_reannounce, NetworkSignature, REANNOUNCE_POLL_INTERVAL,
};
use std::time::Duration;

#[test]
fn network_change_triggers_reannounce() {
    // The box booted with one address, then the real LAN address settled (or the link
    // flapped to a new IP). This is the #297 case: the sender announced on the OLD network and
    // must re-register on the NEW one so OBS rediscovers it.
    let announced = NetworkSignature::from_addrs(["169.254.10.20"]); // link-local at boot
    let current = NetworkSignature::from_addrs(["10.77.9.61"]); // real LAN address now
    assert!(
        should_reannounce(&announced, &current, false),
        "a changed usable-network signature MUST trigger a re-announce (#297)"
    );
}

#[test]
fn network_came_up_after_start_triggers_reannounce() {
    // The most common boot race: the sender was created before any usable address existed
    // (network still coming up), so the announced signature is empty. Once an address appears,
    // the sender must re-announce on it.
    let announced = NetworkSignature::default(); // no usable address at sender create time
    let current = NetworkSignature::from_addrs(["10.77.9.63"]);
    assert!(
        should_reannounce(&announced, &current, false),
        "an address appearing after start MUST trigger a re-announce (#297 boot race)"
    );
}

#[test]
fn stable_network_does_not_reannounce() {
    // Steady state: the usable network is unchanged and no outage occurred. Re-creating the
    // sender here would force every connected OBS receiver to drop + reconnect the feed — so it
    // MUST NOT fire.
    let sig = NetworkSignature::from_addrs(["10.77.9.61"]);
    assert!(
        !should_reannounce(&sig, &sig.clone(), false),
        "a stable network MUST NOT re-announce (would needlessly drop connected receivers)"
    );
}

#[test]
fn same_address_after_an_outage_reannounces() {
    // A link bounce that returned the SAME address: the mDNS registration was dropped during
    // the outage, so even though the address set is unchanged the sender MUST re-register. The
    // feed was already disrupted by the outage, so the re-create costs nothing extra.
    let sig = NetworkSignature::from_addrs(["10.77.9.61"]);
    assert!(
        should_reannounce(&sig, &sig.clone(), true),
        "recovery from a down event MUST re-announce even on the same address (#297 link flap)"
    );
}

#[test]
fn down_event_alone_does_not_reannounce_while_still_down() {
    // The outage flag is set, but the network is STILL down (no address yet) — there is nothing
    // to announce on, so the empty-current guard must win over the down flag.
    let announced = NetworkSignature::from_addrs(["10.77.9.61"]);
    assert!(
        !should_reannounce(&announced, &NetworkSignature::default(), true),
        "an empty current network MUST NOT re-announce even after a down event"
    );
}

#[test]
fn enumeration_order_is_not_a_change() {
    // getifaddrs may return interfaces in any order; the SET, not the order, defines the
    // network. Two enumerations of the same addresses in different order are NOT a change.
    let announced = NetworkSignature::from_addrs(["10.77.9.61", "10.77.9.200"]);
    let current = NetworkSignature::from_addrs(["10.77.9.200", "10.77.9.61"]);
    assert_eq!(
        announced, current,
        "address SET equality must be order-independent"
    );
    assert!(
        !should_reannounce(&announced, &current, false),
        "the same address set in a different order is NOT a network change"
    );
}

#[test]
fn duplicate_addresses_are_not_a_change() {
    let announced = NetworkSignature::from_addrs(["10.77.9.61"]);
    let current = NetworkSignature::from_addrs(["10.77.9.61", "10.77.9.61"]);
    assert_eq!(
        announced, current,
        "duplicate addresses must canonicalize away"
    );
    assert!(!should_reannounce(&announced, &current, false));
}

#[test]
fn empty_current_network_never_reannounces() {
    // The network went (or is still) down — there is no address to announce on, so a
    // re-create is pointless churn. Wait for an address to appear (which is itself a change and
    // will trigger then).
    let announced = NetworkSignature::from_addrs(["10.77.9.61"]);
    let current = NetworkSignature::default();
    assert!(
        !should_reannounce(&announced, &current, false),
        "an empty current network MUST NOT re-announce (nothing to announce on)"
    );
}

#[test]
fn both_empty_never_reannounces() {
    let empty = NetworkSignature::default();
    assert!(!should_reannounce(&empty, &empty.clone(), false));
}

#[test]
fn blank_entries_are_ignored() {
    // Defensive: a blank / whitespace entry from the IO layer must not look like an address
    // (which would otherwise spuriously differ from a clean signature).
    let clean = NetworkSignature::from_addrs(["10.77.9.61"]);
    let with_blanks = NetworkSignature::from_addrs(["", "  ", "10.77.9.61"]);
    assert_eq!(clean, with_blanks);
    assert!(with_blanks.addrs().iter().all(|a| !a.trim().is_empty()));
}

#[test]
fn only_real_lan_nics_are_discoverable() {
    // The signature must key on the real LAN NIC only. Real NICs (predictable + legacy names)
    // count; loopback, container/VM bridges, virtual ethernet, and VPN/overlay tunnels do NOT
    // — otherwise a docker0/tailscale flap would needlessly drop the live program.
    for nic in ["enp3s0", "eno1", "ens33", "eth0", "wlp2s0", "wlan0"] {
        assert!(is_discoverable_interface(nic), "{nic} is a real LAN NIC");
    }
    for virt in [
        "lo",
        "docker0",
        "veth1a2b3c",
        "virbr0",
        "br-9f8e7d",
        "tailscale0",
        "tun0",
        "tap0",
        "wg0",
        "zt0",
        "",
        "   ",
    ] {
        assert!(
            !is_discoverable_interface(virt),
            "{virt:?} must NOT enter the discovery signature"
        );
    }
}

#[test]
fn poll_interval_is_bounded_to_seconds() {
    // "Discoverable within seconds" (#297) — the poll interval must be small (a few seconds at
    // most) so a network change is acted on promptly, but non-zero so it doesn't read
    // interfaces every frame.
    assert!(REANNOUNCE_POLL_INTERVAL > Duration::ZERO);
    assert!(
        REANNOUNCE_POLL_INTERVAL <= Duration::from_secs(5),
        "re-announce poll interval must be at most a few seconds for prompt rediscovery"
    );
}
