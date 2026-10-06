"""A minimal FAKE obs-websocket v5 server for the issue-1367 clean-close tests.

Standard library only (socket + threading): the RFC 6455 opening handshake, unfragmented frames
both ways (client frames masked, server frames not), and the obs-websocket v5 Hello / Identify /
Identified exchange, with or without an authentication challenge. It answers GetStreamStatus,
GetRecordStatus and StopRecord from a scripted state and records every request type it receives,
so a test can prove what the emitted PowerShell asked for and in which order. Anything else is
answered with a failed requestStatus.

Each client connection is served in turn on one background thread. Used by
tests/python/test_deploy_clean_close_win_1367.py.
"""
import base64
import hashlib
import json
import re
import socket
import struct
import threading

WS_GUID = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
CHALLENGE = "+IxH4CnCiqpX1rM9scsNynZzbOe4KhDeYcTNS3PDaeY="
SALT = "lM1GncleQOaCu9lT1yeUZhFYnqhsLLP1G5lAGo3ixaI="


def obs_auth(password, salt=SALT, challenge=CHALLENGE):
    """The obs-websocket v5 authentication string for PASSWORD (protocol.md, Creating an
    authentication string): base64(sha256(base64(sha256(password + salt)) + challenge))."""
    secret = base64.b64encode(hashlib.sha256((password + salt).encode()).digest()).decode()
    return base64.b64encode(hashlib.sha256((secret + challenge).encode()).digest()).decode()


class FakeObsWs:
    """streaming / recording: the output state GetStreamStatus / GetRecordStatus report.
    password: None = no authentication, else the password the Identify must prove.
    record_stops_after: how many GetRecordStatus reads after StopRecord still report active."""

    def __init__(self, streaming=False, recording=False, password=None, record_stops_after=0):
        self.streaming = streaming
        self.recording = recording
        self.password = password
        self.record_stops_after = record_stops_after
        self.requests = []
        self.connections = 0
        self.identified = 0
        self.auth_failures = 0
        self.errors = []
        self._stop_pending = None
        self._sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._sock.bind(("127.0.0.1", 0))
        self._sock.listen(8)
        self.port = self._sock.getsockname()[1]
        self.uri = f"ws://127.0.0.1:{self.port}"
        self._thread = threading.Thread(target=self._serve, daemon=True)
        self._thread.start()

    def close(self):
        self._sock.close()
        self._thread.join(timeout=5)

    # --- transport ----------------------------------------------------------------------------

    def _serve(self):
        while True:
            try:
                conn, _ = self._sock.accept()
            except OSError:
                return
            self.connections += 1
            try:
                self._handle(conn)
            except Exception as exc:  # recorded, never swallowed: tests assert errors == []
                self.errors.append(repr(exc))
            finally:
                conn.close()

    @staticmethod
    def _read_exact(conn, n):
        buf = b""
        while len(buf) < n:
            chunk = conn.recv(n - len(buf))
            if not chunk:
                raise ConnectionError("client closed mid-frame")
            buf += chunk
        return buf

    def _recv_frame(self, conn):
        b1, b2 = self._read_exact(conn, 2)
        opcode = b1 & 0x0F
        length = b2 & 0x7F
        if length == 126:
            length = struct.unpack(">H", self._read_exact(conn, 2))[0]
        elif length == 127:
            length = struct.unpack(">Q", self._read_exact(conn, 8))[0]
        mask = self._read_exact(conn, 4) if b2 & 0x80 else b"\0\0\0\0"
        data = self._read_exact(conn, length)
        return opcode, bytes(c ^ mask[i % 4] for i, c in enumerate(data))

    @staticmethod
    def _send_frame(conn, opcode, payload):
        n = len(payload)
        if n < 126:
            head = struct.pack(">BB", 0x80 | opcode, n)
        elif n < 65536:
            head = struct.pack(">BBH", 0x80 | opcode, 126, n)
        else:
            head = struct.pack(">BBQ", 0x80 | opcode, 127, n)
        conn.sendall(head + payload)

    def _send(self, conn, obj):
        self._send_frame(conn, 0x1, json.dumps(obj).encode())

    # --- obs-websocket v5 ---------------------------------------------------------------------

    def _handle(self, conn):
        conn.settimeout(10)
        head = b""
        while b"\r\n\r\n" not in head:
            chunk = conn.recv(4096)
            if not chunk:
                return
            head += chunk
        key = re.search(rb"Sec-WebSocket-Key:\s*(\S+)", head, re.I).group(1)
        accept = base64.b64encode(hashlib.sha1(key + WS_GUID).digest())
        conn.sendall(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n"
                     b"Connection: Upgrade\r\nSec-WebSocket-Accept: " + accept + b"\r\n\r\n")
        hello = {"op": 0, "d": {"obsWebSocketVersion": "5.6.3", "rpcVersion": 1}}
        if self.password is not None:
            hello["d"]["authentication"] = {"challenge": CHALLENGE, "salt": SALT}
        self._send(conn, hello)
        while True:
            opcode, payload = self._recv_frame(conn)
            if opcode == 0x8:  # close: echo it and end this connection
                self._send_frame(conn, 0x8, payload[:2])
                return
            if opcode != 0x1:
                continue
            msg = json.loads(payload)
            if msg.get("op") == 1:
                if self.password is not None and msg["d"].get("authentication") != obs_auth(self.password):
                    self.auth_failures += 1
                    self._send_frame(conn, 0x8, struct.pack(">H", 4009) + b"Authentication failed.")
                    return
                self.identified += 1
                self._send(conn, {"op": 2, "d": {"negotiatedRpcVersion": 1}})
            elif msg.get("op") == 6:
                self._answer(conn, msg["d"])

    def _answer(self, conn, d):
        rt = d["requestType"]
        self.requests.append(rt)
        ok, data = True, None
        if rt == "GetStreamStatus":
            data = {"outputActive": self.streaming, "outputReconnecting": False}
        elif rt == "GetRecordStatus":
            if self._stop_pending is not None:
                if self._stop_pending <= 0:
                    self.recording = False
                    self._stop_pending = None
                else:
                    self._stop_pending -= 1
            data = {"outputActive": self.recording, "outputPaused": False}
        elif rt == "StopRecord" and self.recording:
            self._stop_pending = self.record_stops_after
            data = {"outputPath": "C:/_REC/fake.mkv"}
        else:
            ok = False
        status = {"result": ok, "code": 100 if ok else 501}
        if not ok:
            status["comment"] = f"fake obs-websocket: {rt} not served"
        out = {"op": 7, "d": {"requestType": rt, "requestId": d["requestId"], "requestStatus": status}}
        if data is not None:
            out["d"]["responseData"] = data
        self._send(conn, out)
