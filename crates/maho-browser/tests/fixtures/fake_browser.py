#!/usr/bin/env python3
"""A stand-in Chromium for maho-browser tests: speaks CDP over the --remote-debugging-pipe file
descriptors (requests on fd 3, NUL-delimited JSON; responses and events on fd 4) and answers the
commands the crate sends with canned results. Every request is appended as one JSON line to
$FAKE_BROWSER_LOG. Written for these tests; it shares no code with any browser.
"""
import base64
import json
import os
import sys
import zlib
import struct

LOG = os.environ.get("FAKE_BROWSER_LOG")
if "--remote-debugging-pipe" not in sys.argv:
    sys.stderr.write("fake browser: expected --remote-debugging-pipe\n")
    sys.exit(2)
# Noise on stdout must not reach the CDP stream.
sys.stdout.write("fake browser starting on stdout\n")
sys.stdout.flush()
sys.stderr.write("fake browser starting on stderr\n")
sys.stderr.flush()

inp = os.fdopen(3, "rb", buffering=0)
out = os.fdopen(4, "wb", buffering=0)


def png(width, height):
    raw = b"".join(b"\x00" + b"\xff\xff\xff" * width for _ in range(height))

    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    ihdr = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b"")


def send(message):
    out.write(json.dumps(message).encode() + b"\0")


world_installed = False
navigations = 0


def evaluate(expression):
    global world_installed
    if expression.startswith("typeof globalThis.__mahoPage"):
        return {"result": {"type": "boolean", "value": world_installed}}
    if "globalThis.__mahoPage = {" in expression:
        world_installed = True
        return {"result": {"type": "undefined"}}
    if "takeSnapshot(" in expression:
        tree = '- heading "Fake Page" [level=1]\n- button "Go" [ref=e1]\n- textbox "Name" [ref=e2]'
        return {"result": {"type": "object", "value": {"tree": tree, "refs": 2, "url": "https://fake.test/", "title": "Fake Page"}}}
    if "waitForReady" in expression:
        if '"e404"' in expression:
            return {"result": {"type": "object", "value": {"error": 'Ref "e404" is stale or unknown'}}}
        return {"result": {"type": "object", "value": {"x": 40, "y": 30, "width": 20, "height": 10, "tag": "button"}}}
    if expression == "[location.href, document.title]":
        return {"result": {"type": "object", "value": ["https://fake.test/", "Fake Page"]}}
    if expression == "throw new Error('nope')":
        return {"result": {"type": "object"}, "exceptionDetails": {"text": "Uncaught", "exception": {"description": "Error: nope"}}}
    if expression == "1 + 1":
        return {"result": {"type": "number", "value": 2}}
    return {"result": {"type": "undefined"}}


buffer = b""
while True:
    chunk = inp.read(65536)
    if not chunk:
        break
    buffer += chunk
    while b"\0" in buffer:
        raw, buffer = buffer.split(b"\0", 1)
        if not raw:
            continue
        message = json.loads(raw)
        if LOG:
            with open(LOG, "a") as log:
                log.write(json.dumps(message) + "\n")
        mid = message.get("id")
        method = message.get("method")
        params = message.get("params") or {}
        session = message.get("sessionId")
        result = {}
        if method == "Browser.getVersion":
            result = {"product": "FakeChrome/1.0", "protocolVersion": "1.3"}
        elif method == "Target.createTarget":
            result = {"targetId": "T1"}
        elif method == "Target.attachToTarget":
            result = {"sessionId": "S1"}
        elif method == "Page.getFrameTree":
            result = {"frameTree": {"frame": {"id": "F1"}}}
        elif method == "Target.getTargets":
            result = {"targetInfos": [{"targetId": "T1", "type": "page", "url": "https://fake.test/", "title": "Fake Page"},
                                      {"targetId": "W1", "type": "service_worker", "url": "", "title": ""}]}
        elif method == "Page.navigate":
            navigations += 1
            world_installed = False
            if "unreachable" in params.get("url", ""):
                result = {"frameId": "F1", "errorText": "net::ERR_NAME_NOT_RESOLVED"}
            else:
                loader = f"L{navigations}"
                result = {"frameId": "F1", "loaderId": loader}
                send({"id": mid, "result": result})
                send({"method": "Page.lifecycleEvent", "sessionId": session,
                      "params": {"frameId": "F1", "loaderId": loader, "name": "DOMContentLoaded"}})
                send({"method": "Page.lifecycleEvent", "sessionId": session,
                      "params": {"frameId": "F1", "loaderId": loader, "name": "load"}})
                continue
        elif method == "Runtime.evaluate":
            result = evaluate(params.get("expression", ""))
        elif method == "Page.getLayoutMetrics":
            result = {"cssLayoutViewport": {"clientWidth": 800, "clientHeight": 600},
                      "cssContentSize": {"width": 800, "height": 1600}}
        elif method == "Page.captureScreenshot":
            clip = params.get("clip") or {"width": 8, "height": 6}
            result = {"data": base64.b64encode(png(int(clip["width"]) // 100 or 8, int(clip["height"]) // 100 or 6)).decode()}
        elif method == "Browser.close":
            send({"id": mid, "result": {}})
            sys.exit(0)
        send({"id": mid, "result": result})
