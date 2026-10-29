//! Issue 1387: the speed of the network link a JPEG XS datagram uses, read from sysfs.
//!
//! Both ends of the link read it (owner 29.10.2026: "nesmie to stáť na sieťovom tuningu"): the
//! RECEIVER names the interface its slices arrive on (`IP_PKTINFO`'s ifindex) and carries that
//! interface's speed in its report; the SENDER reads its own (the connected socket's local
//! address). The pace then follows the slower one by itself (`super::pace_auto`), on any NIC.
//!
//! `/sys/class/net/<if>/speed` holds the negotiated speed in Mb/s. It reads `-1` (or fails with
//! `EINVAL`) when the driver does not know it: a link that is down, Wi-Fi, a tunnel, loopback.
//! A virtual device on top of a real one (a VLAN, a bond, a bridge, macvlan) names its lower
//! devices as `lower_<name>` links in the same directory: when its own speed is unknown the
//! slowest known lower device counts (a bond of 1 + 10 GbE paces for 1 GbE: the slower member may
//! carry the flow). Nothing known = `None` (the caller's safe start).

use std::path::Path;

/// Where the kernel lists the network interfaces.
pub const SYS_CLASS_NET: &str = "/sys/class/net";
/// How deep a stack of virtual devices is followed (VLAN on bond on NIC = 2).
const MAX_LOWER_DEPTH: u32 = 4;
/// The highest speed taken as real (1.6 Tb/s Ethernet); anything above is a driver's garbage.
pub const MAX_LINK_MBPS: u32 = 1_600_000;

/// A sysfs `speed` file's text as Mb/s: a positive whole number up to [`MAX_LINK_MBPS`];
/// `-1`, `0`, blank or anything else = unknown.
pub fn parse_speed_mbps(text: &str) -> Option<u32> {
    let _ = text;
    None
}

/// Whether `name` can be a single directory entry under [`SYS_CLASS_NET`] (never a path).
fn plain_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && name.len() <= 64
}

/// The speed of interface `iface` under `root` (a stand-in for [`SYS_CLASS_NET`] in tests): its
/// own `speed`, else the slowest known of its lower devices (module doc), else `None`.
pub fn link_speed_mbps_in(root: &Path, iface: &str) -> Option<u32> {
    speed_at(root, iface, 0)
}

fn speed_at(root: &Path, iface: &str, depth: u32) -> Option<u32> {
    let _ = (root, iface, depth, MAX_LOWER_DEPTH, plain_name("x"));
    None
}

/// [`link_speed_mbps_in`] on the live [`SYS_CLASS_NET`].
pub fn link_speed_mbps(iface: &str) -> Option<u32> {
    link_speed_mbps_in(Path::new(SYS_CLASS_NET), iface)
}

/// The name of interface number `ifindex` (`if_indextoname`), `None` when there is none.
pub fn iface_name(ifindex: u32) -> Option<String> {
    let _ = ifindex;
    None
}

/// The interface that carries the local IPv4 address `ip` (`getifaddrs`), `None` when none does
/// (an unspecified address, or one no interface holds).
pub fn iface_of_local_ipv4(ip: std::net::Ipv4Addr) -> Option<String> {
    let _ = ip;
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_net() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let mk = |name: &str, speed: Option<&str>| {
            std::fs::create_dir_all(d.path().join(name)).unwrap();
            if let Some(s) = speed {
                std::fs::write(d.path().join(name).join("speed"), s).unwrap();
            }
        };
        mk("enp56s0", Some("1000\n"));
        mk("enx00e04c", Some("2500\n"));
        mk("wlp0", Some("-1\n"));
        mk("lo", None);
        // a VLAN on the 1 GbE NIC: its own speed unknown, one lower device
        mk("enp56s0.70", Some("-1\n"));
        std::os::unix::fs::symlink("../enp56s0", d.path().join("enp56s0.70/lower_enp56s0"))
            .unwrap();
        // a bond of the 1 GbE and the 2.5 GbE NIC: the slower member counts
        mk("bond0", None);
        std::os::unix::fs::symlink("../enp56s0", d.path().join("bond0/lower_enp56s0")).unwrap();
        std::os::unix::fs::symlink("../enx00e04c", d.path().join("bond0/lower_enx00e04c")).unwrap();
        // a bridge over the VLAN: two levels down
        mk("br0", Some("-1\n"));
        std::os::unix::fs::symlink("../enp56s0.70", d.path().join("br0/lower_enp56s0.70")).unwrap();
        d
    }

    #[test]
    fn a_speed_file_reads_as_mbps_and_unknown_is_none() {
        assert_eq!(parse_speed_mbps("1000\n"), Some(1000));
        assert_eq!(parse_speed_mbps(" 10000 "), Some(10_000));
        assert_eq!(parse_speed_mbps("2500"), Some(2500));
        for unknown in ["-1\n", "0", "", "fast", "4294967295", "1600001"] {
            assert_eq!(parse_speed_mbps(unknown), None, "{unknown:?}");
        }
    }

    #[test]
    fn a_nic_reads_its_own_speed_and_a_virtual_device_its_slowest_lower_one() {
        let d = fake_net();
        let r = d.path();
        assert_eq!(link_speed_mbps_in(r, "enp56s0"), Some(1000));
        assert_eq!(link_speed_mbps_in(r, "enx00e04c"), Some(2500));
        assert_eq!(
            link_speed_mbps_in(r, "enp56s0.70"),
            Some(1000),
            "a VLAN = its NIC"
        );
        assert_eq!(
            link_speed_mbps_in(r, "bond0"),
            Some(1000),
            "the slower member"
        );
        assert_eq!(link_speed_mbps_in(r, "br0"), Some(1000), "two levels down");
        assert_eq!(link_speed_mbps_in(r, "wlp0"), None, "Wi-Fi: unknown");
        assert_eq!(link_speed_mbps_in(r, "lo"), None, "no speed file");
        assert_eq!(link_speed_mbps_in(r, "missing"), None);
        assert_eq!(link_speed_mbps_in(r, "../etc"), None, "never a path");
    }

    #[test]
    fn a_loop_of_lower_links_ends_unknown_instead_of_recursing_forever() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("a")).unwrap();
        std::fs::create_dir_all(d.path().join("b")).unwrap();
        std::os::unix::fs::symlink("../b", d.path().join("a/lower_b")).unwrap();
        std::os::unix::fs::symlink("../a", d.path().join("b/lower_a")).unwrap();
        assert_eq!(link_speed_mbps_in(d.path(), "a"), None);
    }

    #[test]
    fn the_loopback_interface_is_named_and_found_by_its_address() {
        // ifindex 1 is the loopback on Linux; 127.0.0.1 is on it
        let lo = iface_name(1).expect("ifindex 1 has a name");
        assert_eq!(
            iface_of_local_ipv4(std::net::Ipv4Addr::LOCALHOST).as_deref(),
            Some(lo.as_str())
        );
        assert_eq!(iface_name(0), None);
        assert_eq!(iface_of_local_ipv4(std::net::Ipv4Addr::UNSPECIFIED), None);
        assert_eq!(
            iface_of_local_ipv4(std::net::Ipv4Addr::new(192, 0, 2, 77)),
            None
        );
    }
}
