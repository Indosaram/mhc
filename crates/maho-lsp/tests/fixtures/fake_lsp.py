#!/usr/bin/env python3
"""Minimal stdio language server for maho-lsp tests (written for this crate)."""
import json
import os
import sys

docs = {}
inits = 0


def read():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.decode().strip()
        if not line:
            break
        name, _, value = line.partition(":")
        if name.lower() == "content-length":
            length = int(value)
    return json.loads(sys.stdin.buffer.read(length))


def send(message):
    body = json.dumps(message).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
    sys.stdout.buffer.flush()


def rng(line, start, end):
    return {"start": {"line": line, "character": start}, "end": {"line": line, "character": end}}


def handle(method, params):
    global inits
    uri = params.get("textDocument", {}).get("uri") if isinstance(params, dict) else None
    if method == "initialize":
        inits += 1
        return {"capabilities": {}, "serverInfo": {"name": "fake", "version": "%d:%d" % (os.getpid(), inits)}}
    if method == "textDocument/diagnostic":
        text = docs.get(uri, "")
        if "CRASH" in text:
            sys.exit(3)
        items = []
        for number, line in enumerate(text.split("\n")):
            if "ERROR" in line:
                items.append({"range": rng(number, line.index("ERROR"), line.index("ERROR") + 5), "severity": 1, "source": "fake", "message": "bad token"})
            if "WARN" in line:
                items.append({"range": rng(number, 0, 1), "severity": 2, "message": "careful"})
        return {"kind": "full", "items": items}
    if method == "textDocument/definition":
        return [{"uri": uri, "range": rng(0, 4, 7)}]
    if method == "textDocument/references":
        return [{"uri": uri, "range": rng(0, 4, 7)}, {"uri": uri, "range": rng(1, 0, 3)}]
    if method == "textDocument/documentSymbol":
        return [{"name": "Outer", "kind": 5, "range": rng(0, 0, 9), "selectionRange": rng(0, 0, 5), "children": [{"name": "inner", "kind": 6, "range": rng(1, 0, 3), "selectionRange": rng(1, 0, 3)}]}]
    if method == "workspace/symbol":
        return [{"name": params.get("query"), "kind": 12, "location": {"uri": "file:///w/x.fake", "range": rng(2, 1, 4)}}]
    if method == "textDocument/prepareRename":
        return {"range": rng(0, 4, 7), "placeholder": "foo"}
    if method == "textDocument/rename":
        return {"changes": {uri: [{"range": rng(0, 4, 7), "newText": params["newName"]}]}}
    if method == "shutdown":
        return None
    raise KeyError(method)


while True:
    message = read()
    if message is None:
        break
    method = message.get("method")
    params = message.get("params") or {}
    if "id" in message and method:
        try:
            send({"jsonrpc": "2.0", "id": message["id"], "result": handle(method, params)})
        except KeyError:
            send({"jsonrpc": "2.0", "id": message["id"], "error": {"code": -32601, "message": "Method not found"}})
    elif method == "textDocument/didOpen":
        docs[params["textDocument"]["uri"]] = params["textDocument"]["text"]
    elif method == "textDocument/didChange":
        docs[params["textDocument"]["uri"]] = params["contentChanges"][-1]["text"]
    elif method == "textDocument/didClose":
        docs.pop(params["textDocument"]["uri"], None)
    elif method == "exit":
        break
