---
paths:
  - "scripts/lib/strih-pipewire-purge.sh"
  - "scripts/strih-pipewire/**"
  - "scripts/lib/drm-output-audio.sh"
  - "tests/python/test_strih_pipewire_purge_1440.py"
---

# PipeWire out of the strih role, by a derived condition (issue 1440)

Phase 4 of the issue-1437 plan. The owner blacklisted PipeWire on the strih role (ROZHODNUTÉ 22.10.2026,
issue 1440 comment 6337295922). Design 6379965724, Approach 1: purge it on a strih box whose facts say
nothing needs it. strih-snv first; strih-pp follows by the same condition the day its console sends the
program as VBAN. The stream role keeps its LOCKED operator PipeWire (issue 1465,
`.claude/rules/stream-operator-audio.md`), and nothing here touches it.

## The condition (`strih_pipewire_purge_decision ROLE STREAMS TOML`)

Purge = strih role AND `STRIH_PROGRAM_VBAN_STREAMS != none` AND no `adapter = "pipewire"` participant in
the box's hub routing (`STRIH_INTERCOM_CONFIG`, read with `tomllib`, so a commented-out rollback line
never counts). No new fact, no box name.

| Box | Verdict | Why |
|---|---|---|
| strih-snv | purge | program = obs-vban `strih-obs` (issue 1439 phase 2), cutters = the hub's `alsa` leg (issue 1438), no pipewire participant |
| strih-pp | keep | `STRIH_PROGRAM_VBAN_STREAMS=none`: its program goes hub `program_out` (adapter pipewire) -> pw-cat -> PipeWire -> OBS |
| stream-pp / stream-snv | keep | the stream role's locked operator PipeWire |

An unreadable TOML, an unknown role or an unread fact = `unknown` (rc 2): setup keeps the old strih
branch with a WARNING, verify FAILs `(pipewire-purge) cannot decide`. Never a purge.

## setup-strih step 12 on a purge box (`strih_pipewire_purge_install REPO USER_HOME DESKTOP_USER`)

The order, each step idempotent:

1. **The pin** `/etc/apt/preferences.d/camera-box-no-pipewire.pref` (`obs_box_write_if_changed`, 0644
   root): every package of `STRIH_PW_PURGE_SERVER_PACKAGES` (pipewire, -bin, -pulse, -alsa, -audio, -jack,
   wireplumber, pulseaudio, gstreamer1.0-pipewire) as `name:<arch>` for EVERY architecture dpkg knows
   (native + `--print-foreign-architectures`), `Pin: release *`, `Pin-Priority: -1`.
   - **A bare package name in apt preferences pins the NATIVE architecture only** (read live on strih-snv
     24.10.2026, `apt-get -s` with a temporary `-o Dir::Etc::PreferencesParts`): pinning bare `pulseaudio`
     + `gstreamer1.0-pipewire` still installed `pulseaudio:i386` + 132 i386 packages; pinning those two
     for both arches still pulled `pipewire-pulse:i386` + 70. Hence every server package, every arch.
2. **The shim** `camera-box-pipewire-shim` (`Provides: gstreamer1.0-pipewire`, Architecture all, no
   Depends): RustDesk 1.4.9 `Depends:` ends with an unversioned `gstreamer1.0-pipewire`. Built ON the box
   from the checked-in `scripts/strih-pipewire/camera-box-pipewire-shim.control` with `dpkg-deb --build
   --root-owner-group` + `dpkg -i`, only when not installed at the control's Version. **Not equivs:**
   `apt-get -s install equivs` on strih-snv pulls 22 build packages (autoconf, automake, libtool,
   debhelper, gettext, ...) onto a production box; dpkg-deb is already there.
3. **The kept libraries** `libpulse0` + `libpipewire-0.3-0t64` marked manual (OBS links them; OBS is not
   a dpkg package, so apt does not know).
4. **The read** `LC_ALL=C apt-get -s purge <installed server packages>` through
   `strih_pipewire_purge_sim_verdict`: REFUSE on any `E:` line, any `Inst`/`Conf` line, a removal outside
   `SERVER + STRIH_PW_PURGE_COLLATERAL` (gnome-control-center, gnome-initial-setup, gnome-settings-daemon,
   gnome-snapshot, libcanberra-pulse: the 22.10 inventory, nothing runs them), or no removal at all. A
   refusal purges nothing and removes no drop-in (the pin and the shim stay: harmless).
5. **The purge** `apt-get purge -y`, then a read-back: no server package left, the kept libraries
   exactly as before.
6. **The strih-role drop-ins** removed: `STREAM_OPERATOR_AUDIO_LEFTOVERS` (stream-operator-audio.sh, the
   one list of the five files: strih-program.conf, strih-program-loopback.conf, 51-strih-quantum-1024.conf,
   51-strih-minifuse.conf, 51-minifuse-output-period.conf).

It NEVER stops or kills the operator's running PipeWire: the processes keep running from the deleted
binaries until the session restarts. A NOTE names them; the reboot is an owner-approved host action.

**Step 13** renders the hub drop-in by the same condition (`strih_intercom_audio_dropin USER UID
"$(strih_pipewire_purge_dropin_mode)"`): `no-pipewire` = `User=`/`Group=` the operator,
`SupplementaryGroups=audio render`, `ProtectHome=read-only` (the HDMI leg's xrandr reads
`~/.Xauthority`), no `pipewire` group (it does not exist on a fresh box), no `XDG_RUNTIME_DIR` /
`PIPEWIRE_RUNTIME_DIR`, no `After=user@<uid>.service`. The default render is byte-identical. Step 13
never restarts a running hub: restart it after the run.

## The browser sources (every strih box, purged or not)

A browser source with `reroute_audio` off plays its page sound to the default device: the cutters' cans
on strih-snv, the program sink on strih-pp. The strih seed manifest carries `"browser_audio":
"reroute-mute"`; `strih_scenes.py --bootstrap` runs `program_audio_vban.mute_browser_sources` (set
`reroute_audio` + mute where either is missing, read both back, status key `browser audio`). The strih
collection-port presets (`strih-pp`, `strih-snv`) set the same offline (`_browser_audio_reroute_mute`).
Changing `reroute_audio` makes obs-browser recreate the page; the seed runs at OBS launch only. Rerouted
+ muted, no page audio reaches the program either (it never did: with reroute off it went to a device).
Live strih-snv 24.10.2026 (read-only, `strih_scenes.py --browser-audio --host 10.77.9.202`): Browser
Ableset, Browser camera crew, CG-presenter, Odpocet all `reroute_audio off; not muted`.

## verify-strih item 9b (`strih_pipewire_purge_grade_report`, before the role branch)

On a purge box 6 rows, each FAIL by name: `(pipewire-packages)` (no server package, the pin = the render
for the box's archs, the shim installed, the kept libraries there), `(pipewire-process)` (no
pipewire / pipewire-pulse / wireplumber process for the operator: FAILs until the reboot),
`(pipewire-rustdesk)` (active; NOTE when not installed), `(pipewire-cards)` (each hub `alsa`
participant's used side, `/proc/asound/<card>/pcm<dev><p|c>/sub0/status` `owner_pid` -> `comm` = aplay /
arecord; closed = the card leg is not running), `(pipewire-dropins)` (the five gone, the hub drop-in =
the no-pipewire render), `(browser-audio)` (`strih_scenes.py --browser-audio`, a NOTE while OBS does not
answer). strih-pp: ONE `(pipewire-purge)` NOTE naming why + the browser row; the stream role: the NOTE
only. The rows end in `END|<n>`; the report FAILs a shorter list. On a purge box the old MiniFuse /
quantum rows are skipped (their files are gone by design).

## The fixed HDMI output's audio check (drm-output-audio.sh)

After the purge `wireplumber --version` answers nothing, which used to read as `wp-unknown` (FAIL) in
verify item 4c and a WARNING in setup step 6. `drm_output_audio_no_sound_server` (neither `wireplumber`
nor `pipewire` is a command; seam `DRM_OUTPUT_AUDIO_SERVER_BINS`): setup writes no rule, verify grades
`rule=none nodes=0` and the leg on its own OBS log line. A box with PipeWire (imag, strih-pp) is
unchanged.

## Tests (Tier-0)

`tests/python/test_strih_pipewire_purge_1440.py`: the condition over the four real fact files + TOMLs,
the pin, the read verdict over the live strih-snv `apt-get -s` text, the install against fake `apt-get`
/ `apt-mark` / `dpkg` / `dpkg-query` / `dpkg-deb` / `pgrep` / `systemctl` on PATH (order, refusal, the
read-back, idempotent re-run), the drop-in renders, every verify row broken by name, the browser seed /
report / port, the wiring and the line budgets. Seams: `STRIH_PW_PURGE_PREF`, `_OWNER`, `_TOML`,
`_ASOUND`, `_PROC`, `_SCENES`, `_HUB_DROPIN`. The 1361 golden carries the manifest's `browser_audio` line.

## Live rollout (supervisor; strih-snv; read-only first)

1. Read, as root on strih-snv: `apt-cache policy` / `dpkg-query -W` of the server packages; `LC_ALL=C
   apt-get -s purge pipewire pipewire-bin pipewire-pulse pipewire-alsa pipewire-audio wireplumber
   gstreamer1.0-pipewire` (expect the 13 of the inventory incl. rustdesk BEFORE the shim); `pactl info`
   as the operator (who still talks to the pulse server); `strih_scenes.py --pulse-capture-inputs`
   (expect `none`: 24.10.2026 read none).
2. Deploy the branch (the strih-snv genlock deploy runs setup-strih with OBS stopped), or run step 12 +
   13 alone with the "A few steps on a live box" driver of `strih-linux-provisioning.md`. Read the step-12
   log: the pin written, the shim built, `apt-get -s purge ...: 12 package(s) to remove, all in the
   declared list`, `purged ...; kept libpulse0:amd64 libpipewire-0.3-0t64:amd64`, the five drop-ins
   removed, the NOTE about the still-running processes.
3. `sudo systemctl restart intercom-hub` (the new drop-in), then start OBS (the seed mutes the browser
   sources: its `browser audio=4 browser source(s) ...` status).
4. **A host reboot clears the old PipeWire processes: owner approval first** (never reboot unasked).
5. `sudo ./scripts/verify-strih.sh --box strih-snv`: item 9b's six rows PASS, item 9 (program-vban) and
   the card-leg rows PASS, item 4c (if the HDMI output is armed) PASS with `no sound server on the box`.
6. By ear: the cutters' talkback to the cameramen and the program + cameramen in the MiniFuse cans;
   RustDesk connects (`rustdesk --get-id`).

Rollback: delete the pin file, `apt-get install pipewire pipewire-pulse pipewire-alsa wireplumber
pipewire-audio gstreamer1.0-pipewire` (the shim then conflicts nowhere; `apt-get remove
camera-box-pipewire-shim` if wanted), re-run setup-strih step 12 + 13 with the old code, reboot.
