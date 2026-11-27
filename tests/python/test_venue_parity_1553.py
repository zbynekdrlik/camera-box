"""Issue 1553 -- SNV/PP parity of the box facts (owner 25.11.2026: "ako to ze nemas mechanizmus ktory
toto zachytava pri praci").

On 25.11.2026 four hardenings made for SNV were missing at PP and every check passed: the rig-NIC CPU
guard (STRIH_HOUSEKEEPING_CPUS), the Realtek r8152 driver (STRIH_NIC_OOT_DRIVER), the Dante RX hand-off
margin and the switch QoS. Every fact accepts `none`, and nothing compared the venues.

The rule: for every role, every fact key carries the same value on each venue's box, unless the key is
per-box by nature (identity, addressing, venue hardware: IDENTITY_KEYS below) or is listed in
scripts/strih-boxes/venue-parity-exceptions.txt as `role|KEY|reason` with a reason. A new key is
parity-checked from its first commit; an exception whose values agree is stale and fails too.

Tier-0: parses the committed fact files, no box.
"""
import pathlib
import re

_BOXES = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "strih-boxes"
_EXCEPTIONS = _BOXES / "venue-parity-exceptions.txt"

# Per-box by nature: who the box is, where it is and how it is reached, which cameras/peers a venue has.
IDENTITY_KEYS = {
    "STRIH_HOSTNAME", "STRIH_IP", "STRIH_NDI_PREFIX", "STRIH_VENUE", "STRIH_CAMERAS", "STRIH_CAMERA_DEV1_NET",
    "STRIH_COMPANION_HOST", "STRIH_CG_SENDER", "STRIH_INTERCOM_CONFIG", "STRIH_OBS_PROFILE", "STRIH_OBS_COLLECTION",
    "STRIH_NDI_RUNTIME_PEER", "STRIH_DANTE_DEVICES", "STRIH_HTTPS_INTERKOM_HOSTS", "STRIH_HTTPS_SHADING_HOST",
    "STRIH_JXS_CAMERAS", "STRIH_BKSHADING_RELAYS", "STRIH_DANTESYNC_UPSTREAM", "STRIH_PROGRAM_SENDER",
    "STRIH_NIC_RENAME", "STRIH_DANTE_GM_ADDR",
}


def _facts(path):
    out = {}
    for line in path.read_text().splitlines():
        m = re.match(r"^(STRIH_[A-Z0-9_]+)=(.*)$", line)
        if m:
            out[m.group(1)] = m.group(2)
    return out


def _boxes_by_role():
    roles = {}
    for f in sorted(_BOXES.glob("*.env")):
        facts = _facts(f)
        roles.setdefault(facts["STRIH_ROLE"], []).append((f.stem, facts))
    return roles


def _exceptions():
    out = {}
    for n, line in enumerate(_EXCEPTIONS.read_text().splitlines(), 1):
        if not line.strip() or line.startswith("#"):
            continue
        parts = line.split("|", 2)
        assert len(parts) == 3 and parts[2].strip(), f"{_EXCEPTIONS.name}:{n} must be role|KEY|reason: {line!r}"
        out[(parts[0].strip(), parts[1].strip())] = parts[2].strip()
    return out


def test_every_hardening_fact_is_the_same_at_both_venues_or_has_a_reason():
    exc = _exceptions()
    bad = []
    for role, boxes in _boxes_by_role().items():
        keys = set().union(*(f.keys() for _, f in boxes))
        for key in sorted(keys - IDENTITY_KEYS):
            values = {b: f.get(key) for b, f in boxes}
            if len(set(values.values())) > 1 and (role, key) not in exc:
                bad.append(f"{role} {key}: {values}")
    assert not bad, "facts that differ between the venues with no recorded reason:\n  " + "\n  ".join(bad)


def test_no_stale_exception():
    roles = _boxes_by_role()
    stale = []
    for (role, key), _ in _exceptions().items():
        values = {f.get(key) for _, f in roles.get(role, [])}
        if len(values) <= 1:
            stale.append(f"{role}|{key}")
    assert not stale, f"exceptions whose values now agree (remove them): {stale}"


def test_both_venues_have_each_role():
    for role, boxes in _boxes_by_role().items():
        venues = {f["STRIH_VENUE"] for _, f in boxes}
        assert {"snv", "pp"} <= venues, (role, venues)
