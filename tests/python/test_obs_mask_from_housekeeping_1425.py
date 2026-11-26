"""issue 1425 (ROZHODNUTÉ issuecomment-7183885100): the OBS mask follows the box fact STRIH_HOUSEKEEPING_CPUS.

Two housekeeping CPUs could not carry strih-snv's atlantic RX softirq, so its fact widens to 0-3 (two whole
cores, the supervisor's decision on the CPU 2/3 SMT split) and the OBS mask /etc/strih-isolated-cpus.conf
that step 11 writes becomes online minus the fact (4-15). strih-pp takes the same two whole cores on its
N,N+8 numbering (0-1,8-9 -> mask 2-7,10-15) since the IRQ-only placement. The stream boxes
keep their file byte-identical: when online minus the fact is the topology plan's CPU set, the plan's own
string is written, as today.

The pure helper `obs_box_obs_mask_from_housekeeping PLAN ONLINE HK` (scripts/lib/obs-box-background.sh) runs
under the callers' `set -euo pipefail`; the plan comes from the REAL `obs_box_cpu_isolation_plan` over each
box's topology, MODELLED on the layouts the fact-file comments record (read 25.11.2026: strih-snv adjacent
pairs, strih-pp N,N+8, stream-pp 12 threads adjacent, stream-snv 12 P-threads + 8 E-cores), not captures.
Values reach bash as ARGUMENTS.
"""
import os
import pathlib
import subprocess

REPO = pathlib.Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "scripts"
BASE = SCRIPTS / "lib" / "obs-box-baseline.sh"
BG = SCRIPTS / "lib" / "obs-box-background.sh"
FACTS = SCRIPTS / "lib" / "strih-box-facts.sh"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
HK = SCRIPTS / "lib" / "strih-housekeeping.sh"
SETUP = SCRIPTS / "setup-strih.sh"
LEAKY = ("STRIH_BOXES_DIR", "STRIH_HOUSEKEEPING_CPUS", "STRIH_HOUSEKEEPING_IRQ_CPUS", "OBS_FLEET",
         "STRIH_LX_IP", "STRIH_NIC_IFACE")


def _bash(body, *args, sources=(BG,)):
    harness = ("set -euo pipefail\n" + 'fail() { echo "FAIL: $*" >&2; exit 9; }\nYELLOW=; NC=\n' +
               "".join('. "%s"\n' % s for s in sources) + body)
    env = {k: v for k, v in os.environ.items() if k not in LEAKY}
    return subprocess.run(["bash", "-c", harness, "harness", *args], capture_output=True, text=True,
                          env=env, cwd=str(REPO), timeout=60)


def _mask(plan, online, hk):
    return _bash('obs_box_obs_mask_from_housekeeping "$1" "$2" "$3"', plan, online, hk)


# The thread-sibling layouts ("CPU SIBLINGS" lines, the stdin of obs_box_cpu_isolation_plan), per box.
def _adjacent_pairs(n):          # cpu 2k and 2k+1 share a core (strih-snv, stream-pp)
    return "".join("%d %d-%d\n" % (c, c - c % 2, c - c % 2 + 1) for c in range(n))


def _split_pairs(n):             # cpu k and k+n/2 share a core (strih-pp's Ryzen 7 5800X)
    h = n // 2
    return "".join("%d %d,%d\n" % (c, c % h, c % h + h) for c in range(n))


def _hybrid(p_threads, e_cores):  # P-core pairs 0..p_threads-1, then unpaired E-cores (stream-snv)
    return _adjacent_pairs(p_threads) + "".join("%d %d\n" % (c, c) for c in range(p_threads, p_threads + e_cores))


TOPOLOGY = {
    "strih-snv": ("0-15", _adjacent_pairs(16)),
    "strih-pp": ("0-15", _split_pairs(16)),
    "stream-pp": ("0-11", _adjacent_pairs(12)),
    "stream-snv": ("0-19", _hybrid(12, 8)),
}


def _plan(box):
    r = subprocess.run(["bash", "-c", "set -euo pipefail\nfail() { echo \"FAIL: $*\" >&2; exit 9; }\n"
                        '. "%s"\nobs_box_cpu_isolation_plan | sed -n 1p' % BASE],
                       input=TOPOLOGY[box][1], capture_output=True, text=True, timeout=60)
    assert r.returncode == 0, r.stderr
    return r.stdout.strip()


def _fact(box):
    r = _bash('strih_box_load "$1" >/dev/null; strih_lx_housekeeping_cpus', box, sources=(FACTS, PROVISION))
    assert r.returncode == 0, r.stderr
    return r.stdout.strip()


# --- the pure helper ---------------------------------------------------------------------------------------

def test_none_keeps_the_topology_plan_byte_for_byte():
    r = _mask("1,9,2,10", "0-15", "none")
    assert r.returncode == 0 and r.stdout == "1,9,2,10\n", (r.stdout, r.stderr)


def test_a_fact_equal_to_the_plans_complement_keeps_the_plan_string():
    # strih-pp's plan lists its split pairs out of order; the complement 0,8 is the same set, so the
    # file must keep that exact text (nothing moves on the box)
    plan = "1,9,2,10,3,11,4,12,5,13,6,14,7,15"
    r = _mask(plan, "0-15", "0,8")
    assert r.returncode == 0 and r.stdout.strip() == plan, (r.stdout, r.stderr)


def test_a_wider_fact_gives_the_canonical_complement():
    plan = ",".join(str(c) for c in range(2, 16))
    r = _mask(plan, "0-15", "0-2")
    assert r.returncode == 0 and r.stdout.strip() == "3-15", (r.stdout, r.stderr)


def test_a_fact_outside_online_or_leaving_no_mask_or_malformed_is_refused():
    plan = ",".join(str(c) for c in range(2, 16))
    for hk in ("0-16", "0-15", "a", "1-0", ""):
        r = _mask(plan, "0-15", hk)
        assert r.returncode == 1 and r.stdout == "", (hk, r.stdout)
        assert "housekeeping" in r.stderr, (hk, r.stderr)
    r = _mask(plan, "", "0-2")
    assert r.returncode == 1 and r.stdout == "", r.stdout


# --- every box from its own fact + topology ----------------------------------------------------------------

def test_strih_snv_housekeeping_is_0_3_and_its_obs_mask_becomes_4_15():
    assert _fact("strih-snv") == "0-3"
    online, _ = TOPOLOGY["strih-snv"]
    r = _mask(_plan("strih-snv"), online, _fact("strih-snv"))
    assert r.returncode == 0 and r.stdout.strip() == "4-15", (r.stdout, r.stderr)


def test_every_other_box_writes_exactly_todays_mask():
    # the ruling: nothing else moves. The written text = the topology plan string, byte for byte.
    for box in ("stream-pp", "stream-snv"):
        online, _ = TOPOLOGY[box]
        plan = _plan(box)
        r = _mask(plan, online, _fact(box))
        assert r.returncode == 0 and r.stdout == plan + "\n", (box, plan, r.stdout, r.stderr)


def test_strih_pp_housekeeping_is_two_whole_cores_and_its_obs_mask_becomes_2_7_10_15():
    # issue 1425 (IRQ-only placement): 0,8 was ONE core, so CPU 8 alone took both ixgbe RX queues.
    # The strih-snv rule on the Ryzen's N,N+8 numbering: cores 0 and 1 = 0-1,8-9; OBS keeps six cores.
    assert _fact("strih-pp") == "0-1,8-9"
    online, _ = TOPOLOGY["strih-pp"]
    r = _mask(_plan("strih-pp"), online, _fact("strih-pp"))
    assert r.returncode == 0 and r.stdout.strip() == "2-7,10-15", (r.stdout, r.stderr)


def test_every_strih_housekeeping_set_is_two_or_more_whole_smt_cores():
    # the whole-core rule: no housekeeping CPU shares a physical core with an OBS CPU, and at least two
    # cores (one task CPU leaves no IRQ-only CPU of a second core otherwise)
    for box in ("strih-snv", "strih-pp"):
        sib = {}
        for line in TOPOLOGY[box][1].splitlines():
            cpu, s = line.split()
            sib[int(cpu)] = s
        r = _bash('obs_box_cpulist_expand "$1"', _fact(box))
        assert r.returncode == 0, r.stderr
        hk = {int(c) for c in r.stdout.split()}
        cores = {sib[c] for c in hk}
        for core in cores:
            r = _bash('obs_box_cpulist_expand "$1"', core)
            assert {int(c) for c in r.stdout.split()} <= hk, (box, core, sorted(hk))
        assert len(cores) >= 2, (box, cores)


def test_strih_pp_consumers_derive_from_the_new_fact():
    # IRQ CPUs 0-1,8-9 (RSS count 4: ixgbe has channel control, so the first live run is a link drop),
    # task CPU 0, the IRQ-only CPUs 1,8-9 kept out of the slice and taking every rig-NIC IRQ
    r = _bash('strih_box_load strih-pp >/dev/null; strih_housekeeping_irq_cpus; strih_housekeeping_task_cpus; '
              'strih_housekeeping_slice_exclude; strih_housekeeping_count "$(strih_housekeeping_irq_cpus)"',
              sources=(FACTS, PROVISION, BG, HK))
    assert r.returncode == 0, r.stderr
    assert r.stdout.split() == ["0-1,8-9", "0", "1,8-9", "4"], r.stdout


def test_strih_snv_consumers_derive_from_the_new_fact():
    # IRQ CPUs 0-3 (the oneshot + RSS count 4), task CPU 0, the slice keeps the IRQ-only CPUs 1-3 out
    r = _bash('strih_box_load strih-snv >/dev/null; strih_housekeeping_irq_cpus; strih_housekeeping_task_cpus; '
              'strih_housekeeping_slice_exclude; strih_housekeeping_count "$(strih_housekeeping_irq_cpus)"',
              sources=(FACTS, PROVISION, BG, HK))
    assert r.returncode == 0, r.stderr
    assert r.stdout.split() == ["0-3", "0", "1-3", "4"], r.stdout


# --- the wiring --------------------------------------------------------------------------------------------

def test_setup_strih_hands_the_fact_to_the_mask_writer():
    s = SETUP.read_text()
    assert s.count('obs_box_cpu_affinity strih "$(strih_lx_housekeeping_cpus)"') == 1


def test_the_mask_writer_derives_through_the_helper_and_imag_stays_topology_only():
    b = BASE.read_text()
    start = b.index("obs_box_cpu_affinity() {")
    body = b[start:b.index("\n}\n", start)]
    assert "obs_box_obs_mask_from_housekeeping" in body
    assert '$# -ge 2' in body       # an argument passed EMPTY (an unreadable fact) fails, absent = topology
    assert (SCRIPTS / "setup-imag.sh").read_text().count("obs_box_cpu_affinity imag\n") == 1
