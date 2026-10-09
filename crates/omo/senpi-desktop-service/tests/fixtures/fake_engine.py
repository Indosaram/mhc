#!/usr/bin/env python3
import json
import os
import sys

if "--stdio" not in sys.argv:
    sys.stderr.write("fake-engine: expected --stdio\n")
    sys.exit(2)

mode = os.environ.get("FAKE_ENGINE_MODE", "normal")
abi = os.environ.get("FAKE_ENGINE_ABI", "senpi-desktop/1")
pid = os.getpid()
resume_token = f"token-{pid}"

capabilities = {
    "backend": "fake",
    "displayServer": None,
    "capture": True,
    "input": True,
    "ax": True,
    "backgroundWindowInput": True,
    "deliveryModes": ["background", "foreground"],
    "capturePermission": "granted",
    "inputPermission": "granted",
    "axPermission": "granted",
    "displayCount": 1,
    "focusGuard": True,
    "stopPath": "global",
    "stopReason": None,
    "integrityLevel": None,
    "screenLocked": False,
}

suspended = False

def get_status():
    return {
        "suspended": suspended,
        "globalLive": True,
        "hostRelayLive": True,
        "heartbeatFresh": True,
        "stopPath": "global",
        "reason": None,
    }

def send(msg):
    payload = {"jsonrpc": "2.0", **msg}
    sys.stdout.write(json.dumps(payload) + "\n")
    sys.stdout.flush()

def reply(req_id, result):
    send({"id": req_id, "result": result})

def fail(req_id, code, message):
    send({"id": req_id, "error": {"code": -32000, "message": message, "data": {"code": code, "hint": None}}})

held = []
cancellable = set()

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    send({"method": "engine.log", "params": {"level": "debug", "message": f"recv {line}"}})
    if mode == "silent":
        continue
    try:
        req = json.loads(line)
    except Exception:
        continue
    req_id = req.get("id")
    method = req.get("method")
    params = req.get("params") or {}

    if method == "engine.hello":
        reply(req_id, {"protocolVersion": "1", "engineVersion": "0.0.0-fake", "buildSha": "fake", "abi": abi})
    elif method == "session.open":
        reply(req_id, {"capabilities": capabilities, "resumeToken": resume_token})
    elif method in ("session.close", "stopPath.heartbeat"):
        reply(req_id, None)
    elif method == "capabilities":
        reply(req_id, capabilities)
    elif method in ("stopPath.start", "stopPath.status"):
        reply(req_id, get_status())
    elif method == "stopPath.stop":
        suspended = True
        send({"method": "stopPath.changed", "params": get_status()})
        reply(req_id, get_status())
    elif method == "stopPath.resume":
        if params.get("token") != resume_token:
            fail(req_id, "InvalidTarget", "wrong resume token")
        else:
            suspended = False
            send({"method": "stopPath.changed", "params": get_status()})
            reply(req_id, get_status())
    elif method == "$/cancel":
        target_id = params.get("id")
        if target_id in cancellable:
            cancellable.remove(target_id)
            fail(target_id, "Cancelled", "request was cancelled by $/cancel")
    else:
        fake = params.get("fake")
        if fake == "hold":
            if req_id is not None:
                held.append(req_id)
        elif fake == "release":
            reply(req_id, {"method": method, "params": params, "pid": pid})
            for h_id in held:
                reply(h_id, {"method": "held", "pid": pid})
            held.clear()
        elif fake == "hang":
            pass
        elif fake == "cancellable":
            if req_id is not None:
                cancellable.add(req_id)
        elif fake == "notify":
            send({"method": "audit", "params": {"action": method, "target": "screen", "delivery": "background", "durationMs": 3}})
            send({"method": "stopPath.changed", "params": get_status()})
            reply(req_id, {"method": method, "params": params, "pid": pid})
        elif fake == "exit":
            sys.exit(7)
        else:
            reply(req_id, {"method": method, "params": params, "pid": pid})
