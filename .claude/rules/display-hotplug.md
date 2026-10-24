---
paths:
  - "vendor/obs-studio/libobs/obs-drm-output-retry.c"
  - "vendor/obs-studio/libobs/obs-drm-output-retry.h"
  - "vendor/obs-studio/libobs/obs-drm-output-logcap.h"
  - "src/drm_output_retry.rs"
  - "tests/drm_output_retry_1424.rs"
  - "tests/drm_output_retry_supervisor_1424.rs"
  - "tests/drm_output_vk_park_1424.rs"
  - "scripts/lib/obs-box-panel.sh"
  - "scripts/strih_display_watch.py"
  - "systemd/strih-display-watch.service"
  - "systemd/obs-box-panel.service"
  - "scripts/lib/strih-x-display.sh"
  - "tests/python/test_strih_display_watch_1424.py"
  - "tests/python/test_strih_x_display_1424.py"
  - "tests/python/test_drm_output_retry_grade_1424.py"
---

# Display hotplug: the Linux OBS boxes work without a monitor, after one is plugged in and after one is switched on (issue 1424)

Owner, 24.10.2026, after strih-pp booted with the PP venue dark: "to musi vediet fungovat aj bez monitora
aj po zapojeni monitora aj po zapnuti!!!" / "nesuhlasim s takym stavom ... Treba to riesit!!". Main
design 6386467303 (Approach 1, three layers); finding 6386340389; validation 6386591234.

## What broke (strih-pp, 24.10.2026 17:52 boot, read live)

- Only the BenQ (HDMI-0, the Multiview, a fixed vk-direct output) was on. NVIDIA X set its boot MetaMode
  `DFP-0:nvidia-auto-select` on it; the autostart turned it off and left an 8x8 screen with 0 monitors.
- OBS: `vkAcquireXlibDisplayEXT('HDMI-0') FAILED VkResult -13` with NO `still in the X layout` line
  before it (so no X CRTC: not the known cause), and `no Vulkan device drives output 'HDMI-1'` (the
  Blackmagic off). Both outputs dormant; the vk-direct outputs started ONCE at the autostart.
- tint2 (obs-box-panel.service) segfaulted every 3 s on the empty screen.
- Nothing put the operator's NEC (DP) into the X layout when it came on (openbox kiosk, no daemon).
- `nvidia-settings -q`: `EnabledDisplays` 0, `CurrentMetaMode` NULL, `AllowEmptyInitialConfiguration`
  already on from the package's `10-nvidia.conf`. Every boot with the NEC on acquired both outputs.

## Layer 1 -- X never sets a mode on a fixed output (`scripts/lib/strih-x-display.sh`)

- ONE `/etc/X11/xorg.conf.d/15-camera-box-fixed-hdmi.conf`: `OutputClass` `MatchDriver "nvidia-drm"`,
  `AllowEmptyInitialConfiguration` and `MetaModes` over the OPERATOR outputs only (every NVIDIA connector X
  lists minus the fixed ones of the drm-output config, and never an HDMI output: step 6 runs on the
  strih role only, where no HDMI is ever the desktop, issue 1346). strih-pp: `DP-0..DP-3:
  nvidia-auto-select`; strih-snv: `DP-0: nvidia-auto-select` (its panel is the iGPU sink `eDP-1-2`, not
  the driver's).
- NVIDIA connector names = one word + a number (`DP-0`, `HDMI-1`); a sink output carries a provider
  suffix (`eDP-1-2`, `DP-1-5`) and is left out. The NVIDIA README (Display Device Names) accepts connector
  names in MetaModes; an OutputClass carries a Screen option (the package's own AllowEmpty works that way).
- Written by setup-strih step 6 (`strih_x_display_provision`, called at the end of
  `strih_drm_output_provision`, AFTER the config): write-if-changed, removed when nothing is armed, a
  loud SKIP when xrandr names no output, and NEVER an X restart -- it applies at the next X start.
- Graded by the display watch's `(display-x-config)` row: the file = the render for the box's config and
  X outputs; OK when the running X's log carries the `Option "MetaModes" "..."`, NOTE when the running X
  started without it, FAIL on a missing / hand-edited file or a fixed output that has an X mode now.
  The same row FIRST FAILs a display-watch policy file (layer 2) that differs from the role's.
- **UNVERIFIED, read it on the next X start (supervisor):** whether a MetaMode with NO connected operator
  output yields an empty X (AllowEmpty) or falls back to `nvidia-auto-select` on every connected display
  (which would put HDMI-0 back in -- today's state, no worse), and whether X keeping HDMI-0 out of its
  MetaMode is what frees it for the acquire. The -13 of 24.10 had no X CRTC on HDMI-0, so the driver's
  refusal there is one of: X had no other display at all, HDMI-0 was the boot display, or it was the
  RandR primary. The watch clears the primary when no operator output is on; the in-OBS retry (layer 3)
  re-acquires as soon as the state that refused it changes.

## Layer 2 -- the session display watch (`scripts/strih_display_watch.py`)

- `strih-display-watch.service`, a session app in the strih-session-apps framework on BOTH roles (6th on
  strih, 4th on stream): reads `xrandr --current` every 2 s (cached state: no DDC re-probe of the
  monitors), pure `decide`:
  - a connected OPERATOR output with no mode -> `--auto`, `--right-of` the rightmost operator output on,
    the first one `--primary`; an active operator output without primary when none has it -> `--primary`;
  - a FIXED output that X drives -> `--off`, with `--fb` in the SAME call when no operator output is on
    (a lone `--off` makes xrandr shrink the screen to 8x8 -- the 24.10 state);
  - no operator output on -> the screen kept >= 1920x1080 (`--fb`), `--noprimary`;
  - an operator output that is on is never touched (unplugged or not); an output OBS holds is never named;
  - a config that cannot be classified -> no output touched, only the screen size; an existing config
    that cannot be READ (EACCES, EIO, a directory) is "cannot classify" too, only a missing file is "no
    config" (review finding: a read error read as "no config" could `--auto` an HDMI OBS holds).
- **The role's HDMI policy (review finding):** the session-app units install verbatim on both roles, so
  the policy travels as data. Step 16d writes `~/.config/strih-display-watch.env` =
  `STRIH_DISPLAY_WATCH_HDMI=off` where the role's `6-hdmi` step applies (the strih role: the kiosk
  autostart keeps every `^HDMI` off, issue 1346), else `=operator` (the stream role keeps HDMI to X); a
  changed file try-restarts a running watch. The unit reads it (`EnvironmentFile=-...`) and with `off`
  the watch treats EVERY HDMI output like a fixed one (`effective_fixed`): never `--auto`, `--off` when X
  drives one, also with no / a rolled-back drm-output config. The `watching` line says `HDMI never on the
  desktop (this role, issue 1346)`.
- ONE xrandr call per change; a failed one retried after 10 s, logged once per distinct error. One
  `watching ...` line per change of the X picture. `--check-state` = the `(display-watch-pass)` row.
- **tint2 never starts on an empty screen:** `obs-box-panel.service` `ExecStart` runs
  `strih_display_watch.py --exec-when-screen /usr/bin/tint2 -c ...`, which waits (one log line) until X
  has a screen >= 320x240 WITH a monitor on it, then execs tint2 in place (the panel row's
  `/proc/<MainPID>/exe` = tint2 from then on). Type=simple, so the autostart's `systemctl --user start`
  never blocks on it. With no monitor at all the panel unit is active but tint2 is not running: verify's
  `(panel-unit)` row FAILs `main process=.../python3` and names it (`the panel waits for a monitor`) --
  honest, there is no screen. The unit has `ConditionPathExists=/usr/local/bin/strih_display_watch.py`
  (step 16d installs it, not the panel's 16h): without it a clean skip, and the row names step 16d.
- Rejected: `xrandr --query` polling (a re-probe reads every monitor's EDID over DDC every 2 s).
  **UNVERIFIED (supervisor):** that NVIDIA updates `--current` on a hotplug by itself; if a monitor
  plugged in is not seen, the runbook step below shows it and the cure is a periodic `--query`.

## Layer 3 -- the in-OBS retry (vendored libobs, `obs-drm-output-retry.{c,h}`)

- **The supervisor thread** (started at the end of `obs_drm_output_maybe_autostart` when a vk-direct
  output exists, stopped FIRST in `obs_drm_output_stop`, outside the registry lock) looks at every
  vk-direct output twice a second and runs the pure `drm_output_retry_step`:
  - open + present loop alive -> LIVE;
  - open + loop dead (`vkd_alive` = active + `drm_output_vk_wants_frames`) -> RELEASE: the registry stop
    (audio leg, then the vk close), then dormant;
  - not open -> DORMANT: the whole `drm_output_inst_start_locked` (EDID, open, audio leg) again after
    2 s for the first 5 failures, 5 s up to 20, then 30 s (a screen connected but off would otherwise
    build a fresh Vulkan instance every 5 s forever); 30 s while X RandR reads the connector
    `disconnected`, and AT ONCE when it reads connected again (the retry state's `unplugged` flag: a
    look that reads it disconnected marks it, the next connected look pulls the try in -- never read off
    the next-try distance, which the 30 s connected backoff would make ambiguous). X is asked only for a
    dormant output, outside the registry lock (`started` is read under it); every reply's X error is
    taken and freed (one handed NULL stays in the event queue nobody reads). A screen that is switched
    off but keeps its HDMI hot-plug line asserted never reads `disconnected`, so after ~85 s of failures
    (5 x 2 s + 15 x 5 s) its switch-on is picked up within up to 30 s, not at once (the price of not
    building a Vulkan instance every 5 s all night; an X RandR change event is the cure if it bites).
- **A display torn down beside a live one is PARKED, never released (review finding):**
  `drm_output_vk_destroy_display` asks the pure `drm_output_vk_park_choice` (lift-tested in
  `tests/drm_output_vk_park_1424.rs`): while another display is open the acquisition stays on the shared
  device (`display '<c>' kept acquired`), a re-open of the same output takes it back after the RandR
  lookup and before any new acquire (`display re-used`), and the device releases every parked display
  before it destroys the instance (never on a wedged device). So a retried open that acquired and then
  failed a later step (the surface while the monitor wakes, an empty mode list), and a dead present loop
  the supervisor released, both come back without an OBS restart. The reuse matches the VkDisplayKHR
  handle; an entry of the SAME connector with another handle (a replug or another screen may hand out a
  new one) is released by name and freed (`has a new handle after a replug`), a new acquire follows.
  **UNVERIFIED (supervisor, runbook step 6):** that NVIDIA keeps the handle across a replug and a
  different screen -- if the log shows `has a new handle`, the reuse never applies there and that
  re-open meets the released-display -13 until the other output closes.
- **The present loop** (`drm_output_vk_rebuild_or_give_up`) keeps a display ACQUIRED through an
  out-of-date / lost presentation (a monitor unplugged or switched off): 1 s for 10 tries, then every 5 s
  for as long as another display of the process is open, logging the 10 try lines, ONE `still out of date
  ... retrying every 5 s` line and `is back after N tries`. Alone it gives up after the 10 fast tries (a
  release tears the device down; the supervisor reopens it on a fresh instance at any mode size).
  WHY keep it acquired: NVIDIA refuses to re-acquire a display this process released while the shared
  VkInstance lives on for the other output (the 1424 STEP 0 finding in `obs-drm-output.md`).
  While it waits in that loop the display's `rebuilding` flag is up: an open of the other output does not
  wait for it to settle (it presents nothing -- the 2 s settle wait held the registry lock and its
  warning became the dormant line's cause), and before each attempt the loop clears the flag, THEN reads
  `dev->acquiring` and defers 5 ms (not a try) while another display acquires: the re-mode's seq-cst
  pair, which also closes the old "a replug rebuild during an open" window.
- **Logging, once per state change:** the module's ONE log entry is `drm_output_blog`
  (`obs-drm-output-logcap.h`; both internal headers `#define blog drm_output_blog`). On the supervisor's
  thread, during an attempt, it CAPTURES the lines: a failed attempt logs nothing of its own, only
  `retry: '<c>' dormant -- <cause>` when the cause (the FIRST warning of the attempt, never the generic
  `start FAILED` echo after it) changes; a successful one flushes its lines in order, then `retry: '<c>'
  LIVE again after N attempt(s)`. Also `retry: '<c>' dormant -- its start at the OBS launch failed` and
  `retry: '<c>' lost its present loop -- releasing the display ...`. The swapchain-overdue warning logs at
  5 s and then at powers of two (an outage of hours is not a line every 5 s).
- **Thread safety:** the registry lock around each look + action (the same lock the autostart and the
  stop hold); `vkd_active` reads a lock-free mirror (`active_pub`) because a retried open holds `k->lock`
  for seconds and the frontend polls it on the UI thread; a stop of a dead output takes the graphics
  context inside the registry lock exactly as `obs_drm_output_stop` always has.
- **The frontend re-attaches:** `DrmOutputView.cpp` polls `obs_drm_output_active_of` every 1 s from
  FINISHED_LOADING to EXIT and re-syncs (the built-in Multiview attached / detached, the Tools menu
  enabled) when an output's active state changes. The Program view's lazy GL bind and canvas mode
  handover run again on their own after a re-open (`vkd_start` resets `gl_ready` and the wanted mode).
- **Known limits:** the supervisor's X reads have no timeout of their own (a wedged-but-alive X server
  hangs OBS's own rendering long before it); the parking's reuse of a display whose loop DIED (not an
  unplug) beside a live one is a new presentation on a still-acquired display, the rebuild path's live
  shape, but that exact sequence is unproven on the rig until the runbook's step 6 reads `display
  re-used` followed by `program scanout LIVE`.
- **Tests:** `tests/drm_output_retry_1424.rs` (anchors + the C-vs-Rust parity over scripted supervisor
  scenarios; a scratch mutation run of the header killed 10/10 mutants),
  `tests/drm_output_retry_supervisor_1424.rs` (the supervisor's own code lifted verbatim and RUN against
  stand-in registry steps: one line per cause, the cause = the first warning, the flush on success, the
  release of a dead loop), `tests/python/test_drm_output_retry_grade_1424.py` (item 4c).
- **Count pins that bite a new path in the vk core:** `tests/drm_output_vk_deadline_1424.rs` pins exactly TWO
  `drm_output_vk_mark_disruption(d->dev);` in vk-setup.c (before the settle wait, after the acquire), so a new
  acquire path (the parked-display reuse) goes through the acquire's ONE mark, never a mark of its own; and a
  new std-only lift test (`{tag}-{pid}-{counter}` temp dir) needs its line on the
  `tests/no_hand_rolled_tempdirs_1409.rs` allowlist in the same commit, or CI's sweep fails.
- **verify item 4c:** `strih_drm_vk_present_dead` keeps an output dead through the supervisor's release
  stop after `lost its present loop`, until `LIVE again`; the per-output grader reports a dormant output
  as `dark -- ... retried by OBS itself` with its last retry line.

## Deploy (supervisor)

1. CI: the vendored libobs + frontend change needs `linux-genlock.yml` (both the imag-parity full build
   and `linux-genlock-build-strih`): the FULL Linux strih bundle (`obs-genlock-linux-x86_64-strih`), never
   a single-file swap (libobs + frontend + plugins built together).
2. strih-pp and strih-snv: the genlock deploy arm (`deploy-genlock-fleet.sh --run-id <id> --boxes
   strih-pp` / `strih-snv`) runs setup-strih, so step 6 writes the X config and step 16d installs the
   watch + the new panel unit (try-restarts a running panel onto the guard). The deploy's start step
   starts the watch with the other session apps. The X config needs one X restart (a reboot) to apply.
3. stream-pp / stream-snv: setup-strih step 16d (the watch, the panel guard); no X config (no fixed output).

## Live acceptance runbook (supervisor, strih-pp; strih-snv the same with its one HDMI-0)

Read the state after each step: `xrandr --current`, `journalctl --user -u strih-display-watch -n 5`,
`grep 'drm-output' <newest OBS log> | tail`, `systemctl --user status obs-box-panel`, verify-strih.

1. After the deploy + one reboot WITH the venue on: the Xorg log shows `Option "MetaModes" "DP-0:
   nvidia-auto-select, ..."` and `Validated MetaModes` without DFP-0/HDMI; both outputs LIVE; the
   `(display-x-config)` row OK.
2. Boot with EVERY monitor off: X up; the watch logs `screen 1920x1080`; the panel unit waits (one
   `waiting for a monitor` line), no tint2 core dumps; OBS up, both outputs `retry: ... dormant -- ...`
   (one line each).
3. BenQ on: within ~2 s (up to 30 s when it was off for more than ~85 s while X still read it
   connected) the HDMI-0 retry acquires (`retry: 'HDMI-0' LIVE again`, `multiview bind LIVE`
   once the frontend re-attaches) -- no OBS restart. If it reads `dormant -- ... -13` until the NEC
   comes on, record it on the ticket: the driver refuses an acquire while X has no other display.
4. Blackmagic on: `retry: 'HDMI-1' LIVE again`, the Program on the vestibule, the hub's HDMI leg
   re-resolves its ELD by itself.
5. NEC on: the watch logs `applied: xrandr --output DP-? --auto --primary`; tint2 starts; OBS + the
   panel on the NEC.
6. Mid-run, switch each monitor off and on (and unplug one, and plug a DIFFERENT screen into HDMI-0):
   the HDMI outputs log the rebuild tries and `is back after N tries` (or a release + `kept acquired` +
   `display re-used` + `LIVE again`; a `has a new handle after a replug` line = the handle-stability
   premise failed, record it on the ticket), never an OBS restart; the NEC stays in the
   layout. verify-strih passes item 4c, `(display-watch)`, `(display-watch-pass)`, `(display-x-config)`.
