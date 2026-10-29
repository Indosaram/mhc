//! Git hook scripts installed into memory repositories.

/// Descriptor for a secret scanning pattern used in the pre-commit hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookSecretPattern {
    pub class: &'static str,
    pub ere: &'static str,
    pub exclude: Option<&'static str>,
}

pub const HOOK_SECRET_PATTERNS: &[HookSecretPattern] = &[
    HookSecretPattern {
        class: "aws_access_key",
        ere: "(^|[^A-Za-z0-9_])AKIA[0-9A-Z]{16}([^A-Za-z0-9_]|$)",
        exclude: None,
    },
    HookSecretPattern {
        class: "credential_assignment",
        ere: "(^|[^A-Za-z0-9_])([Bb][Ee][Aa][Rr][Ee][Rr]|[Tt][Oo][Kk][Ee][Nn]|[Aa][Pp][Ii][-_]?[Kk][Ee][Yy]|[Ss][Ee][Cc][Rr][Ee][Tt]|[Pp][Aa][Ss][Ss][Ww][Oo][Rr][Dd]|[Pp][Aa][Ss][Ss][Ww][Dd]|[Pp][Ww][Dd])[[:space:]]*[=:][[:space:]]*[^[:space:]]{1,255}",
        exclude: Some("^[^A-Za-z0-9_]?[A-Za-z_-]+[[:space:]]*[=:][[:space:]]*(process\\.env\\.[A-Za-z_][A-Za-z0-9_]*|os\\.environ(\\.get\\(|\\[)?(\"[A-Za-z0-9_]*\")?[])]*|env\\.[A-Za-z_][A-Za-z0-9_]*|\\$\\{[A-Za-z_][A-Za-z0-9_]*\\}|\\$[A-Za-z_][A-Za-z0-9_]*|\\{\\{([^}]*\\}\\})?|\\[(redacted|filtered)\\]|(redacted|filtered)|(bearer|changeme|placeholder|example|none|null|undefined|true|false))[],<>})\"]*$"),
    },
    HookSecretPattern {
        class: "authorization_header",
        ere: "(^|[^A-Za-z0-9_])[Aa][Uu][Tt][Hh][Oo][Rr][Ii][Zz][Aa][Tt][Ii][Oo][Nn][[:space:]]*:[[:space:]]*[Bb][Ee][Aa][Rr][Ee][Rr][[:space:]]+[^[:space:]]{1,255}",
        exclude: Some("^[^A-Za-z0-9_]?authorization[[:space:]]*:[[:space:]]*bearer[[:space:]]+([]<>,})\"]|(process\\.env\\.[A-Za-z_][A-Za-z0-9_]*|os\\.environ(\\.get\\(|\\[)?(\"[A-Za-z0-9_]*\")?[])]*|env\\.[A-Za-z_][A-Za-z0-9_]*|\\$\\{[A-Za-z_][A-Za-z0-9_]*\\}|\\$[A-Za-z_][A-Za-z0-9_]*|\\{\\{([^}]*\\}\\})?|\\[(redacted|filtered)\\]|(redacted|filtered)|(bearer|changeme|placeholder|example|none|null|undefined|true|false))[],<>})\"]*$)"),
    },
    HookSecretPattern {
        class: "openai_key",
        ere: "(^|[^A-Za-z0-9_])sk-(proj-)?[-_A-Za-z0-9]+([^A-Za-z0-9_-]|$)",
        exclude: None,
    },
    HookSecretPattern {
        class: "vendor_token",
        ere: "(^|[^A-Za-z0-9_])(ghp|github_pat|glpat|xox[baprs])[-_][-_A-Za-z0-9]+([^A-Za-z0-9_-]|$)",
        exclude: None,
    },
];

/// Pre-commit validator script for staged memory markdown.
pub const PRE_COMMIT_HOOK_SCRIPT: &str = r##"#!/bin/sh
# Validate frontmatter in staged memory .md files.
# Installed by omo memory-core. Do not edit by hand - regenerated on startup.

# provenance is the claim receipt the memory tool stamps (#10488); its value is always a quoted JSON string.
ALL_KNOWN_KEYS="description read_only limit kind aliases provenance"
TAB=$(printf '\t')

fm_value() {
  # $1 = file content, $2 = key. Prints the value of a top-level frontmatter
  # key, or nothing when the key is absent.
  closing=$(printf '%s\n' "$1" | tail -n +2 | grep -n '^---$' | head -1 | cut -d: -f1)
  [ -z "$closing" ] && return 0
  printf '%s\n' "$1" | tail -n +2 | head -n $((closing - 1)) |
    grep "^$2:" | head -1 | cut -d: -f2- | sed 's/^ *//;s/ *$//'
}

check_plain_scalar() {
  # $1 = file, $2 = key, $3 = value. A quoted value is the renderer's own
  # output; a plain value must be one that strict YAML reads back verbatim.
  case "$3" in
    \"*|\'*|"") return 0 ;;
    *": "*|*":"|*" #"*|"#"*|"["*|"]"*|"{"*|"}"*|"&"*|"*"*|"!"*|"|"*|">"*|"%"*|"@"*|"\`"*|","*|"?"*|"-"|"- "*)
      printf "  %s: '%s' is not a safe YAML plain scalar (quote it, or remove ': ' and ' #'): %s\n" "$1" "$2" "$3"
      ;;
  esac
}

check_keys() {
  # $1 = file, $2 = HEAD content, $3 = frontmatter, $4 = key policy
  # ("contract" = memory key allowlist, "skill" = any key). Prints one error
  # line per offending frontmatter key. Runs in a pipeline subshell, so it
  # must never rely on assignments leaking to the caller.
  printf '%s\n' "$3" | while IFS= read -r line; do
    [ -z "$line" ] && continue
    # Indented lines are YAML continuations of the previous key.
    case "$line" in " "*|"$TAB"*) continue ;; esac

    key=$(printf '%s\n' "$line" | cut -d: -f1 | tr -d ' ')
    value=$(printf '%s\n' "$line" | cut -d: -f2- | sed 's/^ *//;s/ *$//')

    if [ "$4" != skill ]; then
      known=false
      for k in $ALL_KNOWN_KEYS; do
        [ "$key" = "$k" ] && known=true && break
      done
      if [ "$known" = false ]; then
        printf "  %s: unknown frontmatter key '%s' (allowed: %s)\n" "$1" "$key" "$ALL_KNOWN_KEYS"
        continue
      fi
    fi

    if [ "$key" != description ] && [ "$key" != aliases ] && [ "$key" != read_only ]; then
      check_plain_scalar "$1" "$key" "$value"
    fi

    if [ "$key" = read_only ]; then
      if [ -n "$2" ]; then
        [ "$value" != "$(fm_value "$2" read_only)" ] &&
          printf "  %s: 'read_only' is a protected field and cannot be changed by the agent\n" "$1"
      else
        printf "  %s: 'read_only' is a protected field and cannot be set by the agent\n" "$1"
      fi
    fi

    # Deviation from letta: a YAML block scalar ('>' or '|', with optional
    # chomping or indent modifiers) is rejected. letta accepted the indicator
    # as the value, which silently stored '>' as the description.
    if [ "$key" = description ]; then
      case "$value" in
        "") printf "  %s: 'description' must not be empty\n" "$1" ;;
        [\>\|]*) printf "  %s: 'description' must be a non-empty single line\n" "$1" ;;
        *) check_plain_scalar "$1" description "$value" ;;
      esac
    fi
  done
}

check_file() {
  # $1 = file, $2 = key policy ("contract" or "skill")
  file="$1"
  policy="${2:-contract}"
  staged=$(git show ":$file")

  if [ "$(printf '%s\n' "$staged" | head -1)" != "---" ]; then
    printf '  %s: missing frontmatter (must start with ---)\n' "$file"
    return 0
  fi

  closing_line=$(printf '%s\n' "$staged" | tail -n +2 | grep -n '^---$' | head -1 | cut -d: -f1)
  if [ -z "$closing_line" ]; then
    printf '  %s: frontmatter opened but never closed (missing closing ---)\n' "$file"
    return 0
  fi

  head_content=$(git show "HEAD:$file" 2>/dev/null || true)
  if [ -n "$head_content" ] && [ "$(fm_value "$head_content" read_only)" = "true" ]; then
    printf '  %s: file is read_only and cannot be modified\n' "$file"
    return 0
  fi

  frontmatter=$(printf '%s\n' "$staged" | tail -n +2 | head -n $((closing_line - 1)))
  check_keys "$file" "$head_content" "$frontmatter" "$policy"

  if ! printf '%s\n' "$frontmatter" | grep -q '^description:'; then
    printf "  %s: missing required field 'description'\n" "$file"
  fi

  if [ -n "$head_content" ] && [ -n "$(fm_value "$head_content" read_only)" ] &&
    [ -z "$(fm_value "$staged" read_only)" ]; then
    printf "  %s: 'read_only' is a protected field and cannot be removed by the agent\n" "$file"
  fi
}

errors=$(
  # Skills must always be directories: skills/<name>/SKILL.md.
  git -c core.quotePath=false diff --cached --name-only --diff-filter=ACMR |
    grep -E '^(memory/)?skills/[^/]+\.md$' |
    while IFS= read -r skill_file; do
      printf '  %s: invalid skill path (skills must be folders). Use skills/<name>/SKILL.md\n' "$skill_file"
    done

  # Memory content keys are an allowlist under system/, reference/ and people/.
  git -c core.quotePath=false diff --cached --name-only --diff-filter=ACMR |
    grep -E '^(memory/)?(system|reference|people)/.*\.md$' |
    while IFS= read -r file; do
      check_file "$file" contract
    done

  # SKILL.md is read by the skill loader as strict YAML: same value rules,
  # description required, any key allowed.
  git -c core.quotePath=false diff --cached --name-only --diff-filter=ACMR |
    grep -E '^(memory/)?skills/.+/SKILL\.md$' |
    while IFS= read -r file; do
      check_file "$file" skill
    done
)

if [ -n "$errors" ]; then
  echo "Frontmatter validation failed:"
  printf '%s\n' "$errors"
  exit 1
fi


# Secret-like material backstop for hand commits (regex-only defense in depth;
# the authoritative gates scan with evasion normalisation before this runs).
# Content is read by blob id from the raw diff (renames shown as an add, so a
# moved file is scanned too), so no file name (quoted,
# non-ASCII or carrying shell metacharacters) decides what gets scanned; the
# name is only ever printed, as data, in git's own quoting.
secret_failed=$(
  git diff --cached --no-renames --diff-filter=AMT --raw --no-abbrev |
    while IFS= read -r line; do
      meta=${line%%"$TAB"*}
      p=${line#*"$TAB"}
      set -- $meta
      [ "$2" = "160000" ] && continue
      blob=$(git cat-file blob "$4" 2>/dev/null) || {
        printf 'memory pre-commit: %s could not be read for secret screening\n' "$p"
        continue
      }
      if printf '%s' "$blob" | grep -E -q -e '(^|[^A-Za-z0-9_])AKIA[0-9A-Z]{16}([^A-Za-z0-9_]|$)'; then
        printf 'memory pre-commit: %s contains secret-like content (aws_access_key)\n' "$p"
      fi
      if printf '%s' "$blob" | grep -E -o -e '(^|[^A-Za-z0-9_])([Bb][Ee][Aa][Rr][Ee][Rr]|[Tt][Oo][Kk][Ee][Nn]|[Aa][Pp][Ii][-_]?[Kk][Ee][Yy]|[Ss][Ee][Cc][Rr][Ee][Tt]|[Pp][Aa][Ss][Ss][Ww][Oo][Rr][Dd]|[Pp][Aa][Ss][Ss][Ww][Dd]|[Pp][Ww][Dd])[[:space:]]*[=:][[:space:]]*[^[:space:]]{1,255}' | grep -E -v -i -e '^[^A-Za-z0-9_]?[A-Za-z_-]+[[:space:]]*[=:][[:space:]]*(process\.env\.[A-Za-z_][A-Za-z0-9_]*|os\.environ(\.get\(|\[)?("[A-Za-z0-9_]*")?[]\)]*|env\.[A-Za-z_][A-Za-z0-9_]*|\$\{[A-Za-z_][A-Za-z0-9_]*\}|\$[A-Za-z_][A-Za-z0-9_]*|\{\{([^}]*\}\})?|\[(redacted|filtered)\]|(redacted|filtered)|(bearer|changeme|placeholder|example|none|null|undefined|true|false))[],<>})"]*$' | grep -q .; then
        printf 'memory pre-commit: %s contains secret-like content (credential_assignment)\n' "$p"
      fi
      if printf '%s' "$blob" | grep -E -o -e '(^|[^A-Za-z0-9_])[Aa][Uu][Tt][Hh][Oo][Rr][Ii][Zz][Aa][Tt][Ii][Oo][Nn][[:space:]]*:[[:space:]]*[Bb][Ee][Aa][Rr][Ee][Rr][[:space:]]+[^[:space:]]{1,255}' | grep -E -v -i -e '^[^A-Za-z0-9_]?authorization[[:space:]]*:[[:space:]]*bearer[[:space:]]+([]<>,})"]|(process\.env\.[A-Za-z_][A-Za-z0-9_]*|os\.environ(\.get\(|\[)?("[A-Za-z0-9_]*")?[]\)]*|env\.[A-Za-z_][A-Za-z0-9_]*|\$\{[A-Za-z_][A-Za-z0-9_]*\}|\$[A-Za-z_][A-Za-z0-9_]*|\{\{([^}]*\}\})?|\[(redacted|filtered)\]|(redacted|filtered)|(bearer|changeme|placeholder|example|none|null|undefined|true|false))[],<>})"]*$)' | grep -q .; then
        printf 'memory pre-commit: %s contains secret-like content (authorization_header)\n' "$p"
      fi
      if printf '%s' "$blob" | grep -E -q -e '(^|[^A-Za-z0-9_])sk-(proj-)?[-_A-Za-z0-9]+([^A-Za-z0-9_-]|$)'; then
        printf 'memory pre-commit: %s contains secret-like content (openai_key)\n' "$p"
      fi
      if printf '%s' "$blob" | grep -E -q -e '(^|[^A-Za-z0-9_])(ghp|github_pat|glpat|xox[baprs])[-_][-_A-Za-z0-9]+([^A-Za-z0-9_-]|$)'; then
        printf 'memory pre-commit: %s contains secret-like content (vendor_token)\n' "$p"
      fi
    done
)
if [ -n "$secret_failed" ]; then
  printf '%s\n' "$secret_failed"
  exit 1
fi

exit 0
"##;

/// Post-commit mirror push script to an optional user-owned remote.
pub const POST_COMMIT_HOOK_SCRIPT: &str = r#"#!/bin/sh
# Push memory commits to the configured memory-repository mirror.
# Installed by omo memory-core. Do not edit by hand - regenerated on startup.
url=$(git config --local --get omo.memoryRepository.url 2>/dev/null)
[ -z "$url" ] && exit 0

branch=$(git symbolic-ref --quiet --short HEAD 2>/dev/null) || exit 0
[ -z "$branch" ] && exit 0
[ "$branch" != "main" ] && exit 0

log="$(git rev-parse --git-dir)/memory-repository-push.log"

push_to_mirror() {
  {
    printf '\n--- %s %s on %s ---\n' "$(date '+%Y-%m-%dT%H:%M:%S')" "$(git rev-parse --short HEAD)" "$branch"
    git push --quiet "$url" "$branch:$branch" 2>&1
    echo "exit=$?"
  } >> "$log" 2>&1
}

if [ "$OMO_MEMORY_PUSH_SYNC" = "1" ]; then
  push_to_mirror
  exit 0
fi

push_to_mirror &
exit 0
"#;
