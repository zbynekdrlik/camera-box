"""#365 — unit tests for the pure helpers in scripts/frozen-camera-gate.py.

Tests the OBS-independent logic: PNG extraction, SHA-1 hashing, and timeline
JSON serialisation.  No OBS WebSocket is opened here.
"""
import importlib.util
import json
import struct
import zlib
from pathlib import Path


HERE = Path(__file__).parent
SCRIPTS = HERE.parent.parent / "scripts"

# ---------------------------------------------------------------------------
# Load the module without executing its __main__ block
# ---------------------------------------------------------------------------

def _load_module():
    spec = importlib.util.spec_from_file_location(
        "frozen_camera_gate",
        SCRIPTS / "frozen-camera-gate.py",
    )
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


_mod = _load_module()
_hash_png = _mod._hash_png
_extract_png = _mod._extract_png
_build_timeline_json = _mod._build_timeline_json
_scene_for_input = _mod._scene_for_input


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _make_png(pixel: bytes = b"\x80\x80\x80") -> bytes:
    """Synthesise a minimal 1×1 PNG (RGB) from a 3-byte pixel value.

    Structure: PNG signature + IHDR + IDAT + IEND, all built by hand so we
    have no PIL/Pillow dep in the test environment."""
    sig = b"\x89PNG\r\n\x1a\n"

    def _chunk(name: bytes, data: bytes) -> bytes:
        crc = zlib.crc32(name + data) & 0xFFFFFFFF
        return struct.pack(">I", len(data)) + name + data + struct.pack(">I", crc)

    ihdr = _chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 2, 0, 0, 0))
    # IDAT: raw scanline for a 1×1 RGB image = filter byte (0) + 3 colour bytes
    raw = b"\x00" + pixel
    idat = _chunk(b"IDAT", zlib.compress(raw))
    iend = _chunk(b"IEND", b"")
    return sig + ihdr + idat + iend


def _b64_png(pixel: bytes = b"\x80\x80\x80") -> str:
    import base64
    return base64.b64encode(_make_png(pixel)).decode()


def _data_uri_png(pixel: bytes = b"\x80\x80\x80") -> str:
    return f"data:image/png;base64,{_b64_png(pixel)}"


# ---------------------------------------------------------------------------
# _hash_png
# ---------------------------------------------------------------------------

class TestHashPng:
    def test_same_bytes_same_hash(self):
        data = b"hello world"
        assert _hash_png(data) == _hash_png(data)

    def test_different_bytes_different_hash(self):
        assert _hash_png(b"aaa") != _hash_png(b"bbb")

    def test_returns_hex_string(self):
        h = _hash_png(b"test")
        assert isinstance(h, str)
        assert all(c in "0123456789abcdef" for c in h)

    def test_sha1_length_is_40(self):
        assert len(_hash_png(b"x" * 100)) == 40

    def test_identical_png_bytes_give_identical_hash(self):
        png = _make_png(b"\xaa\xbb\xcc")
        assert _hash_png(png) == _hash_png(png)

    def test_different_png_bytes_give_different_hash(self):
        png_a = _make_png(b"\x00\x00\x00")
        png_b = _make_png(b"\xff\xff\xff")
        assert _hash_png(png_a) != _hash_png(png_b)


# ---------------------------------------------------------------------------
# _extract_png
# ---------------------------------------------------------------------------

class TestExtractPng:
    def test_bare_base64_returns_bytes(self):
        b64 = _b64_png()
        result = _extract_png(b64)
        assert result is not None
        assert isinstance(result, bytes)
        assert result[:8] == b"\x89PNG\r\n\x1a\n"

    def test_data_uri_returns_bytes(self):
        uri = _data_uri_png()
        result = _extract_png(uri)
        assert result is not None
        assert result[:8] == b"\x89PNG\r\n\x1a\n"

    def test_empty_string_returns_none(self):
        assert _extract_png("") is None

    def test_invalid_base64_returns_none(self):
        assert _extract_png("NOT-VALID-BASE64!!!") is None

    def test_bare_and_data_uri_round_trip_to_same_bytes(self):
        pixel = b"\x11\x22\x33"
        via_bare = _extract_png(_b64_png(pixel))
        via_uri = _extract_png(_data_uri_png(pixel))
        assert via_bare == via_uri

    def test_different_pixels_give_different_bytes(self):
        a = _extract_png(_b64_png(b"\x00\x00\x00"))
        b_ = _extract_png(_b64_png(b"\xff\xff\xff"))
        assert a != b_


# ---------------------------------------------------------------------------
# _build_timeline_json
# ---------------------------------------------------------------------------

class TestBuildTimelineJson:
    def test_returns_valid_json_string(self):
        j = _build_timeline_json({"NDI cam1": ["a", "b", None]})
        parsed = json.loads(j)
        assert parsed == {"NDI cam1": ["a", "b", None]}

    def test_none_encodes_as_json_null(self):
        j = _build_timeline_json({"x": [None, "h"]})
        assert "null" in j

    def test_multiple_sources(self):
        tl = {
            "NDI cam1": ["h1", "h2"],
            "NDI cam2": ["h3", None],
        }
        parsed = json.loads(_build_timeline_json(tl))
        assert parsed["NDI cam1"] == ["h1", "h2"]
        assert parsed["NDI cam2"] == ["h3", None]

    def test_json_contract_round_trips_through_json_loads(self):
        """What the Python writes, the Rust binary must be able to parse.

        The JSON must be a flat object mapping source names to lists of strings
        or nulls — no nesting, no extra keys."""
        tl = {
            "NDI cam1": ["aabbcc", None, "ddeeff"],
            "NDI cam5": [None, None],
        }
        raw = _build_timeline_json(tl)
        parsed = json.loads(raw)
        assert isinstance(parsed, dict)
        for src, entries in parsed.items():
            assert isinstance(entries, list)
            for entry in entries:
                assert entry is None or isinstance(entry, str)


# ---------------------------------------------------------------------------
# #747 — _scene_for_input: map a raw NDI camera INPUT name to its wrapping strih 'Cam N'
# SCENE name, so the per-source warm-up can put that scene on PREVIEW before sampling.
# ---------------------------------------------------------------------------

class TestSceneForInput:
    def test_maps_canonical_ndi_cam_names(self):
        assert _scene_for_input("NDI cam1") == "Cam 1"
        assert _scene_for_input("NDI cam5") == "Cam 5"
        assert _scene_for_input("NDI cam6") == "Cam 6"

    def test_multi_digit_camera_number(self):
        assert _scene_for_input("NDI cam12") == "Cam 12"

    def test_non_canonical_names_return_none(self):
        # Non-standard --sources values (a caller override, a test fixture, a typo) must
        # degrade to "no warm-up for this one" rather than raise — see _capture_timelines.
        assert _scene_for_input("Multiview") is None
        assert _scene_for_input("") is None
        assert _scene_for_input("NDI cam") is None
        assert _scene_for_input("cam1") is None
        assert _scene_for_input("NDI cam5 ") is None
        assert _scene_for_input("NDI cam5x") is None


# ---------------------------------------------------------------------------
# #747 — _resolve_original_preview: read Studio Mode + the operator's current preview
# scene ONCE before warming, so main() can restore it after the gate finishes.
# ---------------------------------------------------------------------------

class TestResolveOriginalPreview:
    def test_studio_on_returns_current_preview(self, monkeypatch):
        def fake_rpc(ws, rtype, rdata=None, ignore_err=False):
            if rtype == "GetStudioModeEnabled":
                return {"studioModeEnabled": True}
            if rtype == "GetCurrentPreviewScene":
                return {"currentPreviewSceneName": "Multiview"}
            raise AssertionError(f"unexpected RPC {rtype!r}")

        monkeypatch.setattr(_mod, "_rpc", fake_rpc)
        studio, orig = _mod._resolve_original_preview(object())
        assert studio is True
        assert orig == "Multiview"

    def test_studio_off_never_reads_preview_and_returns_none(self, monkeypatch):
        def fake_rpc(ws, rtype, rdata=None, ignore_err=False):
            if rtype == "GetStudioModeEnabled":
                return {"studioModeEnabled": False}
            raise AssertionError(
                f"unexpected RPC {rtype!r} — must not read preview when Studio Mode is off"
            )

        monkeypatch.setattr(_mod, "_rpc", fake_rpc)
        studio, orig = _mod._resolve_original_preview(object())
        assert studio is False
        assert orig is None


# ---------------------------------------------------------------------------
# #747 — _capture_timelines warm-up: before sampling EACH source, its wrapping 'Cam N'
# scene must be put on PREVIEW and settled, THEN sampled. Post-#730/#508 Multiview
# decoupling, an input not currently SHOWING renders nothing — sampling cold is
# indistinguishable from a genuine freeze.
# ---------------------------------------------------------------------------

class TestCaptureTimelinesWarmUp:
    def _patch_common(self, monkeypatch, calls, sleeps):
        def fake_rpc(ws, rtype, rdata=None, ignore_err=False):
            calls.append((rtype, rdata))
            return {}

        monkeypatch.setattr(_mod, "_rpc", fake_rpc)
        monkeypatch.setattr(_mod, "_screenshot_hash", lambda ws, src: f"h-{src}")
        monkeypatch.setattr(_mod.time, "sleep", lambda s: sleeps.append(s))

    def test_warms_scene_before_sampling(self, monkeypatch):
        calls, sleeps = [], []
        self._patch_common(monkeypatch, calls, sleeps)

        timelines = _mod._capture_timelines(
            ws=object(), sources=["NDI cam5"], n_samples=2, cadence_s=1.0, warm_settle_s=3.0
        )

        assert timelines == {"NDI cam5": ["h-NDI cam5", "h-NDI cam5"]}
        assert calls[0] == ("SetCurrentPreviewScene", {"sceneName": "Cam 5"}), (
            "the scene warm-up must happen BEFORE any screenshot sampling, got calls=%r" % calls
        )
        assert sleeps[0] == 3.0, "the settle sleep must be the warm_settle_s value, first"
        assert sleeps[1:] == [1.0], "one cadence sleep between the 2 samples (n_samples=2)"

    def test_skips_warm_for_unmapped_source(self, monkeypatch):
        calls, sleeps = [], []
        self._patch_common(monkeypatch, calls, sleeps)

        _mod._capture_timelines(
            ws=object(), sources=["Multiview"], n_samples=1, cadence_s=1.0, warm_settle_s=3.0
        )

        assert calls == [], "a source with no 'Cam N' scene mapping must be sampled cold"

    def test_skips_warm_when_settle_is_zero(self, monkeypatch):
        calls, sleeps = [], []
        self._patch_common(monkeypatch, calls, sleeps)

        _mod._capture_timelines(
            ws=object(), sources=["NDI cam5"], n_samples=1, cadence_s=1.0, warm_settle_s=0.0
        )

        assert calls == [], "warm_settle_s<=0 (Studio Mode off) must skip warming entirely"

    def test_multi_source_warms_each_in_turn_in_order(self, monkeypatch):
        calls, sleeps = [], []
        self._patch_common(monkeypatch, calls, sleeps)

        _mod._capture_timelines(
            ws=object(),
            sources=["NDI cam1", "NDI cam3"],
            n_samples=1,
            cadence_s=1.0,
            warm_settle_s=1.0,
        )

        warm_calls = [c for c in calls if c[0] == "SetCurrentPreviewScene"]
        assert warm_calls == [
            ("SetCurrentPreviewScene", {"sceneName": "Cam 1"}),
            ("SetCurrentPreviewScene", {"sceneName": "Cam 3"}),
        ], "each source's scene must be warmed in turn, in source-list order"
