//! #841 -- imag-nb Program projector stutters. Two confirmed gaps, both in the imag
//! launch/provisioning path (live-diagnosed on 10.77.9.187, an Intel-iGPU-only replacement for
//! the NVIDIA-equipped incumbent .182):
//!
//! (1) The incumbent's anti-stutter display tuning (`nvidia-settings
//!     ForceFullCompositionPipeline=On` + `GPUPowerMizerMode=1`) is NVIDIA-only and was never
//!     provisioned onto the replacement notebook (Intel UHD / Raptor Lake-P, i915,
//!     `modesetting`+glamor, no discrete GPU) -- nothing equivalent exists there.
//!     `setup-imag.sh` must provision the Intel-appropriate counterpart, gated on the ABSENCE of
//!     a discrete NVIDIA GPU via the existing `imag_has_discrete_nvidia` helper, and must leave
//!     the existing NVIDIA-gated driver/PRIME branch byte-for-byte unchanged.
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

/// The Intel TearFree xorg.conf.d snippet must be written INSIDE the existing "no discrete
/// NVIDIA GPU" branch (#816's `imag_has_discrete_nvidia` gate) -- never unconditionally, and
/// never in the NVIDIA-present branch.
#[test]
fn setup_imag_provisions_intel_tearfree_gated_on_no_discrete_nvidia_841() {
    let body = read(SETUP);
    let gate = body
        .find("no discrete NVIDIA GPU on this box")
        .expect("the #816 no-dGPU branch must still exist");
    let tearfree = body
        .find(r#"Option "TearFree" "true""#)
        .expect("setup-imag.sh must provision an xorg.conf.d TearFree snippet for the Intel path");
    assert!(
        gate < tearfree,
        "{SETUP}: the TearFree snippet must be written inside the existing no-discrete-NVIDIA \
         branch, right where the #816 gate already decides there's no dGPU"
    );
    assert!(
        body.contains("/etc/X11/xorg.conf.d/"),
        "{SETUP}: the TearFree snippet must land under /etc/X11/xorg.conf.d/ (same convention as \
         the existing 30-touchpad-tap.conf)"
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
