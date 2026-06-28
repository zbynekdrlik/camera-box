//! #297 — NDI sender re-announce trigger.
//!
//! These tests pin the PURE re-announce decision (`should_reannounce`) + the network
//! signature it keys on. The bug being fixed: the camera-box NDI sender is created ONCE at
//! startup and never re-registers, so a box whose network came up after start (or flapped)
//! announces on a dead/old network and never appears in the OBS NDI source dropdown. The
//! trigger must re-announce precisely when the usable network CHANGES — and must NOT
//! re-announce on a stable network (a re-create drops every connected receiver) nor while the
//! network is still down (nothing to announce on yet).

use camera_box::reannounce::{should_reannounce, NetworkSignature, REANNOUNCE_POLL_INTERVAL};
use std::time::Duration;

#[test]
fn network_change_triggers_reannounce() {
    // The box booted with one address, then the real LAN address settled (or the link
    // flapped to a new IP). This is the #297 case: the sender announced on the OLD network and
    // must re-register on the NEW one so OBS rediscovers it.
    let announced = NetworkSignature::from_addrs(["169.254.10.20"]); // link-local at boot
    let current = NetworkSignature::from_addrs(["10.77.9.61"]); // real LAN address now
    assert!(
        should_reannounce(&announced, &current),
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
        should_reannounce(&announced, &current),
        "an address appearing after start MUST trigger a re-announce (#297 boot race)"
    );
}

#[test]
fn stable_network_does_not_reannounce() {
    // Steady state: the usable network is unchanged. Re-creating the sender here would force
    // every connected OBS receiver to drop + reconnect the feed — so it MUST NOT fire.
    let sig = NetworkSignature::from_addrs(["10.77.9.61"]);
    assert!(
        !should_reannounce(&sig, &sig.clone()),
        "a stable network MUST NOT re-announce (would needlessly drop connected receivers)"
    );
}

#[test]
fn enumeration_order_is_not_a_change() {
    // getifaddrs may return interfaces in any order; the SET, not the order, defines the
    // network. Two enumerations of the same addresses in different order are NOT a change.
    let announced = NetworkSignature::from_addrs(["10.77.9.61", "10.77.9.200"]);
    let current = NetworkSignature::from_addrs(["10.77.9.200", "10.77.9.61"]);
    assert_eq!(announced, current, "address SET equality must be order-independent");
    assert!(
        !should_reannounce(&announced, &current),
        "the same address set in a different order is NOT a network change"
    );
}

#[test]
fn duplicate_addresses_are_not_a_change() {
    let announced = NetworkSignature::from_addrs(["10.77.9.61"]);
    let current = NetworkSignature::from_addrs(["10.77.9.61", "10.77.9.61"]);
    assert_eq!(announced, current, "duplicate addresses must canonicalize away");
    assert!(!should_reannounce(&announced, &current));
}

#[test]
fn empty_current_network_never_reannounces() {
    // The network went (or is still) down — there is no address to announce on, so a
    // re-create is pointless churn. Wait for an address to appear (which is itself a change and
    // will trigger then).
    let announced = NetworkSignature::from_addrs(["10.77.9.61"]);
    let current = NetworkSignature::default();
    assert!(
        !should_reannounce(&announced, &current),
        "an empty current network MUST NOT re-announce (nothing to announce on)"
    );
}

#[test]
fn both_empty_never_reannounces() {
    let empty = NetworkSignature::default();
    assert!(!should_reannounce(&empty, &empty.clone()));
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
