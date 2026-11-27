#!/bin/sh
# Replays Claude CLI auth protocol shapes only. No browser, network, or credential store.
printf '%s\n' "$$" "$@" "$CLAUDE_CONFIG_DIR" "$PWD" > "$RECORD"
case "$*" in
    'auth status --json')
        [ "$NO_COLOR" = 1 ] && [ -z "${ANTHROPIC_API_KEY+x}${BROWSER+x}${CLAUDE_SECURESTORAGE_CONFIG_DIR+x}${CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR+x}" ] || exit 2
        printf '%s\n' '{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","email":"fixture@example.test","ignored":"extra"}'
        exit 0 ;;
    'auth logout')
        exit "${LOGOUT_EXIT:-0}" ;;
    'auth login --claudeai') ;;
    *) exit 2 ;;
esac

auto='https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A54545%2Fcallback&state=fixture'
manual="${MANUAL_URL:-https://claude.ai/oauth/authorize?scope=user%3Aprofile+user%3Ainference&state=fixture}"
case "$MODE" in
    watcher)
        "$BROWSER" "$auto"
        IFS= read -r code || exit 2 ;;
    hang)
        trap '' TERM ;;
    closed-stdin)
        /usr/bin/mkfifo "$HOME/blocked"
        exec 0<&- ;;
    browser|inline|terminal|foreign|wrong-prefix|exit-one|paste)
        "$BROWSER" "$auto" ;;
esac

case "$MODE" in
    terminal) printf "If the browser didn't open, visit: \033]8;;%s\033\\\\%s\033]8;;\033\\\\\r\n" "$manual" "$manual" ;;
    paste) printf "If the browser didn't open, visit: \033]8;;%s\007%s\033]8;;\007\n" "$manual" "$manual" ;;
    *) printf "If the browser didn't open, visit: %s\n" "$manual" ;;
esac
case "$MODE" in
    hang) IFS= read -r ignored; exit 2 ;;
    closed-stdin) exec /usr/bin/cat "$HOME/blocked" ;;
    paste|silent)
        IFS= read -r code || exit 2
        printf '%s\n' "$code" >> "$RECORD"
        [ "$MODE" = paste ] || exit 0 ;;
    org) printf '%s\n' 'OAuth login failed: cli_auth_login_org_not_allowed' >&2; exit 1 ;;
    provider) printf '%s\n' 'OAuth login failed: cli_auth_login_provider_not_allowed' >&2; exit 1 ;;
    oauth) printf '%s\n' 'OAuth login failed (private fixture detail)' >&2; exit 1 ;;
esac
case "$MODE" in
    inline) printf '%s\n' 'Paste code here if prompted > Login successful.' ;;
    wrong-prefix) printf '%s\n' 'Error > Login successful.' ;;
    terminal) printf '\033[32mLogin successful.\033[0m\r\n' ;;
    *) printf '%s\n' 'Login successful.' ;;
esac
[ "$MODE" != exit-one ]
