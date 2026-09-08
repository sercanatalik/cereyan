"""``cereyan mcp``: the stdio transport of the built-in MCP server.

Hosts such as Claude Code and Claude Desktop start this process and speak
JSON-RPC over its stdin and stdout, one message per line. Every message is
forwarded to the running server's ``/mcp`` endpoint (HTTP with the API token,
or the Unix socket), so the tools live in one place: the server.
"""

from __future__ import annotations

import http.client
import json
import os
import sys
import urllib.parse
from typing import Any

from . import _core
from .client import _UnixConnection, read_discovery, resolve_client_token

SESSION_HEADER = "Mcp-Session-Id"


class Transport:
    """One HTTP connection target for ``/mcp``: a URL with a token, or a socket path."""

    def __init__(self, url: str | None, token: str | None, socket_path: str | None, timeout: float = 120.0) -> None:
        self.url = url
        self.token = token
        self.socket_path = socket_path
        self.timeout = timeout
        self.session: str | None = None

    @classmethod
    def discover(cls, url: str | None = None, token: str | None = None, socket_path: str | None = None,
                 home: str | None = None) -> "Transport":
        token = resolve_client_token(token)
        if url or socket_path:
            return cls(url, token, socket_path)
        info = read_discovery(home)
        if not info:
            raise RuntimeError("no cereyan server is running (no server.json in the home directory); start `cereyan serve`")
        sock = info.get("socket")
        if sock and not token and os.path.exists(sock):
            return cls(None, None, sock)
        return cls(info.get("url") or f"http://{info['host']}:{info['port']}", token, None)

    def _connection(self) -> http.client.HTTPConnection:
        if self.socket_path:
            return _UnixConnection(self.socket_path, self.timeout)
        parts = urllib.parse.urlsplit(self.url or "")
        return http.client.HTTPConnection(parts.hostname or "127.0.0.1", parts.port or 80, timeout=self.timeout)

    def send(self, message: dict) -> dict | None:
        """POST one message; returns the JSON reply, or None for a notification."""
        headers = {"content-type": "application/json", "accept": "application/json"}
        if self.token:
            headers["authorization"] = f"Bearer {self.token}"
        if self.session:
            headers[SESSION_HEADER] = self.session
        conn = self._connection()
        try:
            conn.request("POST", "/mcp", body=json.dumps(message).encode("utf-8"), headers=headers)
            resp = conn.getresponse()
            raw = resp.read()
            sid = resp.getheader(SESSION_HEADER)
            if sid:
                self.session = sid
            if resp.status == 202 or not raw:
                if resp.status >= 400:
                    raise RuntimeError(f"server answered {resp.status}")
                return None
            body = json.loads(raw.decode("utf-8"))
            if resp.status >= 400 and "error" not in body:
                raise RuntimeError(body.get("error") if isinstance(body, dict) else f"server answered {resp.status}")
            return body
        finally:
            conn.close()


def serve_stdio(transport: Transport, stdin=None, stdout=None) -> int:
    """Read newline-delimited JSON-RPC from stdin, answer on stdout, until EOF."""
    stdin = stdin or sys.stdin
    stdout = stdout or sys.stdout
    for line in stdin:
        line = line.strip()
        if not line:
            continue
        try:
            message = json.loads(line)
        except ValueError as exc:
            _write(stdout, {"jsonrpc": "2.0", "id": None, "error": {"code": -32700, "message": f"parse error: {exc}"}})
            continue
        msg_id = message.get("id") if isinstance(message, dict) else None
        try:
            reply = transport.send(message)
        except Exception as exc:  # noqa: BLE001 - any transport failure becomes a JSON-RPC error
            if msg_id is not None:
                _write(stdout, {"jsonrpc": "2.0", "id": msg_id, "error": {"code": -32000, "message": f"cereyan server unreachable: {exc}"}})
            continue
        if reply is not None and msg_id is not None:
            _write(stdout, reply)
    return 0


def _write(stdout, payload: Any) -> None:
    stdout.write(json.dumps(payload, separators=(",", ":")) + "\n")
    stdout.flush()


def main(url: str | None = None, token: str | None = None, socket_path: str | None = None) -> int:
    try:
        transport = Transport.discover(url, token, socket_path, home=_core.Store.resolve_home(None))
    except RuntimeError as exc:
        # Stay alive so the host sees a JSON-RPC error per request instead of a dead process.
        transport = _Unreachable(str(exc))
    return serve_stdio(transport)


class _Unreachable(Transport):
    def __init__(self, reason: str) -> None:
        super().__init__(None, None, None)
        self.reason = reason

    def send(self, message: dict) -> dict | None:
        raise RuntimeError(self.reason)
