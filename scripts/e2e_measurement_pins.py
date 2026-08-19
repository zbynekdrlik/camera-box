#!/usr/bin/env python3
"""#1003 -- MEASUREMENT-WINDOW per-camera equalization resolver (STUB -- RED commit).

Real implementation lands in the following GREEN commit. See tests/python/
test_e2e_measurement_pins.py for the contract these functions must satisfy."""
from __future__ import annotations

FRAME_PERIOD_MS = 1000.0 / 30.0


def load_profile(path):
    raise NotImplementedError


def transport_ms(cam):
    raise NotImplementedError


def resolve_pins(profile):
    raise NotImplementedError


def resolve_hold(profile):
    raise NotImplementedError


def resolve_av_expected(profile):
    raise NotImplementedError


def resolve_plan(profile):
    raise NotImplementedError


def coherence_check(profile):
    raise NotImplementedError


def classify_leftover(live_ms, production_ref_ms, test_value_ms, slack_ms):
    raise NotImplementedError


def staleness_report(profile, observed_delivery_ms, staleness_frames):
    raise NotImplementedError


def main(argv=None):
    raise NotImplementedError
