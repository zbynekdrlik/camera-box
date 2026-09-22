---
paths:
  - "scripts/recording-verdict-on-strih-lx.sh"
  - "tests/recording_verdict_on_strih_lx_lowprio.rs"
---

# strih-lx in-place decode runs at idle priority on the E-cores (issue 1351 item / issue 1354)

`scripts/recording-verdict-on-strih-lx.sh` is the Linux sibling of `recording-verdict-on-strih.sh`:
at `[8/8a]` it ssh-runs `recording-verdict --extract-partial strih …` ON strih-lx (10.77.9.202)
against the recording where it already lives, then scps back only the small partial JSON (+ the
`<partial>-pixels` dir). Nothing is copied off-box.

## Why STEP 2 must be low-priority + E-core-pinned

The decode is a multi-core QR/pixel sweep over a 1080p60 5-min recording. Run at nice 0, unpinned,
it competes with OBS's `ndir:video` receiver decode threads on the P-cores, so during the
`[7/8]` StopRecord + `[8/8a]` decode window **every live strih-lx NDI input relock-storms for
minutes** (measured live, issue 1354: cam1 +763, cam4 +799, cam5 +832, cam7 +782 relocks vs 0
during the recording itself). It is verdict-neutral (the recording is already closed) but it leaves
every FIFO in a relock regime that the next run's `[4j/8settle]` then has to wait out — and it is
exactly the failure a bulk job on the strih notebook would cause during a production.

## The shape (pure helper + remote-shell replica)

- `strih_lx_lowprio_prefix <sysfs_root>` is the **canonical, pure, network-free encoding**: always
  `nice -n 19`; plus `taskset -c <range>` onto the Intel hybrid E-cores when
  `<root>/devices/cpu_atom/cpus` is a non-empty range; else `nice -n 19` alone (never a bare
  `taskset -c ""`). It reads only the passed root, so a unit test points it at a fake sysfs tree.
  strih-lx (measured 22.9.2026): 16 CPUs, `cpu_atom` = E-cores `12-15`, `cpu_core` = P-cores `0-11`.
- **STEP 2 in `main()` is the REMOTE-SHELL REPLICA of that contract, not a call to the helper** — the
  E-core range must be resolved ON the box (a replacement notebook has its own core map; dev1 has no
  `cpu_atom`). So `LOWPRIO_SNIPPET` is **single-quoted on the dev1 side** (`LP="nice -n 19"; if [ -s
  /sys/devices/cpu_atom/cpus ]; then LP="$LP taskset -c $(cat …)"; fi;`), so dev1 never expands the
  `$(cat …)` — the strih-lx shell does — and `\$LP` (escaped) prefixes the on-box command inside a
  brace group so `mkdir -p` still gates the decode (the `&&` binds the whole group, not just the
  `LP=` assignment the snippet starts with):
  `ssh … "mkdir -p '$OUT_DIR' && { $LOWPRIO_SNIPPET \$LP $ONSTRIHLX_CMD; }"`.
- This is the repo's usual "pure decision + shell replica pinned by a test" pattern: the helper is
  what `tests/recording_verdict_on_strih_lx_lowprio.rs` pins (fixture roots), and a static anchor
  pins that STEP 2's ssh line launches `$ONSTRIHLX_CMD` through the prefix. Keep the two in parity.

## Gotchas

- The binary `scp` and the partial/pixels pull-back stay **byte-identical** — the change is confined
  to STEP 2's launch prefix.
- `$(cat …)` strips the sysfs file's trailing newline (design note); the helper also `tr -d`s
  whitespace so a whitespace-only file falls back to `nice`-only. The remote replica guards on
  `[ -s … ]` instead, so helper and replica agree on the only two states kernel sysfs actually
  emits — absent (non-hybrid → `nice -n 19`) and a clean range (hybrid → `nice -n 19 taskset -c
  <range>`, verified live 12-15) — and diverge only on a whitespace-only `cpus` file, which sysfs
  never produces (the helper's whitespace fallback is a unit-test-only defensive path).
- The only test that touches this script is `harness_strih_platform_1351.rs` (all `.contains()`
  substring anchors — a new helper/comment cannot break a `.find()`/`.split()` slice here), plus the
  new lowprio test. Run the occurrence-count anchor sweep after any edit anyway.
- Acceptance is a supervisor rig step: the next full E2E showing **zero relocks on strih-lx during
  `[8/8a]`**.
