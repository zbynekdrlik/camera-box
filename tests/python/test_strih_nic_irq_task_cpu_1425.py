"""issue 1425 finding 26.11.2026 09:29 (strih-snv): the rig NIC IRQs never on the task CPU.

Housekeeping 0-3 = the task CPU 0 (rustdesk, the display watch, the launcher, rig-background.slice) + the
IRQ-only CPUs 1-3. The rate-greedy placement (ties to the lower CPU) put IRQ 139 (12k/s) on CPU 0: 43 %
softirq + time_squeeze ~5/s + ~750 InDroppedDma in 25 min, while CPU 1 sat at 1.5 %. The oneshot now places
only on the IRQ-only CPUs (`strih_housekeeping_slice_exclude`, rendered as @STRIH_HOUSEKEEPING_IRQ_ONLY_CPUS@)
and falls back to the whole IRQ set only when there is none (a one-CPU set).

The harness is the one of test_strih_housekeeping_1425.py (imported as a module, so its tests are not
collected twice). An EMPTY STRIH_HOUSEKEEPING_IRQ_ONLY_CPUS = the script's rendered default.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import test_strih_housekeeping_1425 as hk  # noqa: E402

# the live order of magnitude (24.11 / 26.11 reads): 139 is the 12k/s queue of the finding
RATES = {136: 17000, 139: 12000, 140: 9500, 137: 7200, 138: 4000, 134: 3900, 135: 300, 141: 100, 142: 50}


def _placed(tmp_path, rates, start, env=None, combined=4, real_boxes=True, render_env=None, lock=()):
    sysr, irq_dir, bin_dir, e = hk._pcie_box(tmp_path, combined=combined, irqs=sorted(start))
    for n, a in start.items():
        (irq_dir / str(n) / "smp_affinity_list").write_text(a + "\n")
    (tmp_path / "interrupts").write_text(hk._interrupts(rates, 1))
    (tmp_path / "interrupts.after").write_text(hk._interrupts(rates, 3))
    hk._exe(bin_dir / "sleep", hk.SWAP_SLEEP)
    (tmp_path / "sleep.log").write_text("")
    e.update({"PROC_INTERRUPTS": str(tmp_path / "interrupts"), "IRQ_RATE_SAMPLE_S": "2",
              "FAKE_SLEEP_LOG": str(tmp_path / "sleep.log"),
              "FAKE_SLEEP_SWAP": "%s:%s" % (tmp_path / "interrupts.after", tmp_path / "interrupts"),
              "STRIH_HOUSEKEEPING_IRQ_ONLY_CPUS": ""})
    e.update(env or {})
    # an IRQ the run must keep is never written: its read-only file would fail a write
    for n in lock:
        (irq_dir / str(n) / "smp_affinity_list").chmod(0o444)
    try:
        r = hk._render_and_run(tmp_path, sysr, irq_dir, bin_dir, e, render_env=render_env, real_boxes=real_boxes)
    finally:
        for n in lock:
            (irq_dir / str(n) / "smp_affinity_list").chmod(0o644)
    return r, dict(zip(sorted(start), hk._aff(irq_dir, sorted(start))))


def test_every_box_renders_its_irq_only_cpus_into_the_oneshot():
    want = {"strih-snv": "1-3", "strih-pp": "8", "stream-pp": "1", "stream-snv": "1,12-19"}
    for box, v in want.items():
        r = hk._run('strih_box_load "$1"; strih_nic_irq_affinity_script_text', box, real_boxes=True)
        assert r.returncode == 0, (box, r.stderr)
        assert 'IRQ_ONLY_CPUS="${STRIH_HOUSEKEEPING_IRQ_ONLY_CPUS:-%s}"' % v in r.stdout, box


def test_the_live_case_a_loaded_sample_never_places_an_irq_on_the_task_cpu(tmp_path):
    # strih-snv 0-3 from the real fact file: 139 (and every other IRQ) on CPU 0 is moved, the greedy runs
    # over 1-3 only (hand-run: heaviest first to the least-loaded, ties to the lower CPU)
    r, aff = _placed(tmp_path, RATES, {n: "0" for n in RATES})
    assert r.returncode == 0, r.stderr + r.stdout
    assert aff == {134: "2", 135: "3", 136: "1", 137: "3", 138: "2", 139: "2", 140: "3", 141: "1", 142: "3"}
    assert "IRQ 139 (msi enp7s0, iface enp7s0): 12000/s -> cpu 2 (was 0)" in r.stdout
    assert "assigned load: cpu 1 17100/s, cpu 2 19900/s, cpu 3 17050/s" in r.stdout
    assert "cpu 0 " not in r.stdout.split("assigned load:")[1].splitlines()[0]


def test_an_idle_sample_moves_an_irq_off_the_task_cpu_and_keeps_the_rest(tmp_path):
    # under IRQ_LOAD_MIN_PER_S: an IRQ on an IRQ-only CPU keeps it; one on the task CPU counts as misplaced
    # and goes to the IRQ-only CPU holding the fewest IRQs (cpu 3: 0 IRQs, cpus 1 and 2: one each)
    start = {134: "0", 135: "1", 136: "2"}
    r, aff = _placed(tmp_path, {134: 10, 135: 10, 136: 10}, start, lock=(135, 136))
    assert r.returncode == 0, r.stderr + r.stdout
    assert aff == {134: "3", 135: "1", 136: "2"}
    assert "IRQ 135: on housekeeping cpu 1 -- kept (no load to balance by)" in r.stdout
    assert "IRQ 134: on housekeeping cpu 0 -- kept" not in r.stdout
    assert "IRQ 134 (msi enp7s0, iface enp7s0): 10/s -> cpu 3 (was 0)" in r.stdout


def test_a_one_cpu_set_still_places_on_its_only_cpu(tmp_path):
    # no IRQ-only CPU (the task CPU is the whole set): the rendered default is `none`, the IRQs share it
    render_env = hk._box_with(tmp_path, "3")
    r0 = hk._run('strih_box_load strih-snv; strih_nic_irq_affinity_script_text', env=render_env)
    assert 'IRQ_ONLY_CPUS="${STRIH_HOUSEKEEPING_IRQ_ONLY_CPUS:-none}"' in r0.stdout, r0.stderr
    r, aff = _placed(tmp_path, RATES, {n: "0-15" for n in RATES}, combined=1, real_boxes=False,
                     render_env=render_env)
    assert r.returncode == 0, r.stderr + r.stdout
    assert set(aff.values()) == {"3"}


def test_an_irq_only_list_outside_the_irq_cpus_or_not_a_cpulist_is_fatal(tmp_path):
    for i, (bad, why) in enumerate((("1-9", "is not inside the IRQ CPUs"), ("x", "is not a cpulist"))):
        d = tmp_path / str(i)
        d.mkdir()
        r, aff = _placed(d, RATES, {n: "0-15" for n in RATES}, env={"STRIH_HOUSEKEEPING_IRQ_ONLY_CPUS": bad})
        assert r.returncode == 1, (bad, r.stdout)
        assert "FATAL: STRIH_HOUSEKEEPING_IRQ_ONLY_CPUS '%s' %s" % (bad, why) in r.stderr, (bad, r.stderr)
        assert set(aff.values()) == {"0-15"}, bad   # nothing placed before the refusal
