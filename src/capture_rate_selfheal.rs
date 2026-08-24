//! #663 — capture-delivery-rate SELF-HEAL (automatic USB reset), building on #656's detection.
//!
//! #656 shipped DETECT only: `capture_rate_health` WARNs once a camera's captured fps has
//! sustained a >1% deviation from its negotiated rate for `CAPTURE_RATE_WARN_WINDOWS` (6)
//! consecutive 5s report windows. That WARN is loud in the journal, but recovering from it still
//! required an agent/human to notice, SSH in, and manually run the USB reset (unbind → `authorized`
//! 0→1 re-enumeration → restart camera-box, documented in #656's own fix comment). #663's live
//! finding: that manual fix is only TEMPORARY — cam1's ShadowCast 2 recurred the SAME defect
//! within hours, three times in one day. This module is the automatic ACT half: given a confirmed
//! (`should_warn`-level) deviant capture rate, decide whether to perform an automatic USB reset
//! RIGHT NOW, rate-limited so a genuinely dying grabber can't reset-loop forever, and escalating to
//! a CRITICAL log line once the same box keeps re-failing despite repeated resets.
//!
//! ## #685 — per-model tolerance recalibration (the CRITICAL line no longer means "replace it")
//!
//! Fleet-wide forensics (2026-07-11) found the ORIGINAL wording above ("replace the USB
//! cable/port/device") was wrong for the common case: ALL 3 deployed ShadowCast 2 units (CAM1-3)
//! show the same characteristic rate wobble that triggered this escalation, while 0/3
//! other-model grabbers do — a MODEL trait (its USB output clock free-runs even against its own
//! HDMI input), not a per-unit defect, and there are no spare units to swap in anyway. The fix has
//! two parts: (1) `capture_rate_health::tolerance_pct_for_model` now gives ShadowCast 2 a wider,
//! evidence-based deviation floor so its normal wobble never reaches `should_warn` — and therefore
//! never reaches this module at all — while every other model keeps the original strict 1% floor;
//! (2) `critical_escalation_message` (below) is reworded so that IF this escalation still fires
//! (a genuine deviation BEYOND even the widened floor), it reads as "investigate", not "hardware
//! is dying".
//!
//! Split the same way `obs_self_heal.rs` splits from `obs_watchdog.rs` (#411/#391): the DETECT
//! decision stays in `capture_rate_health` unchanged; this module owns WHEN to act (throttle +
//! recurrence-escalation, pure, unit-tested) and the actual reset I/O (sysfs `authorized` toggle).
//!
//! ## Why an in-process reset (no external script, no `systemctl stop/start`)
//!
//! `systemd/camera-box.service` has no `User=` (runs as **root**) and already grants
//! `ReadWritePaths=/dev /sys /run /proc/irq` on top of its `ProtectSystem=strict` +
//! `ReadOnlyPaths=/` sandbox (added for the #289 IRQ-affinity `ExecStartPre`). So the running
//! binary can already write `/sys/bus/usb/devices/*/authorized` and `/sys/bus/usb/drivers/
//! uvcvideo/unbind` directly — no helper script, no privilege escalation, and (per `.claude/skills/
//! ops`'s #295 note) the live cam-box fleet boots a plain RW root anyway, so there is no ro-root
//! overlay to fight either way. After toggling `authorized`, this module deliberately EXITS the
//! process (`SELF_HEAL_EXIT_CODE`) rather than trying to reopen the V4L2 device in place — the unit's
//! `Restart=always` / `RestartSec=3` already gives a clean, well-tested "come back up fresh" path
//! that re-runs `Config::device_path()`'s auto-detect against whatever device node the kernel just
//! re-enumerated. This exactly mirrors the manually-verified #656 fix sequence (`systemctl stop` →
//! unbind → `authorized` 0→1 → `systemctl start`), just triggered automatically and without the
//! explicit stop/start (the process exit + `Restart=always` IS the stop+start).
//!
//! ## Rate-limit + escalation state MUST survive the restart
//!
//! Because the fix path deliberately restarts the whole process, in-memory state cannot carry the
//! "when did we last heal" / "how many times has this recurred" bookkeeping across attempts — it
//! has to be a small file. `/run/camera-box/` (tmpfs, already `ReadWritePaths`-granted) holds it;
//! being tmpfs, a reboot clears it, which is correct (a fresh boot deserves a fresh attempt count).

use std::path::Path;

use crate::capture_rate_health::GrabberModel;

/// Default tolerance/threshold reused: this module is invoked only once `capture_rate_health::
/// should_warn` is already true (a real, sustained-for-30s defect) — see `src/main.rs`'s capture
/// loop call site. No separate confirm-threshold is needed here.
///
/// Minimum seconds between two USB-reset ATTEMPTS on the same box. A grabber that is genuinely
/// dying (failing hardware, not a transient re-negotiation) would otherwise get reset every ~30s
/// forever (`should_warn` keeps firing on every window while the defect persists) — 600s (10 min)
/// bounds that to a sane cadence while still recovering well within a typical rig session.
pub const DEFAULT_MIN_HEAL_INTERVAL_S: u64 = 600;

/// If a heal is followed by ANOTHER confirmed-deviant trigger within this many seconds of the
/// PREVIOUS heal, the previous fix apparently did not hold — count it toward escalation. Past this
/// window, a new trigger is treated as a fresh occurrence (heal count resets to 1) rather than a
/// continuation of the same failing streak. 3600s (1h) comfortably separates "the fix didn't hold"
/// from "an unrelated recurrence days later".
pub const DEFAULT_RECURRENCE_WINDOW_S: u64 = 3600;

/// Number of heals within the recurrence window that means "resets keep not holding — this reads
/// as failing hardware, not a re-negotiation glitch". Chosen to match #663's OWN live incident (3
/// recurrences the same day) — the exact scenario this module exists to escalate on.
pub const DEFAULT_CRITICAL_ESCALATION_HEALS: u32 = 3;

/// Default persisted-state path. tmpfs (`/run`), already granted `ReadWritePaths` by the systemd
/// unit — cleared on reboot, which is correct (a fresh boot deserves a fresh attempt count).
pub const STATE_PATH: &str = "/run/camera-box/capture-rate-selfheal.state";

/// Process exit code used when a self-heal USB reset SUCCEEDED and this module triggers a
/// restart-for-self-heal, distinguishing it in `journalctl`/`systemctl status` from a genuine
/// crash. Any nonzero value works with `Restart=always`; this one is just a recognizable marker.
pub const SELF_HEAL_EXIT_CODE: i32 = 77;

/// Process exit code used when the USB reset attempt itself FAILED (review finding, #663): the
/// caller still exits (after the same graceful cleanup) so `systemctl status` visibly reflects a
/// real failure and a fresh process gets a clean shot at it next rate-limit window, rather than
/// camera-box quietly limping along broken. Distinct from `SELF_HEAL_EXIT_CODE` so the journal
/// tells the two outcomes apart at a glance.
pub const SELF_HEAL_RESET_FAILED_EXIT_CODE: i32 = 78;

/// Persisted state carried across the process restart a heal triggers (mirrors
/// `obs_self_heal::SelfHealState`'s cross-pass persistence, but cross-PROCESS here since the fix
/// path itself is "exit and let systemd restart").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SelfHealState {
    /// Epoch-seconds of the last USB-reset ATTEMPT (`None` = never attempted this boot).
    pub last_heal_epoch_s: Option<u64>,
    /// How many heals have occurred in a row without a gap longer than the recurrence window —
    /// i.e. how many times in a row the fix has NOT held. Reset to 1 (not 0) on a fresh trigger
    /// after a long healthy gap.
    pub recurrence_heal_count: u32,
}

/// What the capture loop should do this report window, given a confirmed sustained deviation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelfHealDecision {
    /// Not (yet) confirmed deviant this window — no action.
    Healthy,
    /// Confirmed deviant, but the last heal attempt was too recent — wait `seconds_remaining` more.
    Throttled { seconds_remaining: u64 },
    /// Confirmed deviant, rate limit cleared — ACT. `attempt_number` is how many heals in a row
    /// (within the recurrence window) this is; `escalate_critical` is set once that count reaches
    /// `DEFAULT_CRITICAL_ESCALATION_HEALS` (or the caller's override).
    Heal {
        attempt_number: u32,
        escalate_critical: bool,
    },
}

/// Decide this window's self-heal action AND the next persisted state.
///
/// `confirmed_deviant` MUST be `capture_rate_health::should_warn(...)`'s result THIS window — this
/// function never re-derives or second-guesses it, it only decides WHEN to act on an already-
/// confirmed sustained defect (mirrors `obs_self_heal::decide`'s division of labor).
pub fn decide_selfheal(
    prev: SelfHealState,
    confirmed_deviant: bool,
    now_epoch_s: u64,
    min_interval_s: u64,
    recurrence_window_s: u64,
    critical_escalation_heals: u32,
) -> (SelfHealDecision, SelfHealState) {
    if !confirmed_deviant {
        return (SelfHealDecision::Healthy, prev);
    }

    let effective_threshold = critical_escalation_heals.max(1);

    let Some(last) = prev.last_heal_epoch_s else {
        // Never healed before (this boot) — act immediately, count starts at 1.
        let heal_count = 1;
        return (
            SelfHealDecision::Heal {
                attempt_number: heal_count,
                escalate_critical: heal_count >= effective_threshold,
            },
            SelfHealState {
                last_heal_epoch_s: Some(now_epoch_s),
                recurrence_heal_count: heal_count,
            },
        );
    };

    let elapsed = now_epoch_s.saturating_sub(last);
    if elapsed < min_interval_s {
        return (
            SelfHealDecision::Throttled {
                seconds_remaining: min_interval_s - elapsed,
            },
            prev,
        );
    }

    let recurred_within_window = elapsed <= recurrence_window_s;
    let heal_count = if recurred_within_window {
        prev.recurrence_heal_count.saturating_add(1)
    } else {
        1
    };
    (
        SelfHealDecision::Heal {
            attempt_number: heal_count,
            escalate_critical: heal_count >= effective_threshold,
        },
        SelfHealState {
            last_heal_epoch_s: Some(now_epoch_s),
            recurrence_heal_count: heal_count,
        },
    )
}

/// The CRITICAL, human-actionable escalation line (#663 ask: "the honest signal for the physical
/// fix the owner may ultimately need"; #685 reword: no longer a hardware-replacement diagnosis).
/// Pure string formatting so it's directly unit-testable.
///
/// Deliberately says "reset ATTEMPTS", not "self-healed" (review finding, #663, unchanged) —
/// `heal_count` counts every trigger within the recurrence window regardless of whether the
/// individual USB reset itself later succeeded or failed (rate-limit state is saved before the
/// attempt runs), so claiming each one was a successful "heal" would overstate what's actually
/// known.
///
/// #685: does NOT say "replace the hardware" / "FAILING HARDWARE" and does NOT recommend a
/// cable/port/dongle swap — the fleet-wide forensics withdrew that advice (ShadowCast 2's
/// characteristic wobble is a MODEL trait, not a per-unit defect, and no spares exist). By the
/// time this fires at all, the caller has ALREADY compared against `model`'s OWN (possibly
/// widened) `capture_rate_health::tolerance_pct_for_model` floor — so a genuine trigger here is,
/// by construction, beyond that model's normal characteristic envelope, and the honest framing is
/// "investigate", not a hardware diagnosis this module has no way to actually confirm.
pub fn critical_escalation_message(
    video_device_path: &str,
    heal_count: u32,
    model: GrabberModel,
) -> String {
    format!(
        "CRITICAL #663/#685: capture device {video_device_path} ({model}) has had {heal_count} \
         USB-reset attempts within {}s without the defect staying fixed — this is persistent \
         BEYOND the {model}'s normal characteristic envelope; investigate. This is NOT the \
         model's normal quantization wobble (that is already tolerated and never reaches this \
         escalation), and this module cannot confirm the root cause is hardware — see #685 \
         before assuming a physical cause.",
        DEFAULT_RECURRENCE_WINDOW_S
    )
}

/// Parse the persisted state file's `key=value` lines (mirrors the bash state-file convention
/// `scripts/lib/rig-restore-decision.sh` already uses, ported to Rust). Missing/malformed lines
/// fall back to their default — a corrupt or half-written file must never panic or block a heal.
pub fn parse_state(contents: &str) -> SelfHealState {
    let mut last_heal_epoch_s = None;
    let mut recurrence_heal_count = 0u32;
    for line in contents.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "last_heal_epoch_s" => {
                if let Ok(v) = value.trim().parse::<u64>() {
                    last_heal_epoch_s = Some(v);
                }
            }
            "recurrence_heal_count" => {
                if let Ok(v) = value.trim().parse::<u32>() {
                    recurrence_heal_count = v;
                }
            }
            _ => {}
        }
    }
    SelfHealState {
        last_heal_epoch_s,
        recurrence_heal_count,
    }
}

/// Format state back to the same `key=value` shape `parse_state` reads.
pub fn format_state(state: &SelfHealState) -> String {
    format!(
        "last_heal_epoch_s={}\nrecurrence_heal_count={}\n",
        state
            .last_heal_epoch_s
            .map(|v| v.to_string())
            .unwrap_or_default(),
        state.recurrence_heal_count
    )
}

/// Load persisted state from `path`. A missing file (expected on first boot / after a reboot
/// clears tmpfs) is silently `SelfHealState::default()` — NOT an error. A file that exists but
/// fails to read/parse into anything meaningful logs a WARN (comprehensive-logging: don't go
/// silent on a genuinely unexpected condition) and falls back to default rather than blocking a
/// heal decision on a corrupt state file.
pub fn load_state(path: &Path) -> SelfHealState {
    match std::fs::read_to_string(path) {
        Ok(contents) => parse_state(&contents),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => SelfHealState::default(),
        Err(e) => {
            tracing::warn!(
                "#663 self-heal: failed to read state file {} ({}) — treating as never-healed",
                path.display(),
                e
            );
            SelfHealState::default()
        }
    }
}

/// Persist state to `path`, creating its parent directory if needed (tmpfs — may not exist yet
/// this boot).
pub fn save_state(path: &Path, state: &SelfHealState) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, format_state(state))
}

/// Pure: derive the uvcvideo driver-unbind identifier (the USB INTERFACE directory's own basename,
/// e.g. `"2-3:1.0"`) from the interface's canonicalized sysfs path (the target of `/sys/class/
/// video4linux/videoN/device`, live-confirmed on cam1: `/sys/devices/pci0000:00/0000:00:14.0/
/// usb2/2-3/2-3:1.0`).
pub fn interface_busid_from_syspath(interface_syspath: &str) -> Option<String> {
    Path::new(interface_syspath)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
}

/// Pure: derive the parent USB DEVICE directory's `authorized` attribute path from the interface's
/// canonicalized sysfs path. Sysfs always nests a USB interface directory DIRECTLY under its
/// device directory regardless of hub depth (live-confirmed: `.../usb2/2-3/2-3:1.0`'s `authorized`
/// lives at `.../usb2/2-3/authorized`, one level up) — so the immediate parent is always correct,
/// for a directly-attached device or one behind any number of nested hubs.
pub fn authorized_path_from_interface_syspath(interface_syspath: &str) -> Option<String> {
    let parent = Path::new(interface_syspath).parent()?;
    Some(parent.join("authorized").to_string_lossy().into_owned())
}

/// Pure: `true` when `busid` has the USB-INTERFACE-level shape (`"<bus>-<port>:<config>.<iface>"`,
/// e.g. `"2-3:1.0"` — contains a `:`), as opposed to a bare USB-DEVICE-level name (`"2-3"`, no
/// `:`). Every uvcvideo-backed V4L2 device registers its `device` symlink at the interface level
/// (this appliance's only capture class), so `authorized_path_from_interface_syspath`'s
/// "immediate parent" derivation is correct ONLY when this holds — `perform_usb_reset` refuses to
/// proceed when it doesn't, rather than silently toggling a possibly-wrong parent sysfs node
/// (e.g. a shared USB hub).
pub fn is_interface_level_busid(busid: &str) -> bool {
    busid.contains(':')
}

/// #971 — RESCOPED AGAIN from #909 (doc below, kept for context/history — the #909 rationale
/// about the RESET itself being harmful mid-measured-window still stands and is why this isn't
/// simply reverted to #717's original two-band OR). Live-rig evidence (2026-08-04, cam2
/// ShadowCast 2) showed #909's premise — "the genlock decimation gate absorbs any capture
/// over-rate into exact NDI output by design, so the sustained band never needs to reset" — is
/// true for RATE but NOT for MOTION CADENCE: a 63.75-64.0fps capture against a 60Hz HDMI source
/// manufactures ~3.75 duplicate+skip pairs/s that decimation cannot cleanly undo. A 60s recording
/// during the live incident measured 16.5% irregular optical-tick steps; a manual USB reset (the
/// issue-656 sequence) dropped that to 0.6% by restoring 59.9-60.0fps. The defect also recurred
/// across repeated device re-opens (2 of 4 in one day) — without automatic self-heal this needs a
/// human to notice and manually re-run the reset every time.
///
/// The resolution: the JITTER band still arms immediately, unchanged (`capture_rate_health::
/// tolerance_pct_for_model` + `CAPTURE_RATE_WARN_WINDOWS`, 30s — a real device fault genuinely
/// beyond #685's widened per-model tolerance). The SUSTAINED band now ALSO arms, but ONLY once
/// genuinely CHRONIC — confirmed for `capture_rate_health::CHRONIC_SUSTAINED_WARN_WINDOWS` (15
/// min @ 5s/window, 15x the 60s bar the informational-only log at the `src/main.rs` call site
/// still uses) consecutive report windows. #909's own finding — a reset firing mid-measured-
/// window corrupted an E2E run, misclassified as `frozen_leg` — is exactly why this is a much
/// longer bar than the 60s informational one, not a reversion to firing on it directly.
/// `next_consecutive_breaches`'s existing "any healthy window resets the streak to 0" behavior
/// (`capture_rate_health.rs`, unchanged) IS the hysteresis this needs: a borderline/flapping
/// device can never accumulate to the 180-window chronic bar, so it can never reset-loop on this
/// band alone. `decide_selfheal`'s existing throttle (`DEFAULT_MIN_HEAL_INTERVAL_S`, 10 min) and
/// recurrence-escalation (`DEFAULT_RECURRENCE_WINDOW_S`/`DEFAULT_CRITICAL_ESCALATION_HEALS`) are
/// reused UNCHANGED for this band too — no parallel throttle/escalation mechanism.
///
/// The caller (`src/main.rs`) computes `sustained_chronic` from a THIRD consecutive-breach
/// counter fed by the SAME `sustained_deviant` flag the 60s-confirm `sustained_confirmed` already
/// uses, just checked against the much longer `CHRONIC_SUSTAINED_WARN_WINDOWS` bar instead — the
/// exact same mechanism the existing jitter/sustained split already uses (only tolerance + window
/// count differ), never a parallel mechanism. This function stays a pure OR of two
/// already-confirmed booleans, same shape as before this rescope.
///
/// ---- #909 (superseded by #971 above; kept for context/history) ----
/// RESCOPED from #717's original two-band OR (history below, kept for context). The
/// user's architectural ruling on #909: a grabber's own crystal/timer free-running against its
/// HDMI input is EXPECTED, and `src/main.rs`'s genlock decimation gate (emit the first capture
/// at/after each DanteSync wall-clock boundary, drop the rest — `CAMERA_BOX_GENLOCK_FPS`) already
/// absorbs any capture over-rate into exact NDI output by design. cam1's own live incident proved
/// the RESET was the actual harm, not the over-rate: a USB reset firing mid-measured-window
/// produced an ~8.3s stale/copy gap that the E2E gate then misclassified as `frozen_leg` (#909's
/// own evidence, 6 resets/hour, none holding). So only the JITTER band (`capture_rate_health::
/// tolerance_pct_for_model` + `CAPTURE_RATE_WARN_WINDOWS`, 30s) — a deviation genuinely BEYOND
/// even #685's widened per-model tolerance, i.e. a real device fault — still escalates to a USB
/// reset. The SUSTAINED band (`sustained_tolerance_pct_for_model` + `SUSTAINED_WARN_WINDOWS`,
/// 60s) stays fully computed and LOGGED at the call site (`src/main.rs`, informational only) —
/// never silently dropped, only decoupled from the reset action, mirroring `self_heal_
/// attribution.rs`'s "ALLOW never SUPPRESS" precedent (#895/#914).
///
/// Original #717 history: for every model except ShadowCast 2 the sustained band uses the SAME
/// tolerance as the jitter band, so their sustained arm was always a strict superset of their
/// jitter arm and could never fire earlier — #717 (and every rescope since) changes NOTHING
/// about their self-heal cadence.
pub fn should_trigger_selfheal(jitter_confirmed: bool, sustained_chronic: bool) -> bool {
    jitter_confirmed || sustained_chronic
}

/// Perform the actual USB reset on the capture device backing `video_device_path` (e.g.
/// `/dev/video1`) — mirrors the manually-verified #656 fix sequence: uvcvideo unbind (best-effort;
/// the `authorized` toggle below is what actually forces re-enumeration, so a failed/missing
/// unbind must not abort the reset) → `authorized` 0 → sleep → `authorized` 1. Does NOT restart
/// camera-box itself — the caller is expected to exit the process afterward so `Restart=always`
/// brings it back up against the freshly re-enumerated device (see module doc).
pub fn perform_usb_reset(video_device_path: &str) -> anyhow::Result<()> {
    use anyhow::Context;

    let name = Path::new(video_device_path)
        .file_name()
        .with_context(|| format!("video device path has no file name: {video_device_path}"))?
        .to_string_lossy()
        .into_owned();
    let device_link = format!("/sys/class/video4linux/{name}/device");
    let interface_syspath = std::fs::canonicalize(&device_link)
        .with_context(|| format!("resolve capture device syspath via {device_link}"))?
        .to_string_lossy()
        .into_owned();

    // Defensive check: every uvcvideo-backed V4L2 device (this appliance's ONLY capture device
    // class — the Genki ShadowCast 2 fleet, live-confirmed layout in the module doc) registers
    // its "device" symlink at the USB INTERFACE level, whose basename always has the
    // "<bus>-<port>:<config>.<iface>" shape (contains ':'). If that ever isn't true (a different
    // capture device class, or a kernel/driver change), `authorized_path_from_interface_syspath`'s
    // "immediate parent" derivation would silently walk up to the WRONG sysfs node (e.g. a shared
    // USB hub) and toggle ITS `authorized` — bail loudly instead of guessing.
    let busid = interface_busid_from_syspath(&interface_syspath)
        .with_context(|| format!("derive USB interface bus-id from {interface_syspath}"))?;
    anyhow::ensure!(
        is_interface_level_busid(&busid),
        "capture device syspath {interface_syspath} does not look like a USB INTERFACE \
         directory (basename {busid} has no ':<config>.<iface>' suffix) — refusing to guess an \
         `authorized` path a level up, which could toggle the WRONG USB device"
    );

    let authorized_path = authorized_path_from_interface_syspath(&interface_syspath)
        .with_context(|| format!("derive `authorized` path from {interface_syspath}"))?;

    tracing::warn!(
        "#663 self-heal: USB-resetting capture device {} (interface syspath {}, authorized={})",
        video_device_path,
        interface_syspath,
        authorized_path
    );

    let unbind_path = "/sys/bus/usb/drivers/uvcvideo/unbind";
    if let Err(e) = std::fs::write(unbind_path, format!("{busid}\n")) {
        tracing::warn!(
            "#663 self-heal: uvcvideo unbind of {} failed (non-fatal — the authorized \
             toggle below forces re-enumeration regardless): {}",
            busid,
            e
        );
    }

    std::fs::write(&authorized_path, "0\n")
        .with_context(|| format!("deauthorize USB device ({authorized_path})"))?;
    std::thread::sleep(std::time::Duration::from_millis(1500));

    // #663 review finding: a device successfully DEAUTHORIZED but then failing to reauthorize
    // would be left permanently disconnected — strictly WORSE than the original rate defect (no
    // capture at all, vs a wrong rate). Retry the reauthorize write a few times before giving up,
    // since transient sysfs contention during re-enumeration is far more likely than a permanent
    // failure, and the cost of one extra write attempt is negligible.
    const REAUTHORIZE_ATTEMPTS: u32 = 3;
    let mut reauthorize_result = Err(anyhow::anyhow!("reauthorize not attempted"));
    for attempt in 1..=REAUTHORIZE_ATTEMPTS {
        reauthorize_result = std::fs::write(&authorized_path, "1\n")
            .with_context(|| format!("reauthorize USB device ({authorized_path})"));
        if reauthorize_result.is_ok() {
            break;
        }
        if attempt < REAUTHORIZE_ATTEMPTS {
            tracing::warn!(
                "#663 self-heal: reauthorize attempt {}/{} failed for {} — retrying: {:?}",
                attempt,
                REAUTHORIZE_ATTEMPTS,
                authorized_path,
                reauthorize_result.as_ref().err()
            );
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }
    reauthorize_result.with_context(|| {
        format!(
            "device {authorized_path} was deauthorized and FAILED to reauthorize after {REAUTHORIZE_ATTEMPTS} \
             attempts — it is now DISCONNECTED, worse than the original rate defect; needs manual \
             `echo 1 > {authorized_path}` recovery"
        )
    })?;

    tracing::warn!(
        "#663 self-heal: USB reset complete for {} — process will exit (code {}) so systemd's \
         Restart=always picks up the re-enumerated device",
        video_device_path,
        SELF_HEAL_EXIT_CODE
    );
    Ok(())
}

/// #1149 — trigger-specific log-message pieces for the shared self-heal action sequence.
///
/// The #656/#663/#971 capture-rate trigger and the #1128 grabber-STUCK trigger run the SAME
/// destructive `load_state → decide_selfheal → save_state → perform_usb_reset → exit` sequence;
/// ONLY these three `&'static str` fragments differ between them, so [`attempt_self_heal`] is
/// parameterized by this struct (two consts below) instead of the ~80 lines being duplicated at
/// both call sites in `src/main.rs`. The fragments reproduce the pre-#1149 log wording
/// byte-for-byte (proven by the `*_message` builder tests), so every downstream dev1-watchdog
/// grep anchor (`#663 self-heal: USB reset attempt`, etc.) stays intact.
pub struct SelfHealMessages {
    /// Non-critical line prefix, e.g. `#663 self-heal` / `#1128 grabber-stuck self-heal`.
    pub tag: &'static str,
    /// CRITICAL failure-line prefix up to (but excluding) ` USB reset attempt`, e.g.
    /// `CRITICAL #663: self-heal` / `CRITICAL #1128 grabber-stuck self-heal:`.
    pub critical_prefix: &'static str,
    /// The `…WORSE state than the original {} defect…` word: `rate` / `stuck`.
    pub defect_word: &'static str,
}

/// #656/#663/#971 capture-delivery-rate self-heal wording.
pub const CAPTURE_RATE_SELF_HEAL_MESSAGES: SelfHealMessages = SelfHealMessages {
    tag: "#663 self-heal",
    critical_prefix: "CRITICAL #663: self-heal",
    defect_word: "rate",
};

/// #1128 grabber-STUCK self-heal wording.
pub const GRABBER_STUCK_SELF_HEAL_MESSAGES: SelfHealMessages = SelfHealMessages {
    tag: "#1128 grabber-stuck self-heal",
    critical_prefix: "CRITICAL #1128 grabber-stuck self-heal:",
    defect_word: "stuck",
};

/// #1193 sustained-OVER-RATE self-heal wording — the 3rd trigger (`capture_overrate`). Its `tag`
/// shares no substring with the `#663 self-heal` / `#1128 grabber-stuck self-heal` prefixes, so the
/// reset-line grep anchors of the other two triggers never mis-match this one.
pub const OVER_RATE_SELF_HEAL_MESSAGES: SelfHealMessages = SelfHealMessages {
    tag: "#1193 over-rate self-heal",
    critical_prefix: "CRITICAL #1193 over-rate self-heal:",
    defect_word: "over-rate",
};

/// `SelfHealDecision::Throttled` WARN line (rate-limited — no reset this window).
pub fn throttled_message(msgs: &SelfHealMessages, seconds_remaining: u64) -> String {
    format!(
        "{} rate-limited: the last USB reset attempt was too recent — {}s remaining before another attempt is allowed",
        msgs.tag, seconds_remaining
    )
}

/// `save_state` failure ERROR line (state persist failed — the reset still proceeds).
pub fn save_state_failed_message(msgs: &SelfHealMessages, state_path: &str, err: &str) -> String {
    format!(
        "{}: failed to persist self-heal state to {}: {} (rate-limit/escalation count may not survive this restart)",
        msgs.tag, state_path, err
    )
}

/// Successful-USB-reset WARN line (process will exit `exit_code` after graceful shutdown).
pub fn reset_success_message(
    msgs: &SelfHealMessages,
    attempt_number: u32,
    exit_code: i32,
) -> String {
    format!(
        "{}: USB reset attempt #{} succeeded — will exit (code {}) after graceful shutdown so systemd restarts camera-box against the re-enumerated device",
        msgs.tag, attempt_number, exit_code
    )
}

/// Failed-USB-reset CRITICAL line (device possibly disconnected — still exits `exit_code`).
pub fn reset_failed_message(
    msgs: &SelfHealMessages,
    attempt_number: u32,
    err: &str,
    exit_code: i32,
) -> String {
    format!(
        "{} USB reset attempt #{} FAILED: {} — the capture device may now be in a WORSE state than the original {} defect (possibly disconnected); exiting (code {}) after graceful shutdown so systemd retries with a fresh process",
        msgs.critical_prefix, attempt_number, err, msgs.defect_word, exit_code
    )
}

/// #1149 — the ONE shared in-process USB self-heal action sequence, invoked by BOTH the
/// #656/#663/#971 capture-rate trigger and the #1128 grabber-STUCK trigger in `src/main.rs`.
///
/// Runs the identical destructive flow — `load_state → decide_selfheal → match → save_state →
/// perform_usb_reset` — and returns the pending process-exit code the caller must apply:
/// `Some(SELF_HEAL_EXIT_CODE)` (77) on a successful reset, `Some(SELF_HEAL_RESET_FAILED_EXIT_CODE)`
/// (78) on a failed one, and `None` when the decision was `Healthy`/`Throttled` and NO reset was
/// attempted. The caller applies a returned `Some(code)` by stopping capture and setting its own
/// `pending_self_heal_exit_code` — behaviorally identical to the two pre-#1149 inline blocks.
///
/// `reset` is INJECTED (production passes [`perform_usb_reset`]) so the sequencing is unit-testable
/// without ever firing a real USB re-enumeration; `msgs` selects the trigger-specific log wording.
pub fn attempt_self_heal(
    device_path: &str,
    model: GrabberModel,
    now_epoch_s: u64,
    state_path: &Path,
    msgs: &SelfHealMessages,
    reset: impl FnOnce(&str) -> anyhow::Result<()>,
) -> Option<i32> {
    let prev_selfheal_state = load_state(state_path);
    let (selfheal_decision, next_selfheal_state) = decide_selfheal(
        prev_selfheal_state,
        true,
        now_epoch_s,
        DEFAULT_MIN_HEAL_INTERVAL_S,
        DEFAULT_RECURRENCE_WINDOW_S,
        DEFAULT_CRITICAL_ESCALATION_HEALS,
    );
    match selfheal_decision {
        // Unreachable in production (both call sites only reach this with a confirmed defect),
        // but map it to a no-op so the shared helper never resets on a Healthy decision.
        SelfHealDecision::Healthy => None,
        SelfHealDecision::Throttled { seconds_remaining } => {
            tracing::warn!("{}", throttled_message(msgs, seconds_remaining));
            None
        }
        SelfHealDecision::Heal {
            attempt_number,
            escalate_critical,
        } => {
            if escalate_critical {
                tracing::error!(
                    "{}",
                    critical_escalation_message(device_path, attempt_number, model)
                );
            }
            if let Err(e) = save_state(state_path, &next_selfheal_state) {
                tracing::error!(
                    "{}",
                    save_state_failed_message(
                        msgs,
                        &state_path.display().to_string(),
                        &e.to_string()
                    )
                );
            }
            match reset(device_path) {
                Ok(()) => {
                    tracing::warn!(
                        "{}",
                        reset_success_message(msgs, attempt_number, SELF_HEAL_EXIT_CODE)
                    );
                    Some(SELF_HEAL_EXIT_CODE)
                }
                Err(e) => {
                    tracing::error!(
                        "{}",
                        reset_failed_message(
                            msgs,
                            attempt_number,
                            &format!("{e:#}"),
                            SELF_HEAL_RESET_FAILED_EXIT_CODE
                        )
                    );
                    Some(SELF_HEAL_RESET_FAILED_EXIT_CODE)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // #717 RED marker: `should_trigger_selfheal` is NOT YET IMPLEMENTED — these tests reference
    // it already (RED commit: proves via compile failure the two-band OR combinator does not
    // exist yet). Implemented in the immediately-following GREEN commit.

    #[test]
    fn selfheal_triggers_on_jitter_band_alone() {
        assert!(should_trigger_selfheal(true, false));
    }

    // #971 RED marker (REPLACES the #909 test below, which pinned the OPPOSITE expectation):
    // this now-CORRECT expectation contradicts the CURRENT implementation
    // (`fn should_trigger_selfheal(jitter_confirmed, _sustained_confirmed) -> bool {
    // jitter_confirmed }`, which ignores its second argument entirely) — the test fails until
    // the GREEN commit changes `should_trigger_selfheal` to `jitter_confirmed ||
    // sustained_chronic`. Justification for replacing (not just adding to) the #909 test: issue
    // 971's live evidence (2026-08-04, cam2 ShadowCast 2 — 63.75-64.0fps sustained for 458
    // consecutive report windows) showed #909's premise ("the sustained band alone is absorbed
    // by the genlock decimation gate by design, so it must never escalate to a reset") is true
    // for RATE but not for MOTION CADENCE — a sustained-alone over-rate manufactures a real,
    // visible dup+skip cadence defect (measured 16.5% irregular optical-tick steps, dropping to
    // 0.6% after a manual USB reset) that the decimation gate does not cleanly absorb. The old
    // #909 test asserted the literal opposite of what issue 971 now requires, so it is replaced
    // here rather than kept alongside a contradictory new one. The second argument's MEANING
    // also changes: it is no longer the 60s-confirm `sustained_confirmed` (kept, still computed,
    // still used for the informational-only log) but a much longer `sustained_chronic` (see
    // `capture_rate_health::CHRONIC_SUSTAINED_WARN_WINDOWS`, 15 min) — this test only exercises
    // the pure boolean combinator, agnostic to which counter feeds it.
    #[test]
    fn selfheal_triggers_on_chronic_sustained_band_alone_971() {
        assert!(should_trigger_selfheal(false, true));
    }

    #[test]
    fn selfheal_triggers_when_both_bands_confirm() {
        assert!(should_trigger_selfheal(true, true));
    }

    #[test]
    fn selfheal_does_not_trigger_when_neither_band_confirms() {
        assert!(!should_trigger_selfheal(false, false));
    }

    const T0: u64 = 1_000_000;

    #[test]
    fn first_ever_deviant_triggers_heal_attempt_one() {
        let (decision, next) = decide_selfheal(
            SelfHealState::default(),
            true,
            T0,
            DEFAULT_MIN_HEAL_INTERVAL_S,
            DEFAULT_RECURRENCE_WINDOW_S,
            DEFAULT_CRITICAL_ESCALATION_HEALS,
        );
        assert_eq!(
            decision,
            SelfHealDecision::Heal {
                attempt_number: 1,
                escalate_critical: false,
            }
        );
        assert_eq!(next.last_heal_epoch_s, Some(T0));
        assert_eq!(next.recurrence_heal_count, 1);
    }

    #[test]
    fn healthy_pass_never_acts_and_state_is_unchanged() {
        let prev = SelfHealState {
            last_heal_epoch_s: Some(T0),
            recurrence_heal_count: 2,
        };
        let (decision, next) = decide_selfheal(
            prev,
            false,
            T0 + 5,
            DEFAULT_MIN_HEAL_INTERVAL_S,
            DEFAULT_RECURRENCE_WINDOW_S,
            DEFAULT_CRITICAL_ESCALATION_HEALS,
        );
        assert_eq!(decision, SelfHealDecision::Healthy);
        assert_eq!(
            next, prev,
            "a healthy pass must never mutate persisted state"
        );
    }

    #[test]
    fn too_soon_retry_is_throttled_and_state_unchanged() {
        let prev = SelfHealState {
            last_heal_epoch_s: Some(T0),
            recurrence_heal_count: 1,
        };
        let (decision, next) = decide_selfheal(
            prev,
            true,
            T0 + 30,
            DEFAULT_MIN_HEAL_INTERVAL_S, // 600
            DEFAULT_RECURRENCE_WINDOW_S,
            DEFAULT_CRITICAL_ESCALATION_HEALS,
        );
        assert_eq!(
            decision,
            SelfHealDecision::Throttled {
                seconds_remaining: 570
            }
        );
        assert_eq!(next, prev, "a throttled decision must never mutate state");
    }

    #[test]
    fn past_min_interval_within_recurrence_window_increments_heal_count() {
        let prev = SelfHealState {
            last_heal_epoch_s: Some(T0),
            recurrence_heal_count: 1,
        };
        let (decision, next) = decide_selfheal(
            prev,
            true,
            T0 + 610, // past the 600s throttle, well within the 3600s recurrence window
            DEFAULT_MIN_HEAL_INTERVAL_S,
            DEFAULT_RECURRENCE_WINDOW_S,
            DEFAULT_CRITICAL_ESCALATION_HEALS,
        );
        assert_eq!(
            decision,
            SelfHealDecision::Heal {
                attempt_number: 2,
                escalate_critical: false,
            }
        );
        assert_eq!(next.recurrence_heal_count, 2);
        assert_eq!(next.last_heal_epoch_s, Some(T0 + 610));
    }

    #[test]
    fn escalates_critical_on_the_663_live_scenario_three_recurrences_same_day() {
        // #663's own live finding: 3 recurrences in one day. Heal 1 (no escalate) -> heal 2 (no
        // escalate) -> heal 3 (ESCALATE, matches DEFAULT_CRITICAL_ESCALATION_HEALS=3).
        let (d1, s1) = decide_selfheal(
            SelfHealState::default(),
            true,
            T0,
            DEFAULT_MIN_HEAL_INTERVAL_S,
            DEFAULT_RECURRENCE_WINDOW_S,
            DEFAULT_CRITICAL_ESCALATION_HEALS,
        );
        assert_eq!(
            d1,
            SelfHealDecision::Heal {
                attempt_number: 1,
                escalate_critical: false
            }
        );

        let (d2, s2) = decide_selfheal(
            s1,
            true,
            T0 + 700,
            DEFAULT_MIN_HEAL_INTERVAL_S,
            DEFAULT_RECURRENCE_WINDOW_S,
            DEFAULT_CRITICAL_ESCALATION_HEALS,
        );
        assert_eq!(
            d2,
            SelfHealDecision::Heal {
                attempt_number: 2,
                escalate_critical: false
            }
        );

        let (d3, s3) = decide_selfheal(
            s2,
            true,
            T0 + 1400,
            DEFAULT_MIN_HEAL_INTERVAL_S,
            DEFAULT_RECURRENCE_WINDOW_S,
            DEFAULT_CRITICAL_ESCALATION_HEALS,
        );
        assert_eq!(
            d3,
            SelfHealDecision::Heal {
                attempt_number: 3,
                escalate_critical: true
            },
            "the 3rd recurrence within the recurrence window must escalate CRITICAL"
        );
        assert_eq!(s3.recurrence_heal_count, 3);
    }

    #[test]
    fn recurrence_resets_after_a_long_healthy_gap() {
        let prev = SelfHealState {
            last_heal_epoch_s: Some(T0),
            recurrence_heal_count: 2,
        };
        // Elapsed exceeds the 3600s recurrence window -> treated as a FRESH occurrence.
        let (decision, next) = decide_selfheal(
            prev,
            true,
            T0 + 4000,
            DEFAULT_MIN_HEAL_INTERVAL_S,
            DEFAULT_RECURRENCE_WINDOW_S,
            DEFAULT_CRITICAL_ESCALATION_HEALS,
        );
        assert_eq!(
            decision,
            SelfHealDecision::Heal {
                attempt_number: 1,
                escalate_critical: false,
            },
            "a recurrence past the window must reset the heal count to 1, not keep accumulating"
        );
        assert_eq!(next.recurrence_heal_count, 1);
    }

    #[test]
    fn exactly_at_recurrence_window_boundary_still_counts_as_recurred() {
        let prev = SelfHealState {
            last_heal_epoch_s: Some(T0),
            recurrence_heal_count: 1,
        };
        let (decision, _) = decide_selfheal(
            prev,
            true,
            T0 + DEFAULT_RECURRENCE_WINDOW_S,
            DEFAULT_MIN_HEAL_INTERVAL_S,
            DEFAULT_RECURRENCE_WINDOW_S,
            DEFAULT_CRITICAL_ESCALATION_HEALS,
        );
        assert_eq!(
            decision,
            SelfHealDecision::Heal {
                attempt_number: 2,
                escalate_critical: false,
            },
            "elapsed == recurrence_window_s (inclusive boundary) must still count as recurred"
        );
    }

    #[test]
    fn zero_critical_threshold_is_clamped_to_one() {
        // A misconfigured threshold=0 must not mean "escalate before even one heal" — clamp to 1
        // so the very first heal already escalates instead of silently disabling escalation.
        let (decision, _) = decide_selfheal(
            SelfHealState::default(),
            true,
            T0,
            DEFAULT_MIN_HEAL_INTERVAL_S,
            DEFAULT_RECURRENCE_WINDOW_S,
            0,
        );
        assert_eq!(
            decision,
            SelfHealDecision::Heal {
                attempt_number: 1,
                escalate_critical: true,
            }
        );
    }

    #[test]
    fn critical_escalation_message_names_the_device_model_and_heal_count() {
        let msg = critical_escalation_message("/dev/video1", 3, GrabberModel::ShadowCast2);
        assert!(msg.contains("CRITICAL"));
        assert!(msg.contains("#663"));
        assert!(msg.contains("#685"));
        assert!(msg.contains("/dev/video1"));
        assert!(msg.contains("ShadowCast 2"));
        assert!(msg.contains('3'));
    }

    #[test]
    fn critical_escalation_message_685_no_longer_recommends_hardware_replacement() {
        // #685: the fleet-wide forensics WITHDREW the "replace hardware" advice — ShadowCast 2's
        // characteristic wobble is a MODEL trait (3/3 units), not a per-unit defect, and there
        // are no spare units to swap in anyway (user instruction). This must stay reworded.
        let msg = critical_escalation_message("/dev/video1", 3, GrabberModel::ShadowCast2);
        let lower = msg.to_lowercase();
        assert!(
            !lower.contains("replace"),
            "must not recommend replacing hardware/cable/port/device: {msg}"
        );
        assert!(
            !lower.contains("warranty"),
            "must not mention warranty: {msg}"
        );
        assert!(
            !lower.contains("failing hardware"),
            "must not diagnose failing hardware — this module cannot confirm that: {msg}"
        );
        assert!(
            lower.contains("investigate"),
            "must reframe as 'investigate', not a hardware diagnosis: {msg}"
        );
    }

    #[test]
    fn critical_escalation_message_names_the_actual_model_per_box() {
        let shadowcast = critical_escalation_message("/dev/video1", 3, GrabberModel::ShadowCast2);
        let nzxt = critical_escalation_message("/dev/video0", 3, GrabberModel::NzxtSignalHd60);
        assert!(shadowcast.contains("ShadowCast 2"));
        assert!(nzxt.contains("NZXT Signal HD60"));
        assert_ne!(
            shadowcast, nzxt,
            "the message must actually vary per model, not just per device path"
        );
    }

    #[test]
    fn state_round_trips_through_parse_and_format() {
        let state = SelfHealState {
            last_heal_epoch_s: Some(1_731_000_000),
            recurrence_heal_count: 2,
        };
        let formatted = format_state(&state);
        let parsed = parse_state(&formatted);
        assert_eq!(parsed, state);
    }

    #[test]
    fn parse_state_defaults_on_missing_or_malformed_content() {
        assert_eq!(parse_state(""), SelfHealState::default());
        assert_eq!(
            parse_state("garbage\nnot_a_key_value_line\n"),
            SelfHealState::default()
        );
        assert_eq!(
            parse_state("last_heal_epoch_s=not_a_number\nrecurrence_heal_count=2\n"),
            SelfHealState {
                last_heal_epoch_s: None,
                recurrence_heal_count: 2,
            },
            "a malformed individual field must fall back to its own default, not poison the whole \
             parse"
        );
    }

    #[test]
    fn parse_state_handles_never_healed_empty_epoch_field() {
        // format_state's own output when last_heal_epoch_s is None (empty value after '=').
        assert_eq!(
            parse_state("last_heal_epoch_s=\nrecurrence_heal_count=0\n"),
            SelfHealState::default()
        );
    }

    #[test]
    fn load_state_from_missing_path_is_default_not_an_error() {
        let path = Path::new("/nonexistent/does/not/exist/capture-rate-selfheal.state");
        assert_eq!(load_state(path), SelfHealState::default());
    }

    #[test]
    fn save_state_creates_parent_dir_and_load_state_reads_it_back() {
        let dir = std::env::temp_dir().join(format!(
            "camera-box-selfheal-test-{}-{}",
            std::process::id(),
            T0
        ));
        let path = dir.join("nested/state.txt");
        let state = SelfHealState {
            last_heal_epoch_s: Some(42),
            recurrence_heal_count: 5,
        };
        save_state(&path, &state).expect("save_state must create the parent dir and write");
        let loaded = load_state(&path);
        assert_eq!(loaded, state);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Live-confirmed cam1 layout (2026-07-10, `readlink -f /sys/class/video4linux/video1/device`):
    /// `/sys/devices/pci0000:00/0000:00:14.0/usb2/2-3/2-3:1.0`, whose device-level `authorized`
    /// file lives at `/sys/devices/pci0000:00/0000:00:14.0/usb2/2-3/authorized`.
    const LIVE_CAM1_INTERFACE_SYSPATH: &str =
        "/sys/devices/pci0000:00/0000:00:14.0/usb2/2-3/2-3:1.0";

    #[test]
    fn authorized_path_matches_the_live_confirmed_cam1_layout() {
        assert_eq!(
            authorized_path_from_interface_syspath(LIVE_CAM1_INTERFACE_SYSPATH),
            Some("/sys/devices/pci0000:00/0000:00:14.0/usb2/2-3/authorized".to_string())
        );
    }

    #[test]
    fn interface_busid_matches_the_live_confirmed_cam1_layout() {
        assert_eq!(
            interface_busid_from_syspath(LIVE_CAM1_INTERFACE_SYSPATH),
            Some("2-3:1.0".to_string())
        );
    }

    #[test]
    fn authorized_path_handles_deeper_hub_nesting() {
        // A device behind two nested hubs, e.g. bus 1, port path 2.3.1 -> interface "1-2.3.1:1.0".
        // The device dir is still the immediate parent regardless of nesting depth.
        assert_eq!(
            authorized_path_from_interface_syspath(
                "/sys/devices/pci0000:00/.../usb1/1-2/1-2.3/1-2.3.1/1-2.3.1:1.0"
            ),
            Some("/sys/devices/pci0000:00/.../usb1/1-2/1-2.3/1-2.3.1/authorized".to_string())
        );
    }

    #[test]
    fn syspath_helpers_return_none_on_a_rootless_path() {
        assert_eq!(authorized_path_from_interface_syspath("/"), None);
    }

    #[test]
    fn is_interface_level_busid_accepts_the_live_confirmed_cam1_shape() {
        assert!(is_interface_level_busid("2-3:1.0"));
        assert!(
            is_interface_level_busid("1-2.3.1:1.0"),
            "nested-hub interface ids also qualify"
        );
    }

    #[test]
    fn is_interface_level_busid_rejects_a_bare_device_level_name() {
        assert!(
            !is_interface_level_busid("2-3"),
            "a device-level name (no ':<config>.<iface>' suffix) must be rejected — \
             perform_usb_reset's guard depends on this to avoid toggling the wrong sysfs node"
        );
    }

    // -----------------------------------------------------------------------------------------
    // #1149 — the unified self-heal helper: byte-exact legacy log wording + the destructive
    // sequence's exit-code mapping (with an INJECTED fake reset so no real USB re-enumeration
    // fires). The golden strings are copied verbatim from the two pre-#1149 inline blocks in
    // src/main.rs, so a wording drift on EITHER the #656/#663/#971 or the #1128 path fails here.
    // -----------------------------------------------------------------------------------------

    #[test]
    fn capture_rate_self_heal_messages_byte_match_the_legacy_656_663_971_wording() {
        let m = &CAPTURE_RATE_SELF_HEAL_MESSAGES;
        assert_eq!(
            throttled_message(m, 42),
            "#663 self-heal rate-limited: the last USB reset attempt was too recent — 42s remaining before another attempt is allowed"
        );
        assert_eq!(
            save_state_failed_message(m, "/run/camera-box/capture-rate-selfheal.state", "boom"),
            "#663 self-heal: failed to persist self-heal state to /run/camera-box/capture-rate-selfheal.state: boom (rate-limit/escalation count may not survive this restart)"
        );
        assert_eq!(
            reset_success_message(m, 3, SELF_HEAL_EXIT_CODE),
            "#663 self-heal: USB reset attempt #3 succeeded — will exit (code 77) after graceful shutdown so systemd restarts camera-box against the re-enumerated device"
        );
        assert_eq!(
            reset_failed_message(m, 3, "boom", SELF_HEAL_RESET_FAILED_EXIT_CODE),
            "CRITICAL #663: self-heal USB reset attempt #3 FAILED: boom — the capture device may now be in a WORSE state than the original rate defect (possibly disconnected); exiting (code 78) after graceful shutdown so systemd retries with a fresh process"
        );
        // The dev1-watchdog `self_heal_reset_grep_pattern` and `capture_rate_defect_grep_pattern_hard`
        // anchors both key on this exact substring — it must never drift.
        assert!(reset_success_message(m, 1, SELF_HEAL_EXIT_CODE)
            .contains("#663 self-heal: USB reset attempt"));
    }

    #[test]
    fn grabber_stuck_self_heal_messages_byte_match_the_legacy_1128_wording() {
        let m = &GRABBER_STUCK_SELF_HEAL_MESSAGES;
        assert_eq!(
            throttled_message(m, 42),
            "#1128 grabber-stuck self-heal rate-limited: the last USB reset attempt was too recent — 42s remaining before another attempt is allowed"
        );
        assert_eq!(
            save_state_failed_message(m, "/run/camera-box/capture-rate-selfheal.state", "boom"),
            "#1128 grabber-stuck self-heal: failed to persist self-heal state to /run/camera-box/capture-rate-selfheal.state: boom (rate-limit/escalation count may not survive this restart)"
        );
        assert_eq!(
            reset_success_message(m, 3, SELF_HEAL_EXIT_CODE),
            "#1128 grabber-stuck self-heal: USB reset attempt #3 succeeded — will exit (code 77) after graceful shutdown so systemd restarts camera-box against the re-enumerated device"
        );
        assert_eq!(
            reset_failed_message(m, 3, "boom", SELF_HEAL_RESET_FAILED_EXIT_CODE),
            "CRITICAL #1128 grabber-stuck self-heal: USB reset attempt #3 FAILED: boom — the capture device may now be in a WORSE state than the original stuck defect (possibly disconnected); exiting (code 78) after graceful shutdown so systemd retries with a fresh process"
        );
    }

    #[test]
    fn over_rate_self_heal_messages_carry_the_1193_tag_and_no_sibling_anchor() {
        let m = &OVER_RATE_SELF_HEAL_MESSAGES;
        assert_eq!(
            reset_success_message(m, 3, SELF_HEAL_EXIT_CODE),
            "#1193 over-rate self-heal: USB reset attempt #3 succeeded — will exit (code 77) after graceful shutdown so systemd restarts camera-box against the re-enumerated device"
        );
        assert_eq!(
            reset_failed_message(m, 3, "boom", SELF_HEAL_RESET_FAILED_EXIT_CODE),
            "CRITICAL #1193 over-rate self-heal: USB reset attempt #3 FAILED: boom — the capture device may now be in a WORSE state than the original over-rate defect (possibly disconnected); exiting (code 78) after graceful shutdown so systemd retries with a fresh process"
        );
        // The over-rate reset lines must NOT carry the other two triggers' grep anchors, so their
        // dev1-watchdog patterns never mis-match this trigger's events.
        let s = reset_success_message(m, 1, SELF_HEAL_EXIT_CODE);
        assert!(!s.contains("#663 self-heal: USB reset attempt"));
        assert!(!s.contains("#1128 grabber-stuck self-heal"));
    }

    fn selfheal_temp_state_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir()
            .join(format!(
                "camera-box-selfheal-1149-{}-{}",
                name,
                std::process::id()
            ))
            .join("state.txt")
    }

    #[test]
    fn attempt_self_heal_heal_success_returns_77_and_calls_reset_exactly_once() {
        let sp = selfheal_temp_state_path("ok");
        let _ = std::fs::remove_dir_all(sp.parent().unwrap());
        let mut reset_calls = 0u32;
        let code = attempt_self_heal(
            "/dev/video0",
            GrabberModel::ShadowCast2,
            2_000_000,
            &sp,
            &CAPTURE_RATE_SELF_HEAL_MESSAGES,
            |p: &str| {
                reset_calls += 1;
                assert_eq!(p, "/dev/video0");
                Ok(())
            },
        );
        assert_eq!(code, Some(SELF_HEAL_EXIT_CODE));
        assert_eq!(reset_calls, 1);
        std::fs::remove_dir_all(sp.parent().unwrap()).ok();
    }

    #[test]
    fn attempt_self_heal_reset_failure_returns_the_reset_failed_exit_code() {
        let sp = selfheal_temp_state_path("err");
        let _ = std::fs::remove_dir_all(sp.parent().unwrap());
        let code = attempt_self_heal(
            "/dev/video0",
            GrabberModel::ShadowCast2,
            2_000_000,
            &sp,
            &GRABBER_STUCK_SELF_HEAL_MESSAGES,
            |_: &str| Err(anyhow::anyhow!("simulated reset failure")),
        );
        assert_eq!(code, Some(SELF_HEAL_RESET_FAILED_EXIT_CODE));
        std::fs::remove_dir_all(sp.parent().unwrap()).ok();
    }

    #[test]
    fn attempt_self_heal_throttled_returns_none_and_never_resets() {
        let sp = selfheal_temp_state_path("throttled");
        let _ = std::fs::remove_dir_all(sp.parent().unwrap());
        // Seed a heal one second ago; DEFAULT_MIN_HEAL_INTERVAL_S (600) has NOT elapsed, so the
        // decision is Throttled — no reset must fire and the return value is None.
        save_state(
            &sp,
            &SelfHealState {
                last_heal_epoch_s: Some(2_000_000),
                recurrence_heal_count: 1,
            },
        )
        .expect("seed state");
        let mut reset_calls = 0u32;
        let code = attempt_self_heal(
            "/dev/video0",
            GrabberModel::ShadowCast2,
            2_000_001,
            &sp,
            &CAPTURE_RATE_SELF_HEAL_MESSAGES,
            |_: &str| {
                reset_calls += 1;
                Ok(())
            },
        );
        assert_eq!(code, None);
        assert_eq!(
            reset_calls, 0,
            "a Throttled decision must never fire a USB reset"
        );
        std::fs::remove_dir_all(sp.parent().unwrap()).ok();
    }
}
