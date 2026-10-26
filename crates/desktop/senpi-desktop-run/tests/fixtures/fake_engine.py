#!/usr/bin/env python3
import json
import os
import sys

if "--stdio" not in sys.argv:
    sys.stderr.write("fake-engine: expected --stdio\n")
    sys.exit(2)

mode = os.environ.get("FAKE_ENGINE_MODE", "normal")
abi = os.environ.get("FAKE_ENGINE_ABI", "senpi-desktop/1")
stalled = os.environ.get("FAKE_ENGINE_STALL")
input_error = os.environ.get("FAKE_ENGINE_INPUT_ERROR")
is_desktop = os.environ.get("FAKE_ENGINE_DESKTOP") == "1"
pid = os.getpid()
resume_token = f"token-{pid}"

PNG_1X1 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg=="

DISPLAYS = [
    {
        "id": "1",
        "name": "Built-in",
        "x": 0,
        "y": 0,
        "width": 1440,
        "height": 900,
        "scale": 2.0,
        "pixelX": 0,
        "pixelY": 0,
        "pixelWidth": 2880,
        "pixelHeight": 1800,
        "isPrimary": True,
    }
]

WINDOWS = [
    {"id": "101", "title": "main.ts", "app": "Code", "pid": 42, "x": 0, "y": 25, "width": 1200, "height": 800, "focused": True},
    {"id": "202", "title": "Inbox", "app": "Mail", "pid": 43, "x": 100, "y": 100, "width": 900, "height": 700, "focused": False},
]

BUTTON = {
    "ref": "e1",
    "role": "button",
    "nativeRole": "AXButton",
    "title": "Run",
    "enabled": True,
    "focused": False,
    "x": 10,
    "y": 20,
    "width": 30,
    "height": 40,
    "actions": ["press"],
    "childCount": 0,
}

POINTER_METHODS = {"click", "moveMouse", "drag", "scroll"}
NULL_METHODS = {"typeText", "keyChord", "raiseWindow", "ax.perform", "ax.setValue", "ax.focus", "ax.click"}
INPUT_ERROR_RPC = {
    "PermissionDenied": -32000,
    "StopPathUnavailable": -32014,
    "Suspended": -32015,
    "ScreenLocked": -32016,
}

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
frames = {}
clipboard = ""

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

def fail(req_id, rpc_code, code, message, hint=None, permission=None):
    err_data = {"code": code, "hint": hint}
    if permission:
        err_data["permission"] = permission
    send({"id": req_id, "error": {"code": rpc_code, "message": message, "data": err_data}})

def audit(action, target, frame_id, code):
    send({
        "method": "audit",
        "params": {
            "action": action,
            "target": target,
            "delivery": "background",
            "frameId": frame_id,
            "code": code,
            "durationMs": 4,
        },
    })

def capture_target(target, frame_id):
    common = {"target": target, "frameId": frame_id, "scale": 1.0, "note": None}
    if target == "desktop":
        size = {"width": 1280, "height": 800, "sourceWidth": 2880, "sourceHeight": 1800}
        return {**common, **size, "mode": "inline-png", "data": PNG_1X1, "mimeType": "image/png", "artifactPath": None}
    w = next((candidate for candidate in WINDOWS if candidate["id"] == target), None)
    size = {"width": w["width"], "height": w["height"], "sourceWidth": w["width"], "sourceHeight": w["height"]}
    if target == "101":
        return {**common, **size, "mode": "inline-png", "data": PNG_1X1, "mimeType": "image/png", "artifactPath": None}
    note = "capture exceeds the inline byte budget; open the artifact instead"
    return {**common, **size, "mode": "artifact-only", "artifactPath": f"/tmp/fake-captures/{frame_id}.png", "note": note}

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
            fail(req_id, -32000, "InvalidTarget", "wrong resume token")
        else:
            suspended = False
            send({"method": "stopPath.changed", "params": get_status()})
            reply(req_id, get_status())
    elif method == "$/cancel":
        target_id = params.get("id")
        if target_id in cancellable:
            cancellable.remove(target_id)
            fail(target_id, -32000, "Cancelled", "request was cancelled by $/cancel")
    elif is_desktop and stalled == method:
        if req_id is not None:
            cancellable.add(req_id)
    elif is_desktop:
        target = params.get("target")
        if input_error is not None and (method in POINTER_METHODS or method in ("typeText", "keyChord")):
            rpc_c = INPUT_ERROR_RPC.get(input_error, -32013)
            perm = None
            if input_error == "PermissionDenied":
                perm = {
                    "permission": "accessibility",
                    "settingsUrl": "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
                    "app": "QA App",
                    "relaunchRequired": True,
                }
            audit(method, target or "desktop", None, input_error)
            fail(req_id, rpc_c, input_error, f"input refused: {input_error}", permission=perm)
        elif method in POINTER_METHODS:
            frame_id = frames.get(target)
            if frame_id is None:
                audit(method, target, None, "InvalidCoordinateFrame")
                fail(req_id, -32007, "InvalidCoordinateFrame", f"no capture of {target} yet", hint=f"capture {target} first")
            else:
                audit(method, target, frame_id, None)
                reply(req_id, None)
        elif method in NULL_METHODS:
            reply(req_id, None)
        elif method == "displays":
            reply(req_id, DISPLAYS)
        elif method == "windows":
            reply(req_id, {"not": "windows"} if os.environ.get("FAKE_ENGINE_MALFORMED_WINDOWS") == "1" else WINDOWS)
        elif method == "capture":
            if target != "desktop" and not any(w["id"] == target for w in WINDOWS):
                fail(req_id, -32004, "WindowNotFound", f"no window {target}")
            else:
                frame_id = f"{target}-{len(frames) + 1}"
                frames[target] = frame_id
                reply(req_id, capture_target(target, frame_id))
        elif method == "clipboard.read":
            reply(req_id, {"text": clipboard})
        elif method == "clipboard.write":
            clipboard = params.get("text", "")
            reply(req_id, None)
        elif method == "ax.snapshot":
            reply(req_id, {"text": "button Run [ref=e1]", "nodeCount": 1, "truncated": False})
        elif method in ("ax.query", "ax.children"):
            reply(req_id, [BUTTON])
        elif method in ("ax.node", "ax.elementAt", "ax.focused"):
            reply(req_id, BUTTON)
        elif method == "ax.parent":
            reply(req_id, None)
        elif method == "ax.attributes":
            reply(req_id, [["AXRole", "AXButton"]])
        else:
            reply(req_id, {"method": method, "params": params, "pid": pid})
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
