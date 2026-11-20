#!/usr/bin/env bash
# Counts *.rs lines per crate dir, sums them per target crate (scripts/budgets.txt)
# and prints each sum against its budget. Exits non-zero only with BUDGET_ENFORCE=1,
# when a target exceeds its budget by more than BUDGET_SLACK percent (default 5)
# or the workspace exceeds TOTAL_CEILING lines (default 50000).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
budgets="$root/scripts/budgets.txt"
slack="${BUDGET_SLACK:-5}"
ceiling="${TOTAL_CEILING:-50000}"

counts=$(for dir in "$root"/crates/*/; do
  name=$(basename "$dir")
  lines=$(find "$dir" -name '*.rs' -print0 | xargs -0r cat | wc -l)
  echo "count $name $lines"
done)

{ grep -Ev '^\s*(#|$)' "$budgets"; echo "$counts"; } | awk -v slack="$slack" -v ceiling="$ceiling" '
  $1 == "budget" { budget[$2] = $3; order[++n] = $2 }
  $1 == "map"    { target[$2] = $3 }
  $1 == "count"  { t = ($2 in target) ? target[$2] : $2; sum[t] += $3; total += $3
                   if (!(t in budget) && !(t in seen)) { extra[++m] = t; seen[t] = 1 } }
  END {
    printf "%-18s %8s %8s %8s  %s\n", "target", "lines", "budget", "delta", "status"
    for (i = 1; i <= n; i++) {
      t = order[i]; b = budget[t]; s = sum[t] + 0
      status = (s > b * (1 + slack / 100)) ? "OVER" : ((s > b) ? "over (within slack)" : "ok")
      if (status == "OVER") bad = 1
      printf "%-18s %8d %8d %+8d  %s\n", t, s, b, s - b, status
    }
    for (i = 1; i <= m; i++) { printf "%-18s %8d %8s %8s  %s\n", extra[i], sum[extra[i]], "-", "-", "NO BUDGET"; bad = 1 }
    printf "%-18s %8d %8d %+8d  %s\n", "TOTAL", total, ceiling, total - ceiling, ((total > ceiling) ? "OVER" : "ok")
    if (total > ceiling) bad = 1
    exit bad
  }' && over=0 || over=$?

if [[ "${BUDGET_ENFORCE:-0}" == 1 && $over -ne 0 ]]; then
  echo "budget exceeded" >&2
  exit 1
fi
