#!/bin/sh
# init-deep managed block in git's exclude file.
#   local_mode.sh add      # local mode: append the managed block once (idempotent)
#   local_mode.sh remove   # local -> committed: delete the block, marker to marker inclusive
# Never touches lines outside the block. Portable across GNU and BSD tools; leaves no backup file.
set -eu

BEGIN='# >>> mhc init-deep local (managed) >>>'
END='# <<< mhc init-deep local (managed) <<<'

exclude=$(git rev-parse --git-path info/exclude)   # resolves correctly inside worktrees
mkdir -p "$(dirname "$exclude")"
[ -f "$exclude" ] || : > "$exclude"

case "${1:-}" in
  add)
    if grep -qxF "$BEGIN" "$exclude"; then
      echo "managed block already present in $exclude"
      exit 0
    fi
    # keep the user's last line intact if it has no trailing newline
    if [ -s "$exclude" ] && [ "$(tail -c 1 "$exclude" | od -An -c | tr -d ' ')" != '\n' ]; then
      printf '\n' >> "$exclude"
    fi
    {
      printf '%s\n' "$BEGIN"
      printf '%s\n' '# Managed by mhc init-deep. Do not edit by hand; switch modes through init-deep.'
      printf '%s\n' '/.mhc/init-deep.json'
      printf '%s\n' 'AGENTS.md'
      printf '%s\n' "$END"
    } >> "$exclude"
    echo "managed block added to $exclude"
    ;;
  remove)
    tmp="$exclude.init-deep.$$"
    # BRE: '>' '<' '(' ')' are literal here on both GNU and BSD sed
    sed '/^# >>> mhc init-deep local (managed) >>>$/,/^# <<< mhc init-deep local (managed) <<<$/d' "$exclude" > "$tmp"
    mv "$tmp" "$exclude"
    echo "managed block removed from $exclude"
    ;;
  *)
    echo "usage: $0 add|remove" >&2
    exit 2
    ;;
esac
