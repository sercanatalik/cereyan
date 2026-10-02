"""The worker's read-only status page: ``/``, ``status.json`` and ``healthz``.

The page is ``worker.html`` from the UI build embedded in ``_core``, the same
bundle ``cereyan serve`` serves, so it looks like the rest of the Cereyan UI. It
fetches ``status.json`` relatively, and the handler matches paths by their
last part, so the page works behind a reverse proxy under any prefix whether
or not the proxy strips it. Only ``GET`` and ``HEAD`` are answered.
"""

from __future__ import annotations

import json
import socket
import socketserver
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any, Callable
from urllib.parse import urlsplit

from . import _core


class _Server(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True
    status: Callable[[], dict[str, Any]]
    health: Callable[[], tuple[bool, dict[str, Any]]]

    def server_bind(self) -> None:
        # `HTTPServer.server_bind` resolves `socket.getfqdn(host)` for a
        # `server_name` nothing here reads. A slow reverse lookup (macOS often
        # takes tens of seconds) then stalls the worker before it says anything.
        socketserver.TCPServer.server_bind(self)
        host, port = self.server_address[:2]
        self.server_name = str(host)
        self.server_port = port


class _Server6(_Server):
    address_family = socket.AF_INET6


class _Handler(BaseHTTPRequestHandler):
    server: _Server
    server_version = "cereyan-worker"

    def log_message(self, format: str, *args: Any) -> None:  # noqa: A002 - the base class's name
        pass

    def _send(self, status: int, body: bytes, content_type: str, *, head: bool, cache: str = "no-store") -> None:
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", cache)
        self.send_header("X-Content-Type-Options", "nosniff")
        self.end_headers()
        if not head:
            self.wfile.write(body)

    def _json(self, status: int, value: Any, *, head: bool) -> None:
        self._send(status, json.dumps(value).encode(), "application/json", head=head)

    def _serve(self, head: bool) -> None:
        path = urlsplit(self.path).path
        last = path.rsplit("/", 1)[-1]
        if last == "status.json":
            self._json(200, self.server.status(), head=head)
        elif last == "healthz":
            ok, body = self.server.health()
            self._json(200 if ok else 503, body, head=head)
        elif "/assets/" in path or last == "favicon.svg":
            name = "assets/" + last if "/assets/" in path else last
            found = _core.ui_asset(name)
            if found is None:
                self._send(404, b"not found", "text/plain; charset=utf-8", head=head)
            else:
                data, mime = found
                cache = "public, max-age=31536000, immutable" if name.startswith("assets/") else "no-cache"
                self._send(200, data, mime, head=head, cache=cache)
        elif last in ("", "index.html"):
            found = _core.ui_asset("worker.html")
            if found is None:
                self._send(500, b"this build has no worker page", "text/plain; charset=utf-8", head=head)
            else:
                self._send(200, found[0], "text/html; charset=utf-8", head=head, cache="no-cache")
        else:
            self._send(404, b"not found", "text/plain; charset=utf-8", head=head)

    def do_GET(self) -> None:  # noqa: N802 - the base class's name
        self._serve(head=False)

    def do_HEAD(self) -> None:  # noqa: N802 - the base class's name
        self._serve(head=True)

    def _refuse(self) -> None:
        self.send_response(405)
        self.send_header("Allow", "GET, HEAD")
        self.send_header("Content-Length", "0")
        self.end_headers()

    do_POST = do_PUT = do_PATCH = do_DELETE = do_OPTIONS = _refuse  # noqa: N815 - the base class's names


class StatusServer:
    """The listener: bound on construction, served from a daemon thread by `start`."""

    def __init__(self, host: str, port: int, status: Callable[[], dict[str, Any]],
                 health: Callable[[], tuple[bool, dict[str, Any]]]) -> None:
        cls = _Server6 if ":" in host else _Server
        self._httpd = cls((host, port), _Handler)
        self._httpd.status = status
        self._httpd.health = health
        self.host = host
        self.port = int(self._httpd.server_address[1])
        self._thread: threading.Thread | None = None

    @property
    def url(self) -> str:
        """Where the page is, with the hostname in place of a wildcard address."""
        host = socket.gethostname() if self.host in ("0.0.0.0", "::", "") else self.host
        if ":" in host:
            host = f"[{host}]"
        return f"http://{host}:{self.port}"

    def start(self) -> None:
        self._thread = threading.Thread(target=self._httpd.serve_forever, name="cereyan-worker-status", daemon=True)
        self._thread.start()

    def close(self) -> None:
        if self._thread is not None:
            self._httpd.shutdown()
        self._httpd.server_close()
