#!/bin/sh
set -eu

case "${1:-} ${2:-}" in
  'auth login')
    test "$3" = --claudeai
    test "$CLAUDE_CONFIG_DIR" != "$HOME/.claude"
    test -z "${CLAUDE_CODE_OAUTH_TOKEN:-}${ANTHROPIC_API_KEY:-}"
    if [ -f "$HOME/use-manual" ]; then
      if [ -f "$HOME/save-before-code" ]; then touch "$CLAUDE_CONFIG_DIR/login"; fi
      printf '%s\n' "If the browser didn't open, visit: https://claude.ai/oauth/authorize?fixture=manual"
      IFS= read -r code
      test "$code" = fixture-code
    fi
    touch "$CLAUDE_CONFIG_DIR/login"
    printf '%s\n' 'Login successful.'
    ;;
  'auth status')
    test "$3" = --json
    if [ -f "$HOME/reject-status" ]; then
      printf '%s\n' '{"loggedIn":true,"authMethod":"api_key","apiProvider":"firstParty"}'
    elif [ -f "$CLAUDE_CONFIG_DIR/login" ]; then
      printf '%s\n' '{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","subscriptionType":"max"}'
    else
      printf '%s\n' '{"loggedIn":false}'
      exit 1
    fi
    ;;
  'auth logout')
    test -f "$CLAUDE_CONFIG_DIR/login"
    rm "$CLAUDE_CONFIG_DIR/login"
    printf '%s\n' "${CLAUDE_CONFIG_DIR##*/}" >> "$HOME/logged-out"
    ;;
  *)
    exec python3 -c '
import json, os, pathlib, sys
account_dir = pathlib.Path(os.environ["CLAUDE_CONFIG_DIR"])
assert (account_dir / "login").is_file()
with open(os.environ["FIXTURE_AUTH"]) as f:
    slots = json.load(f)["anthropic-subscription"]["accounts"]
assert any(s["dirId"] == account_dir.name and s["store"] == "claude-cli" for s in slots)
init = json.loads(sys.stdin.readline())
assert init["request"]["subtype"] == "initialize"
print(json.dumps({"type": "control_response", "response": {
    "subtype": "success", "request_id": init["request_id"],
    "response": {"account": {"apiProvider": "firstParty", "subscriptionType": "max"}}
}}), flush=True)
prompt = json.loads(sys.stdin.readline())
assert prompt["message"]["content"] == "Reply with OK"
if (pathlib.Path(os.environ["HOME"]) / "reject-probe").exists():
    print(json.dumps({"type": "result", "subtype": "error_during_execution", "is_error": True,
        "errors": ["authentication_failed"], "api_error_status": 401}), flush=True)
else:
    print(json.dumps({"type": "result", "subtype": "success", "is_error": False}), flush=True)
'
    ;;
esac
