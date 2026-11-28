#!/usr/bin/env bash
# Theme lint: UI code takes colours, sizes and spacing
# from loa::ui::kit::theme tokens. Scans cpp/ui/* (except the kit's token layer) and cpp/app,
# or the dirs given as arguments (relative to cpp/). Tests are skipped: their literals are
# input data (escape sequences, fixtures), not styling. Reports only; exits non-zero with
# LINT_ENFORCE=1.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

if [[ $# -gt 0 ]]; then
  dirs=("$@")
else
  dirs=()
  for d in ui/*/ app/; do [[ -d $d ]] && dirs+=("${d%/}"); done
fi
[[ ${#dirs[@]} -eq 0 ]] && { echo "lint-ui-literals: clean"; exit 0; }

rules=(
  'QColor[[:space:]]*[({][[:space:]]*["#0-9]'
  'QColor::from(Rgb|Rgba|RgbF|Hsv|Hsl|String)'
  'Qt::(white|black|red|darkRed|green|darkGreen|blue|darkBlue|cyan|darkCyan|magenta|darkMagenta|yellow|darkYellow|gray|darkGray|lightGray)\b'
  '"[^"]*#[0-9a-fA-F]{6}([0-9a-fA-F]{2})?\b'
  'rgba?\([[:space:]]*[0-9]'
  'setSpacing\([1-9]'
  'setContentsMargins\([1-9]'
  'setFixed(Width|Height|Size)\([1-9]'
  'set(Point|Pixel)Size(F)?\([1-9]'
)
pattern=$(IFS='|'; echo "${rules[*]}")

hits=$(grep -rnE --include='*.cpp' --include='*.hpp' "$pattern" "${dirs[@]}" \
  | grep -vE '^ui/kit/(src/theme/|src/qt\.cpp)|^(ui/[^/]+|app)/tests/' || true)
count=$(printf '%s' "$hits" | grep -c . || true)

if [[ $count -gt 0 ]]; then
  printf '%s\n' "$hits"
  echo "lint-ui-literals: $count literal(s) outside the theme layer" >&2
  [[ "${LINT_ENFORCE:-0}" == 1 ]] && exit 1
else
  echo "lint-ui-literals: clean"
fi
exit 0
