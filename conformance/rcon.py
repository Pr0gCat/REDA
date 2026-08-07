"""Minimal Source RCON client.

No dependencies beyond the standard library, on purpose: this whole probe
suite exists to ask a *live* Minecraft server questions, so the client that
talks to it should not itself carry any risk of being wrong in an
interesting way. The protocol is small enough to implement directly from
the spec (https://developer.valvesoftware.com/wiki/Source_RCON_Protocol)
rather than trust a third-party package we cannot audit as quickly.

Packet layout (all integers little-endian, body is ASCII + two NUL bytes):

    int32 length      (byte count of everything after this field)
    int32 request_id
    int32 type         (3 = SERVERDATA_AUTH, 2 = SERVERDATA_EXECCOMMAND,
                         0 = SERVERDATA_RESPONSE_VALUE,
                         2 = SERVERDATA_AUTH_RESPONSE)
    bytes body          (ASCII, NUL-terminated)
    byte  pad           (extra NUL required by the spec)
"""

from __future__ import annotations

import socket
import struct


SERVERDATA_AUTH = 3
SERVERDATA_EXECCOMMAND = 2
SERVERDATA_AUTH_RESPONSE = 2
SERVERDATA_RESPONSE_VALUE = 0


class RconError(RuntimeError):
    pass


class RconClient:
    def __init__(self, host: str, port: int, password: str, timeout: float = 10.0):
        self.host = host
        self.port = port
        self.password = password
        self.timeout = timeout
        self._sock: socket.socket | None = None
        self._next_id = 1

    def connect(self) -> None:
        sock = socket.create_connection((self.host, self.port), timeout=self.timeout)
        sock.settimeout(self.timeout)
        self._sock = sock
        req_id = self._send(SERVERDATA_AUTH, self.password)
        resp_id, _resp_type, _body = self._read_packet()
        if resp_id != req_id:
            self.close()
            raise RconError("RCON authentication failed (bad password?)")

    def close(self) -> None:
        if self._sock is not None:
            try:
                self._sock.close()
            finally:
                self._sock = None

    def __enter__(self) -> "RconClient":
        self.connect()
        return self

    def __exit__(self, *exc) -> None:
        self.close()

    def command(self, cmd: str) -> str:
        if self._sock is None:
            raise RconError("not connected")
        req_id = self._send(SERVERDATA_EXECCOMMAND, cmd)
        # Multi-packet responses are not a concern for the short strings
        # ("Test passed"/"Test failed"/block dumps) this suite reads back.
        _resp_id, _resp_type, body = self._read_packet()
        return body

    def _send(self, pkt_type: int, body: str) -> int:
        assert self._sock is not None
        req_id = self._next_id
        self._next_id += 1
        payload = struct.pack("<ii", req_id, pkt_type) + body.encode("utf-8") + b"\x00\x00"
        packet = struct.pack("<i", len(payload)) + payload
        self._sock.sendall(packet)
        return req_id

    def _read_packet(self) -> tuple[int, int, str]:
        assert self._sock is not None
        length_bytes = self._recv_exact(4)
        (length,) = struct.unpack("<i", length_bytes)
        payload = self._recv_exact(length)
        req_id, resp_type = struct.unpack("<ii", payload[:8])
        body = payload[8:-2].decode("utf-8", errors="replace")
        return req_id, resp_type, body

    def _recv_exact(self, n: int) -> bytes:
        assert self._sock is not None
        chunks = []
        remaining = n
        while remaining > 0:
            chunk = self._sock.recv(remaining)
            if not chunk:
                raise RconError("connection closed while reading RCON response")
            chunks.append(chunk)
            remaining -= len(chunk)
        return b"".join(chunks)
