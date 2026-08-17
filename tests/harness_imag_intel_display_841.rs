//! #841 -- imag-nb Program projector stutters. Two confirmed gaps, both in the imag
//! launch/provisioning path (live-diagnosed on 10.77.9.187, an Intel-iGPU-only replacement for
//! the NVIDIA-equipped incumbent .182):
//!
//! (1) The incumbent's anti-stutter display tuning (`nvidia-settings
//!     ForceFullCompositionPipeline=On` + `GPUPowerMizerMode=1`) is NVIDIA-only and was never
//!     provisioned onto the replacement notebook (Intel UHD / Raptor Lake-P, i915,
//!     `modesetting`+glamor, no discrete GPU) -- nothing equivalent exists there.
//!     `setup-imag.sh` must provision the GENUINELY-APPLICABLE Intel counterpart, gated on the
//!     ABSENCE of a discrete NVIDIA GPU via the existing `imag_has_discrete_nvidia` helper, and
//!     must leave the existing NVIDIA-gated driver/PRIME branch byte-for-byte unchanged.
//!
//!     IMPORTANT correction made DURING this ticket's own live verification (not a later fix):
//!     the naive "port ForceFullCompositionPipeline as TearFree" idea was tested LIVE on
//!     10.77.9.187 and found to be a DEAD option -- `Option "TearFree" "true"` under `Driver
//!     "modesetting"` produced `(WW) modeset(0): Option "TearFree" is not used` in Xorg.0.log, and
//!     `strings modesetting_drv.so` contains no "TearFree" text at all. TearFree is a feature of
//!     the LEGACY `xf86-video-intel` DDX, not of the built-in `modesetting` driver Xorg actually
//!     autoconfigures for this PCI id here. Shipping it anyway would have been exactly the
//!     cargo-culted-NVIDIA-semantics mistake this ticket explicitly warns against, so it is NOT
//!     written. What this stack verifiably already provides tear-free by default (same log:
//!     `Present`+`DRI3` init cleanly, `PageFlip`/`Atomic` compiled into the driver per `strings`)
//!     is direct page-flipped full-screen scanout with no compositor running -- the real
//!     mechanism, not an xorg.conf.d knob. VRR (`Option "VariableRefresh"`, also real on this
//!     build) was considered too, but the HDMI-1 projector output itself reports `vrr_capable: 0`
//!     -- not applicable to the affected output. The genuinely-applicable Intel/i915 counterpart
//!     to `GPUPowerMizerMode=1` is the GPU frequency-floor pin below, which IS real and
//!     live-verified (gt_min_freq_mhz 100 -> 1400, confirmed to survive an X/OBS restart).
//!
//! (2) `imag-obs-start.sh`'s `taskset` CPU pin fell back to a bare hardcoded `2-11` -- the
//!     INCUMBENT's 16-thread range -- whenever the wrapper is invoked without `IMAG_ISOLATED_CPUS`
//!     set (i.e. every manual "Spustit OBS" menu invocation). On the 12-thread replacement
//!     notebook, `2-11` overlaps the kernel's own `irqaffinity=...,8,9,10,11` IRQ cores, defeating
//!     the CPU isolation the kernel cmdline was derived to provide (live-verified:
//!     `Cpus_allowed_list: 2-11` on the running `obs` process while the box's OWN derived
//!     isolated set is `2,3,4,5,6,7`). The fallback must come from the SAME
//!     `imag_cpu_isolation_plan` derivation `setup-imag.sh` already uses for the kernel cmdline
//!     (persisted to a config file -- one source of truth), never a second hardcoded literal.

use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

const SETUP: &str = "scripts/setup-imag.sh";
const START: &str = "scripts/imag-obs-start.sh";

// ================================================================================================
// Gap 1 -- Intel display tuning (setup-imag.sh)
// ================================================================================================

/// TearFree must NOT be shipped -- it was live-tested and proven to be a DEAD option on the
/// `modesetting` driver actually bound on this box (`(WW) ... Option "TearFree" is not used`,
/// confirmed live 2026-07-28). Pin the ABSENCE so a future edit doesn't cargo-cult it back in
/// without re-verifying, and require the finding to be documented inline (never silently dropped
/// with no trace of why).
#[test]
fn setup_imag_does_not_ship_the_dead_tearfree_option_841() {
    let body = read(SETUP);
    // #779 legitimately provisions the touchpad INPUT config as an xorg.conf.d write
    // (30-touchpad-tap.conf) -- NOT a display-tuning file. So this ban (which exists to keep the
    // dead `Option "TearFree"` DISPLAY snippet, and any other cargo-culted display xorg.conf.d
    // knob, off the Intel path) narrows from "no xorg.conf.d write at all" to "no xorg.conf.d write
    // OTHER than the touchpad input config" -- a display cargo-cult would be a different file and
    // still trips this. A `#`-prefixed comment mentioning the option's literal name is still fine
    // (only a WRITE is banned), same as before.
    let non_touchpad_xorg_writes: Vec<&str> = body
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            t.starts_with("cat > /etc/X11/xorg.conf.d/") && !t.contains("30-touchpad-tap.conf")
        })
        .collect();
    assert!(
        non_touchpad_xorg_writes.is_empty(),
        "{SETUP}: the ONLY permitted xorg.conf.d WRITE is the #779 touchpad INPUT config \
         (30-touchpad-tap.conf); a live-tested `Option \"TearFree\"` snippet was proven a DEAD \
         option on the `modesetting` driver bound here (#841) and must not ship, nor any other \
         display xorg.conf.d knob -- found a non-touchpad write: {non_touchpad_xorg_writes:?}"
    );
    assert!(
        body.contains("is not used") && body.contains("modesetting_drv.so"),
        "{SETUP}: the comment explaining WHY TearFree was tried and rejected (the live \
         `(WW) ... is not used` finding + the strings/modesetting_drv.so check) must stay \
         documented inline, not silently dropped"
    );
}

/// The GPU frequency-scaling floor must be derived from the box's OWN reported hardware ceiling
/// (`gt_RP0_freq_mhz`) -- never a hardcoded MHz literal standing in for it (this box's ceiling
/// happens to be 1400MHz; a future Intel notebook's will differ).
#[test]
fn setup_imag_pins_igpu_min_freq_from_its_own_rp0_never_a_literal_841() {
    let body = read(SETUP);
    assert!(
        body.contains("gt_RP0_freq_mhz"),
        "{SETUP}: must read the box's OWN reported max frequency (gt_RP0_freq_mhz), never assume \
         a fixed MHz value"
    );
    assert!(
        body.contains("gt_min_freq_mhz"),
        "{SETUP}: must raise gt_min_freq_mhz to the derived ceiling so the iGPU stops idling down \
         and ramping back up under load (the DVFS-ramp stutter GPUPowerMizerMode=1 avoids on \
         NVIDIA)"
    );
    assert!(
        !body.contains("echo 1400") && !body.contains("> \"$card/gt_min_freq_mhz\" <<<1400"),
        "{SETUP}: must never hardcode THIS box's particular 1400MHz ceiling as a literal -- read \
         it from gt_RP0_freq_mhz instead"
    );
}

/// The frequency pin must be reapplied on EVERY boot (sysfs values reset on reboot) via a
/// boot-durable systemd oneshot unit -- matching the existing `cpu-performance.service` /
/// `dantesync.service` convention this script already uses, never a provisioning-time-only write.
#[test]
fn setup_imag_igpu_maxperf_service_is_enabled_every_boot_841() {
    let body = read(SETUP);
    assert!(
        body.contains("imag-igpu-maxperf.service"),
        "{SETUP}: must install a dedicated systemd oneshot unit for the iGPU frequency pin, \
         mirroring the existing cpu-performance.service convention"
    );
    assert!(
        body.contains("systemctl enable --now imag-igpu-maxperf.service"),
        "{SETUP}: the iGPU max-perf unit must be enabled (boot-durable) AND started now \
         (sysfs values reset on reboot, so re-application every boot is mandatory)"
    );
}

/// The existing NVIDIA driver + PRIME install lines must be completely untouched -- a box WITH a
/// discrete GPU keeps the existing path byte-for-byte identical.
#[test]
fn setup_imag_nvidia_branch_stays_byte_for_byte_unchanged_841() {
    let body = read(SETUP);
    assert!(
        body.contains("nvidia-driver-595-open install failed"),
        "{SETUP}: the #500 nvidia driver install must remain exactly as before"
    );
    assert!(
        body.contains("prime-select nvidia || fail \"prime-select nvidia failed\""),
        "{SETUP}: the #500 prime-select call must remain exactly as before"
    );
}

// ================================================================================================
// Gap 2 -- imag-obs-start.sh's wrapper CPU pin (no more hardcoded box-specific literal)
// ================================================================================================

/// The OLD box's bare 16-thread literal must be gone from the wrapper entirely -- not even as a
/// "sane default" fallback, since a hardcoded range from ONE box is never a sane default for a
/// DIFFERENT box's topology (#816's hardware-agnostic-derivation rule, now applied to the wrapper
/// too).
#[test]
fn imag_obs_start_no_longer_hardcodes_a_box_specific_cpu_range_841() {
    let body = read(START);
    assert!(
        !body.contains("2-11"),
        "{START}: the old box's 16-thread literal \"2-11\" must be completely gone -- no \
         hardcoded fallback taskset range for ANY box"
    );
}

/// The wrapper must fall back to the SAME persisted, derived value setup-imag.sh computes for the
/// kernel cmdline -- one source of truth, never a second hardcoded default.
#[test]
fn imag_obs_start_falls_back_to_the_persisted_derived_config_841() {
    let body = read(START);
    assert!(
        body.contains("/etc/imag-isolated-cpus.conf"),
        "{START}: a manual invocation (no IMAG_ISOLATED_CPUS env set) must fall back to reading \
         the SAME derived isolated-CPU set setup-imag.sh persists for the kernel cmdline, not a \
         second hardcoded literal"
    );
}

/// When NEITHER the env var NOR the persisted config file gives a value, the script must FAIL
/// LOUD rather than silently guess a taskset pin (the same discipline `imag_pick_ndi_peer` /
/// `imag_cpu_isolation_plan` already apply elsewhere in this provisioning path).
#[test]
fn imag_obs_start_fails_loud_when_no_derived_cpu_set_is_available_841() {
    let body = read(START);
    assert!(
        body.contains("FAIL") && body.contains("isolated"),
        "{START}: must fail loud (never silently taskset to an empty/guessed string) when neither \
         IMAG_ISOLATED_CPUS nor /etc/imag-isolated-cpus.conf provides a CPU set"
    );
}

/// setup-imag.sh must persist the SAME `imag_cpu_isolation_plan`-derived value it writes into the
/// grub cmdline into `/etc/imag-isolated-cpus.conf`, AFTER deriving it -- so the wrapper's fallback
/// is never out of sync with the actual kernel isolation.
#[test]
fn setup_imag_persists_the_derived_isolated_cpus_for_the_wrapper_841() {
    let body = read(SETUP);
    let derive = body
        .find("IMAG_ISOLATED_CPUS=\"$(printf")
        .expect("the #816 IMAG_ISOLATED_CPUS derivation must still exist");
    let persist = body.find("/etc/imag-isolated-cpus.conf").expect(
        "setup-imag.sh must persist the derived IMAG_ISOLATED_CPUS value to \
             /etc/imag-isolated-cpus.conf for the wrapper to read as its fallback",
    );
    assert!(
        derive < persist,
        "{SETUP}: the persisted config file must be written AFTER deriving IMAG_ISOLATED_CPUS, \
         not before (there'd be nothing to persist yet)"
    );
}
