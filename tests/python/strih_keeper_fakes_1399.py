"""Shared fakes for the issue-1399 keeper tests: a REAL obs-websocket 5 server on localhost (stdlib
RFC 6455), with an OBS identity (a process id + the GetStats frame counter) that `restart()` changes and
`drop()` keeps. Not a test module (no `test_` prefix), imported by both keeper test files."""
import base64
import hashlib
import json
import socket
import struct
import threading


# --- a real obs-websocket 5 server (stdlib RFC 6455) ------------------------------------------------

WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"


def _recv_exact(conn, n):
    buf = b""
    while len(buf) < n:
        chunk = conn.recv(n - len(buf))
        if not chunk:
            raise ConnectionError("peer closed")
        buf += chunk
    return buf


class FakeObsWebSocket:
    """obs-websocket 5 over a real localhost socket: Hello -> Identify -> Identified, then op 6
    requests answered with op 7. Records every Identify and request with its connection number and
    the number of GetInputList requests answered so far (= the keeper's pass count)."""

    SALT, CHALLENGE = "c2FsdC0xMzk5", "Y2hhbGxlbmdlLTEzOTk="

    def __init__(self, inputs, password=None, process="boot-1:1000"):
        self.inputs = inputs
        self.password = password
        self.process = process  # the OBS process identity the keeper's obs_process() reads
        self.frames = 0  # GetStats renderTotalFrames: grows with every read, back to 0 on a restart
        self.auth_ok = []
        self.lock = threading.Lock()
        self.connections = 0
        self.identifies = []
        self.requests = []
        self.list_calls = 0
        self._conn = None
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.sock.bind(("127.0.0.1", 0))
        self.sock.listen(8)
        self.port = self.sock.getsockname()[1]
        threading.Thread(target=self._accept, daemon=True).start()

    def close(self):
        self.sock.close()
        self.drop()

    def process_identity(self):
        """The keeper's obs_process callable: the identity of the OBS process now running."""
        return self.process

    def restart(self, process=None, frames=0):
        """An OBS restart: a new process, the frame count from FRAMES (0), the connection dropped."""
        with self.lock:
            self.frames = frames
            self.process = process or self.process + "+"
        self.drop()

    def drop(self):
        """Close the current connection WITHOUT an OBS restart (a WS hiccup, a request timeout)."""
        with self.lock:
            conn, self._conn = self._conn, None
        if conn is not None:
            try:
                conn.shutdown(socket.SHUT_RDWR)
            except OSError as e:
                print("fake obs: shutdown of a dead connection: %s" % e)
            conn.close()

    def presses(self):
        return [(c, p, d["inputName"], d["propertyName"]) for c, p, t, d in self.requests
                if t == "PressInputPropertiesButton"]

    def _accept(self):
        while True:
            try:
                conn, _ = self.sock.accept()
            except OSError:
                return
            with self.lock:
                self.connections += 1
                n = self.connections
                self._conn = conn
            threading.Thread(target=self._serve, args=(conn, n), daemon=True).start()

    def _send(self, conn, obj):
        data = json.dumps(obj).encode()
        if len(data) < 126:
            head = struct.pack("!BB", 0x81, len(data))
        elif len(data) < 65536:
            head = struct.pack("!BBH", 0x81, 126, len(data))
        else:
            head = struct.pack("!BBQ", 0x81, 127, len(data))
        conn.sendall(head + data)

    def _recv(self, conn):
        while True:
            b1, b2 = _recv_exact(conn, 2)
            opcode, length = b1 & 0x0F, b2 & 0x7F
            if length == 126:
                length = struct.unpack("!H", _recv_exact(conn, 2))[0]
            elif length == 127:
                length = struct.unpack("!Q", _recv_exact(conn, 8))[0]
            mask = _recv_exact(conn, 4) if b2 & 0x80 else b"\0\0\0\0"
            payload = bytes(c ^ mask[i % 4] for i, c in enumerate(_recv_exact(conn, length)))
            if opcode == 8:
                return None
            if opcode == 9:
                conn.sendall(struct.pack("!BB", 0x8A, len(payload)) + payload)
                continue
            if opcode == 1:
                return json.loads(payload.decode())

    def _handshake(self, conn):
        raw = b""
        while b"\r\n\r\n" not in raw:
            chunk = conn.recv(4096)
            if not chunk:
                raise ConnectionError("closed during handshake")
            raw += chunk
        key = ""
        for line in raw.decode("latin-1").split("\r\n"):
            if line.lower().startswith("sec-websocket-key:"):
                key = line.split(":", 1)[1].strip()
        accept = base64.b64encode(hashlib.sha1((key + WS_GUID).encode()).digest()).decode()
        conn.sendall(("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
                      "Sec-WebSocket-Accept: %s\r\n\r\n" % accept).encode())

    def _handle(self, rtype, data):
        if rtype == "GetInputList":
            with self.lock:
                self.list_calls += 1
            return True, {"inputs": [{"inputName": i["name"], "inputKind": i["kind"],
                                      "unversionedInputKind": i["kind"]} for i in self.inputs]}
        if rtype == "GetInputSettings":
            for i in self.inputs:
                if i["name"] == data.get("inputName"):
                    return True, {"inputSettings": i["settings"], "inputKind": i["kind"]}
            return False, None
        if rtype == "PressInputPropertiesButton":
            return any(i["name"] == data.get("inputName") for i in self.inputs), None
        if rtype == "GetStats":
            with self.lock:
                self.frames += 30
                return True, {"renderTotalFrames": self.frames, "activeFps": 30.0}
        return False, None

    def _serve(self, conn, n):
        try:
            self._handshake(conn)
            hello = {"obsWebSocketVersion": "5.6.2", "rpcVersion": 1}
            if self.password:
                hello["authentication"] = {"challenge": self.CHALLENGE, "salt": self.SALT}
            self._send(conn, {"op": 0, "d": hello})
            ident = self._recv(conn)
            if ident is None:
                return
            with self.lock:
                self.identifies.append((n, ident))
            if self.password:
                b64sha = lambda s: base64.b64encode(hashlib.sha256(s.encode()).digest()).decode()
                ok = ident["d"].get("authentication") == b64sha(b64sha(self.password + self.SALT) + self.CHALLENGE)
                self.auth_ok.append(ok)
                if not ok:
                    return
            self._send(conn, {"op": 2, "d": {"negotiatedRpcVersion": 1}})
            while True:
                msg = self._recv(conn)
                if msg is None:
                    return
                if msg.get("op") != 6:
                    continue
                d = msg["d"]
                rtype, rdata = d["requestType"], d.get("requestData") or {}
                with self.lock:
                    self.requests.append((n, self.list_calls, rtype, rdata))
                ok, resp = self._handle(rtype, rdata)
                out = {"requestType": rtype, "requestId": d["requestId"],
                       "requestStatus": {"result": ok, "code": 100 if ok else 600}}
                if resp is not None:
                    out["responseData"] = resp
                self._send(conn, {"op": 7, "d": out})
        except (OSError, ConnectionError, ValueError):
            return
        finally:
            conn.close()


INPUTS = [
    {"name": "Browser camera crew", "kind": "browser_source", "settings": {"url": "http://presenter.lan/ui/camera"}},
    {"name": "Odpocet", "kind": "browser_source", "settings": {"url": "http://presenter.lan/stream/moderator"}},
    {"name": "Browser Ableset", "kind": "browser_source", "settings": {"url": "fohabl.lan"}},
    {"name": "Local page", "kind": "browser_source", "settings": {"is_local_file": True, "local_file": "/x.html"}},
    {"name": "NDI cam1", "kind": "ndi_source", "settings": {"ndi_source_name": "CAM1 (usb)"}},
]
PRESENTER, FOHABL = ("presenter.lan", 80), ("fohabl.lan", 80)
