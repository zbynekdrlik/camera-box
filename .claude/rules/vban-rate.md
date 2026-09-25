---
paths:
  - "scripts/vban_rate.py"
  - "scripts/vban-rate-alert-watchdog.sh"
  - "systemd/vban-rate-alert-watchdog.*"
  - "tests/python/test_vban_rate*_1372.py"
---

# Inter-PC VBAN rate + loss — by FRAME COUNTER, never by packet count (issue 1372 part C)

## The measurement (`scripts/vban_rate.py`, pure, pytest Tier-0)

- **Rate = the VBAN frame counter**, never the packet count. Each header carries `nuFrame` (u32 LE at
  byte 24); a packet's sample index is `nuFrame x (nbs+1)`; the stream clock is the least-squares
  slope of that index against capture time, pooled over counter segments (common slope, one
  intercept per segment). A lost packet leaves a hole but moves no point, so loss never biases rate.
  The count method read −132.9 ppm for a +18.1 ppm stream with 5 lost packets (#1367 comment
  5832526338) — a count mixes two faults into one number.
- **Loss** = per segment `(max − min + 1) − unique`: a pktmon duplicate (the same packet at several
  components) or a reorder is never loss. **Jump** = a step back > 64 frames or forward beyond
  2 × the frames the elapsed time explains + 64 (a sender restart); a real outage advances the
  counter by ~the elapsed frames and therefore counts as LOSS, not a jump.
- Reported alongside: `max_gap_ms`, the fit residual (arrival jitter — obs-vban bursts), the slope
  stderr. A stream shorter than `--min-span-s` (default 20 s) reads SHORT, never a FAULT.
- **Formats:** classic pcap (either byte order, µs/ns) LINUX_SLL2 276 (`tcpdump -i any`), SLL 113,
  EN10MB 1 (+802.1Q), raw IPv4; pcapng (pktmon `etl2pcap`, any `if_tsresol`). IPv4/UDP only;
  fragments skipped. **snaplen ≥ 96** or the 28-byte header is cut: every-header-cut =
  `CAPTURE_TRUNCATED`, a loud config error, never an empty OK.
- `--dst IP` keeps only streams ARRIVING at the receiver (a strih-lx capture also sees the hub's own
  strih-lx → camN streams, which say nothing about a peer's clock). `--json` / `--tsv` for tools.

## The watchdog (`scripts/vban-rate-alert-watchdog.sh`, dev1, SHIPS DISABLED)

- One pass = a ~60 s `tcpdump -i any -s 96 -U -w - 'udp and udp[8:4] = 0x5642414e'` on strih-lx
  over ssh, sudo fed on stdin (`sudo -S -p ''`), pcap streamed back to dev1. strih-lx is the
  reference receiver because Linux adjtimex slews CLOCK_REALTIME → its capture clock is the
  dantesync-disciplined clock. `timeout` exit 124 = a complete capture.
- Without the magic BPF a 15 s capture was 192 MB (NDI UDP); with it ~10 MB per 40 s.
- Grades against nominal ±`VBAN_RATE_PPM_BOUND` (20) and `VBAN_RATE_LOSS_CEILING` (1e-4) — both
  PROVISIONAL: calibrate from data once part A (Windows OBS on the dantesync rate) is live. A
  Dante-clocked sender still differs from system time by dantesync's `f_phase`, so the bound must
  leave room for it.
- Production-critical re-ping (on-air audio): `watchdog_notify_key` time bucket, allowlisted in
  `test_notify_dedup_key_sweep_1206.py`. The incident key is stream NAME + sender IP, never the
  sender's ephemeral source port. 2-pass confirm; recovery = one log line; capture failure / no
  streams / SHORT = SKIP. REPORT-ONLY: no E2E step reads it (no clean single-line seam in
  recording-e2e.sh for a 60 s capture — kept as watchdog + CLI).
- Seam: `VBAN_RATE_CAPTURE_CMD <outfile>` replaces the ssh capture (tests feed synthetic pcaps).

## Live baseline (25.9.2026, before part A)

`fohabl-strih` (10.77.7.30, 96 kHz, 103 samples/frame) at strih-lx: +10.3 ppm, 39 lost / 40 s
(1.05e-3) and 49 / 60 s (8.8e-4) — the FOH VBAN OUT loss. The hub's own outputs: −0.6 ppm, 0 loss.

## Windows side (pktmon)

The same parser reads a pktmon capture converted with `pktmon etl2pcap` (pcapng, EN10MB). Filter to
the NIC component or pktmon's per-component copies show up as `dup=` (harmless — dedup is by counter).
