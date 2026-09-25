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
- **The fit is ONE-SIDED TRIMMED (review round 1).** Delay only makes an arrival LATER, so a plain
  fit over every arrival was biased by one stall-then-burst (+65.7 ppm for a single 500 ms stall in
  60 s). Points later than the line by more than `max(4 × 1.4826 × MAD, 2 ms)` above the median
  lateness are dropped and the line refitted (≤ 4 rounds). Symmetric jitter is bounded and keeps
  every point. A per-window lower-envelope fit was tried first and REJECTED: with ~190 packets per
  1 s window the minimum is itself noisy at ~0.2 ms, which is ±10 ppm over 60 s.
- **The stderr is cluster-robust (review round 4):** packets arriving < 1 ms apart were released as
  one burst and share one timing error, so each arrival cluster is ONE observation (a sandwich
  estimator). A per-packet stderr was ~2x too small on obs-vban bursts, and a true 0 ppm clock graded
  FAULT on 2/40 seeds. With ±20 ms burst jitter a 60 s capture resolves only ~14 ppm (1 σ), so such a
  stream honestly grades UNCERTAIN; the live strih-lx streams jitter ~0.4-2.7 ms rms.
- **Known limit:** a sender that restarts less than a second after the capture began, onto counters
  within ~50 ms worth of its head, can be merged under heavy jitter (its continuation is only
  ~50 ms late). A real restart from a running sender (a large counter) is always a jump. A sender
  that resets its counter with ZERO send pause while its old packets are still reordered in flight
  can book phantom loss in the old segment (review round 5: 11/20 fohabl-model seeds at 0 ms pause,
  0/20 at >= 5 ms). Not handled: a VBAN sender reset is a process restart, never under 5 ms.
- **Grading by the 2-stderr interval (review round 3):** FAULT when `|rate| − 2σ` is outside the
  bound (a gross fault is a FAULT however noisy the fit), OK when `|rate| + 2σ` stays inside it,
  UNCERTAIN when the interval straddles it (no page, no recovery; loss is still graded).
- **Loss** = per segment `(max − min + 1) − unique`: a pktmon duplicate (the same packet at several
  components) or a reorder is never loss. **Jump** = a step back > 64 frames, a step back to a
  counter already seen more than 50 ms earlier or below everything the segment has seen (a restart
  right after the capture began), or a step forward beyond 2 × the frames the elapsed time explains
  + 64 (a sender restart). A real outage advances the counter by ~the elapsed frames and therefore
  counts as LOSS, not a jump. **Every jump candidate is confirmed by lookahead (review rounds 2-3):**
  counters the segment already holds, counters still in flight around the head (an ordinary
  reorder, review round 4) and further stragglers far below the head (a second late original,
  round 5) are skipped, and the first counter ABOVE the head decides. If it continues the OLD head AND
  (when the skipped counters cover the whole gap back to the candidate, i.e. a possible restart
  re-send) arrives on time for that step (within 50 ms of step / rate after the last accepted packet),
  the candidate is a straggler or a late duplicate: a duplicate stays a duplicate, a straggler fills its
  own hole, and a lone far-ahead counter or a pre-segment packet is dropped, so there is never phantom
  loss. If it continues from the candidate, it is a restart. A restart whose re-sent counters collide
  with the old ones is still a restart, because its continuation arrives LATE for the old head. A
  reorder in a segment's first 50 ms (2,0,1 at capture start) is never a jump.
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
- **Never silently blind (review round 1).** The remote sudo/tcpdump stderr is kept. A capture that
  fails `VBAN_RATE_CAPTURE_FAIL_PASSES` (3) passes in a row while strih-lx answers ssh :22 (tcpdump
  missing, a wrong sudo password) pages once (`vban-rate-capture-<box>`); a failure with the box down
  is a SKIP. A stream not GRADED (OK/FAULT) for longer than `VBAN_RATE_STALE_S` (900 s) restarts its confirm count; the blind-capture page uses a STABLE key (a chronic config fault, one page per incident). The step limits use the arrival of the last ACCEPTED packet, so a late duplicate never shrinks
  them (a duplicate near the end of an outage cannot hide the outage's loss), and the lookahead treats
  the rest of a late-duplicate burst as the old stream.
- Seams: `VBAN_RATE_CAPTURE_CMD <outfile>` replaces the ssh capture (tests feed synthetic pcaps);
  `VBAN_RATE_BOX_UP_CMD` replaces the ssh :22 probe.

## Live baseline (25.9.2026, before part A)

`fohabl-strih` (10.77.7.30, 96 kHz, 103 samples/frame) at strih-lx: +10.3 ppm by the plain fit,
+7.7 ppm by the trimmed fit (its late tail was the bias), 39 lost / 40 s (1.05e-3) and 49 / 60 s
(8.8e-4) — the FOH VBAN OUT loss. The hub's own outputs: −0.55 ppm, 0 loss.

## Windows side (pktmon)

The same parser reads a pktmon capture converted with `pktmon etl2pcap` (pcapng, EN10MB). Filter to
the NIC component or pktmon's per-component copies show up as `dup=` (harmless — dedup is by counter).
