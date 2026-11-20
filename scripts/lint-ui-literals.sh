#!/usr/bin/env bash
# Theme lint: UI code takes colours, sizes and spacing from loa-ui-kit tokens.
# Scans every loa-ui-* crate (except loa-ui-kit/src/theme) and loa-app, or the
# crate dirs given as arguments. Reports only; exits non-zero with LINT_ENFORCE=1.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

if [[ $# -gt 0 ]]; then
  dirs=("$@")
else
  dirs=()
  for d in crates/loa-ui-*/ crates/loa-app/; do [[ -d $d ]] && dirs+=("${d%/}"); done
fi

rules=(
  'Color::from_rgb'
  'Color \{'
  'extended_palette\(\)'
  '\.size\([0-9]'
  '\.padding\(\[?[0-9]'
  '\.spacing\([0-9]'
  'Length::Fixed\([0-9]'
)
pattern=$(IFS='|'; echo "${rules[*]}")

hits=$(grep -rnE --include='*.rs' "$pattern" "${dirs[@]}" | grep -v '^crates/loa-ui-kit/src/theme' || true)
count=$(printf '%s' "$hits" | grep -c . || true)

if [[ $count -gt 0 ]]; then
  printf '%s\n' "$hits"
  echo "lint-ui-literals: $count literal(s) outside the theme layer" >&2
  [[ "${LINT_ENFORCE:-0}" == 1 ]] && exit 1
else
  echo "lint-ui-literals: clean"
fi
exit 0
