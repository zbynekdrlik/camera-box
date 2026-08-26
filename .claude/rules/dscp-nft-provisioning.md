---
paths:
  - "scripts/lib/dscp-nft.sh"
  - "tests/harness_dscp_nft_ds52.rs"
---

# NTP-client DSCP nftables rule (dantesync issue 52 provisioning half)

dantesync's Linux NTP client (`rsntp::SntpClient`) creates its UDP socket internally, so it has NO
handle to `setsockopt(IP_TOS)` — it can EF-mark the master's REPLIES (`dantesync/src/dscp.rs`) but
NOT its own client REQUESTS on Linux. The cam boxes mark the request direction (udp dport 123) at
the kernel netfilter layer, installed by provisioning, so the venue MikroTik CRS switches (TRUST-L3,
DSCP-in-hardware) prioritise it too.

## Mechanism (why this shape)
- `scripts/lib/dscp-nft.sh` is the single source of truth (content generators + verify verdict fns),
  consumed by THREE places: `setup-device.sh` STEP 16 (`nftables` pkg) + STEP 17c (write rule+unit,
  enable), `verify-device.sh` check `(ae)` (post-reboot acceptance), `create-usb-linux.sh` (base-image
  dual-bake: host-side file writes + chroot `nftables` install + `systemctl enable dantesync-dscp`).
- A DEDICATED `table ip dantesync_dscp` + tiny `dantesync-dscp.service` oneshot — **NOT** the distro
  `nftables.service`. The boxes ship no nftables config; a dedicated table (never `flush ruleset`,
  applied via the idempotent `table`/`delete table`/redefine atomic single-table replace) owns
  nothing but this one rule and coexists with any future firewall. `type filter hook output priority
  mangle; policy accept;` + a set-only statement means the rule can NEVER drop a packet.
- **DSCP class = EF (46) — must match `dantesync/src/dscp.rs`'s own default** so request and reply
  carry the same class end to end. If that default ever changes, change it here too.

## Tier-0 gotcha: nft needs ROOT NETLINK even for `-c`
`nft -c -f file` (check mode) AND any `nft list`/`nft -f` fail as non-root with
`netlink: Error: cache initialization failed: Operation not permitted` — so you canNOT syntax-check
or render an nftables ruleset in the Tier-0 (no-root, no-cargo) local context. Rootless
`unshare -rn` is also blocked on dev1 (`write failed /proc/self/uid_map: Operation not permitted`).
To render a REAL nft fixture / validate syntax locally without touching dev1's global ruleset:
```bash
sudo -n unshare -n bash -c '/usr/sbin/nft -f file && /usr/sbin/nft list ruleset'
```
(throwaway network namespace — the table is added + listed then discarded with the netns). Use the
EXACT rendered output as the GREEN fixture; nft renders EF as the keyword `ef`
(`udp dport 123 ip dscp set ef`).

## Test pitfalls
- A "never flushes" assertion must match a non-comment `flush` DIRECTIVE line, **not** the substring
  `flush ruleset` — the ruleset's own comment says "NEVER `flush ruleset`", so `!contains("flush
  ruleset")` FALSE-fails. Check `line.trim_start()` is not a `#` comment and starts with `flush `.
- `dscp_nft_rule_present` is deliberately anchorless + skips the priority line (survives nft render
  variance) and accepts `ef`/`0x2e`/`46`; test the numeric forms + a wrong-class (`cs0`) rejection.
- verify-device.sh runs `set -euo pipefail`: `dscp_nft_verdict` returns 0 on every path so a no-match
  grep never aborts the caller (#1133 class) — assert it under `set -e`.

## Conventions honored
- Check `(ae)` stays BEFORE `(q)` (the intentionally-last check; `provisioning-scripts.md`).
- Enable-only in provisioning (no live `systemctl start` / no `nft -f` apply) — effective next boot,
  proven live by verify `(ae)`. Fail-loud `systemctl enable` (no `|| true`).
- `setup-device.sh` uses bare `nft` (root sudo `secure_path` includes `/usr/sbin`); the verify
  gather snippet uses the absolute `/usr/sbin/nft` (a non-login ssh PATH may omit sbin dirs).
- Commits use a `feat(ds52-nft):` style prefix and say "dantesync issue 52" — NEVER a bare `#52`
  (camera-box #52 is a different ticket; a closing keyword would auto-close it, and the design gate
  scans for any `#N`).
