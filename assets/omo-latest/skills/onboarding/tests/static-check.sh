#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Indo Yoon
#
# Static checks for the onboarding skill. Paths are relative to the skill directory
# (<skills_root>/onboarding/), so this runs the same in the source tree and when staged.
set -uo pipefail

SKILL_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SKILL="$SKILL_DIR/SKILL.md"
errors=0
err() { echo "FAIL: $*" >&2; errors=$((errors + 1)); }

[[ -f "$SKILL" ]] || { echo "FAIL: missing $SKILL" >&2; exit 1; }
[[ -f "$SKILL_DIR/LICENSE" ]] || err "LICENSE missing"
grep -q 'MIT License' "$SKILL_DIR/LICENSE" 2>/dev/null || err "LICENSE is not MIT"
grep -q 'Copyright (c) 2026 Indo Yoon' "$SKILL_DIR/LICENSE" 2>/dev/null || err "LICENSE copyright line"
for f in scripts/savings-estimate.mjs scripts/init-deep-gate.mjs tests/savings-fixture.sh; do
  [[ -f "$SKILL_DIR/$f" ]] || err "missing $f"
done

# --- frontmatter ---
[[ "$(head -n1 "$SKILL")" == "---" ]] || err "SKILL.md must start with a frontmatter fence"
FM="$(awk 'NR==1 && $0=="---"{f=1; next} f && $0=="---"{exit} f{print}' "$SKILL")"
grep -qx 'name: onboarding' <<<"$FM" || err "frontmatter lacks 'name: onboarding'"
grep -q 'disable-model-invocation' <<<"$FM" && err "frontmatter must not set disable-model-invocation"
DESC="$(sed -n 's/^description:[[:space:]]*//p' <<<"$FM")"
[[ -n "$DESC" ]] || err "description is empty"
(( ${#DESC} <= 1024 )) || err "description is ${#DESC} chars (> 1024)"

# --- no cost caveats about onboarding itself (R5) ---
if grep -Eiq 'token cost warning|may use tokens|this may cost|warning.*token' "$SKILL"; then
  err "SKILL.md contains a cost-warning phrase:"; grep -Ein 'token cost warning|may use tokens|this may cost|warning.*token' "$SKILL" >&2
fi

# --- banned upstream names (§8) ---
BANNED='Sisyphus|Prometheus|oh-my-openagent|OmO|ultrawork|\bulw\b|mass-ulw|mass ulw|hyperplan|boulder|Fable'
if grep -Eq "$BANNED" "$SKILL"; then
  err "banned name in SKILL.md:"; grep -En "$BANNED" "$SKILL" >&2
fi
grep -Eiq 'hyperplan|ultrawork|\bulw\b|boulder' "$SKILL" && err "banned name (case-insensitive)"

# 'senpi' and bare 'omo' are allowed only inside exact runtime identifiers.
STRIPPED="$(sed -E \
  -e 's/omo-senpi-onboarding-disabled//g' \
  -e 's/--platform senpi//g' \
  -e 's/omo-onboarding(:[a-z]+)?//g' \
  -e 's#~/\.omo/[A-Za-z0-9_./-]*##g' \
  -e 's#~/\.omo##g' \
  -e 's/omo setup( --dry-run| --yes)?//g' \
  -e 's/command -v omo//g' \
  -e 's/omo\.onboarding\.apply//g' \
  -e 's/name `omo`//g' \
  "$SKILL")"
grep -Eiq 'senpi' <<<"$STRIPPED" && { err "'senpi' outside an allowed runtime identifier:"; grep -Ein 'senpi' <<<"$STRIPPED" >&2; }
grep -Eiq '\bomo\b' <<<"$STRIPPED" && { err "'omo' used as a product name:"; grep -Ein '\bomo\b' <<<"$STRIPPED" >&2; }

# --- runtime identifiers the skill must use verbatim ---
for id in onboarding_session_history onboarding_understand onboarding_memory_apply 'consent: true' \
          'omo-onboarding:bootstrap' 'omo-onboarding:context' 'omo-onboarding:history' 'Value summary:' \
          'Migration inventory' 'system/human.md' 'system/project.md' 'mhc --onboard' 'mhc --list-tips' \
          'omo setup --dry-run' 'omo setup --yes' 'init-deep/SKILL.md' 'tool_search' \
          'scripts/savings-estimate.mjs' 'scripts/init-deep-gate.mjs' 'find-agent-sessions.py list --platform senpi'; do
  grep -qF -- "$id" "$SKILL" || err "SKILL.md does not mention required identifier: $id"
done

# --- skills named in the tour must be shipped and not dropped ---
SHIPPED='adversarial-plan ast-grep browser coding-agent-sessions dag-library data-scientist debugging frontend git-master give-me-tips init-deep lsp-setup onboarding programming refactor remove-ai-slops review-work ultimate-browsing visual-qa visualize'
for s in $(grep -oE '`[a-z][a-z0-9-]+`' "$SKILL" | tr -d '`' | sort -u); do
  case "$s" in *-*) ;; *) continue ;; esac   # only hyphenated names look like skill ids
  if grep -qE '^(mass-|ulw-)' <<<"$s"; then err "dropped skill named: $s"; fi
done
for s in init-deep coding-agent-sessions adversarial-plan dag-library browser give-me-tips; do
  grep -qw "$s" <<<"$SHIPPED" || err "tour names unshipped skill $s"
done

if (( errors > 0 )); then echo "static-check: $errors failure(s)" >&2; exit 1; fi
echo "OK static-check: frontmatter, no cost-warning phrases, no banned names, identifiers present"
