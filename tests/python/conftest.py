"""Shared pytest fixtures for the camera-box Python test suite.

#1207: reset av_sync_measure's module-level throttle state (`_WEBHOOK_LAST_SENT`) and outer-loop
guard cache (`_OUTER_LOOP_GUARDS`) around every test. Both `test_av_sync_measure_notify_dedup_1207.py`
and `test_av_sync_outer_loop_apply.py` write that SHARED module state within one pytest process (the
webhook throttle latches for `WEBHOOK_THROTTLE_S`=1200s), so without a reset a webhook-delivering test
leaves state latched for the rest of the process — a future webhook-delivering test (or a randomized
order) would then inherit the throttle and flake order-dependently. Guarded via `sys.modules` so this
is a genuine no-op for any test that never imports av_sync_measure (no import cost imposed on the
~1670 unrelated tests)."""
import sys

import pytest


@pytest.fixture(autouse=True)
def _reset_av_sync_measure_module_state():
    def _clear():
        mod = sys.modules.get("av_sync_measure")
        if mod is not None:
            for name in ("_WEBHOOK_LAST_SENT", "_OUTER_LOOP_GUARDS"):
                state = getattr(mod, name, None)
                if isinstance(state, dict):
                    state.clear()

    _clear()
    yield
    _clear()

import os

# issue 1487: dev1 itself may still run dantesync, and a swap program reads the box it runs on. No test
# reads the real legacy paths; tests/python/test_dsync_swap_1487.py sets its own.
os.environ["DSYNC_LEGACY_LINUX_UNIT"] = "/nonexistent/dantesync.service"
os.environ["DSYNC_LEGACY_LINUX_BIN"] = "/nonexistent/dantesync"
os.environ["DSYNC_LEGACY_LINUX_STATE"] = "/nonexistent/etc-dantesync"

# issue 1404: the program-audio sampler pins each NDI receiver to an on-link address read from the
# box's own avahi; no test reads dev1's live avahi (tests/python/test_program_audio_ndi_url_1404.py
# turns it back on with its own resolver).
os.environ["PROGRAM_AUDIO_NDI_PIN"] = "0"
