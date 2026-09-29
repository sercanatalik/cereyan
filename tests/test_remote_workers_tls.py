"""A worker reaching its server through a TLS-terminating proxy under a base path,
trusting the proxy's CA through SSL_CERT_FILE, as the same network with a corporate
CA would have it."""

from __future__ import annotations

import shutil
import subprocess
import sys
import time

import pytest

from cereyan.client import Client
from server_helpers import ServerProcess, free_port, own_group, stop_server
from test_remote_workers import PIPELINE, TOKEN, WorkerProcess, wait_for, worker_view

pytestmark = pytest.mark.skipif(shutil.which("openssl") is None, reason="needs the openssl command")

PROXY = '''
import asyncio, ssl, sys
listen, target, cert, key = int(sys.argv[1]), int(sys.argv[2]), sys.argv[3], sys.argv[4]
ctx = ssl.create_default_context(ssl.Purpose.CLIENT_AUTH)
ctx.load_cert_chain(cert, key)

async def pipe(r, w):
    try:
        while (data := await r.read(65536)):
            w.write(data)
            await w.drain()
    finally:
        w.close()

async def handle(r, w):
    ur, uw = await asyncio.open_connection("127.0.0.1", target)
    await asyncio.gather(pipe(r, uw), pipe(ur, w), return_exceptions=True)

async def main():
    srv = await asyncio.start_server(handle, "127.0.0.1", listen, ssl=ctx)
    async with srv:
        await srv.serve_forever()

asyncio.run(main())
'''


def make_ca(d):
    run = lambda *a: subprocess.run(a, cwd=d, check=True, capture_output=True)  # noqa: E731
    # A CA as real ones are made: Python 3.13 verifies strictly and refuses one
    # without key usage.
    run("openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", "ca.key", "-out", "ca.pem",
        "-days", "2", "-subj", "/CN=cereyan-test-ca",
        "-addext", "basicConstraints=critical,CA:TRUE",
        "-addext", "keyUsage=critical,keyCertSign,cRLSign",
        "-addext", "subjectKeyIdentifier=hash")
    run("openssl", "req", "-newkey", "rsa:2048", "-nodes", "-keyout", "srv.key", "-out", "srv.csr",
        "-subj", "/CN=localhost")
    (d / "san.ext").write_text(
        "subjectAltName=DNS:localhost,IP:127.0.0.1\n"
        "basicConstraints=critical,CA:FALSE\n"
        "keyUsage=critical,digitalSignature,keyEncipherment\n"
        "extendedKeyUsage=serverAuth\n"
        "authorityKeyIdentifier=keyid\n"
    )
    run("openssl", "x509", "-req", "-in", "srv.csr", "-CA", "ca.pem", "-CAkey", "ca.key", "-CAcreateserial",
        "-out", "srv.pem", "-days", "2", "-extfile", "san.ext")
    return d / "ca.pem", d / "srv.pem", d / "srv.key"


@pytest.fixture
def behind_proxy(isolated_home, tmp_path):
    from cereyan import engine

    project = tmp_path / "server-checkout"
    project.mkdir()
    (project / "pipeline.py").write_text(PIPELINE)
    ca, cert, key = make_ca(tmp_path)
    engine.close_store()
    server = ServerProcess(str(isolated_home), str(project), max_engines=1,
                           extra=["--token", TOKEN, "--base-path", "/cereyan"])
    server.client = Client(server.info["url"], token=TOKEN)
    port = free_port()
    (tmp_path / "proxy.py").write_text(PROXY)
    proxy = subprocess.Popen([sys.executable, str(tmp_path / "proxy.py"), str(port), str(server.info["url"]).rsplit(":", 1)[1].split("/")[0],
                              str(cert), str(key)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                             **own_group())
    time.sleep(0.5)
    url = f"https://localhost:{port}/cereyan"
    try:
        yield server, project, ca, url
    finally:
        stop_server(proxy, timeout=10)
        server.stop()


def test_a_worker_through_a_tls_proxy_with_a_base_path(behind_proxy, tmp_path):
    server, project, ca, url = behind_proxy
    # The test talks to the server directly; the worker goes through the proxy.
    client = server.client
    trusted = WorkerProcess(tmp_path, url, name="tls-1", source=project, env={"SSL_CERT_FILE": str(ca)})
    try:
        view = wait_for(lambda: (worker_view(server, "tls-1") or {}).get("state") == "online"
                        and worker_view(server, "tls-1"), what="tls-1 online")
        assert view["meta"]["connection"] == "TLS"
        gate = str(tmp_path / "gate")
        blocker = client.run("gated", gate=gate)
        server.wait_run(blocker["id"], until=lambda r: r["state"]["type"] == "Running")
        remote = client.run("gated", gate=gate, seconds=0.5)
        done = server.wait_run(remote["id"], timeout=60)
        (tmp_path / "gate").write_text("")
        assert done["state"]["type"] == "Completed", trusted.read_log()
        assert done["host"] == "tls-1"
    finally:
        trusted.stop()


def test_a_worker_without_the_ca_does_not_connect(behind_proxy, tmp_path):
    server, project, ca, url = behind_proxy
    untrusted = WorkerProcess(tmp_path, url, name="no-ca", source=project, env={"SSL_CERT_FILE": ""})
    try:
        time.sleep(6)
        assert "certificate" in untrusted.read_log().lower()
        assert untrusted.proc.poll() is None, "it keeps retrying rather than exiting"
    finally:
        untrusted.stop()
