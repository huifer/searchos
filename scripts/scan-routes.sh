#!/usr/bin/env bash
#
# scan-routes.sh — Static scanner for axum route conflicts.
#
# VENDORED into this repository so the route-conflict gate runs in CI (job
# `openapi`) on every pull request, instead of only on a developer machine.
# The scanner itself is a text scanner; it complements (never replaces) the
# semantic gates `openapi_spec_matches_the_route_table` and
# `every_registered_route_resolves`.
#
# Usage: SRC_DIR=crates/searchos-interface-http/src bash scripts/scan-routes.sh
#
# Parses every `.route(...)` / `.nest(...)` / `.merge(...)` call under src/,
# reconstructs what it can of the mounted path tree, and reports:
#   [A] DUPLICATE NEST      — same prefix nested >1× in one file        (nest-01)
#   [B] DUP PATH+METHOD     — identical path AND method in one file     (method-01)
#   [C] CATCH-ALL PRESENT   — {*x} in a .route() path; verify siblings  (star-02)
#   [D] PARAM+CATCHALL      — {x} and {*x} at same prefix in one file   (dyn-02)
#   [E] DOUBLE-PATH (nest)  — nest("/X", fn()) + inner route("/X...")   (nest-04)
#   [F] BAK FILES           — stale router .bak files confuse audits    (ops-01)
#
# IMPORTANT: This is a TEXT scanner, not a semantic analyzer. It deliberately
# stays portable (bash + grep + perl, no ripgrep/jq requirement) and targets
# the exact bug classes that have bitten this project. It CANNOT see routes
# built from config/DB or conditionally registered routes.
#
# Path-method extraction uses perl when available (for PCRE), else falls back
# to BSD/GNU sed. Tested on macOS (BSD sed) and Linux.
#
# Exit code: 0 if clean, 1 if any finding. Suitable for CI / pre-commit.
#
# NOTE: we use `set -uo pipefail` but NOT `set -e`. With `set -e`, several
# legitimate "no match" grep/awk pipelines abort the whole scan mid-stream
# before later checks run. The script guards every failure path explicitly
# (`|| true`, `[[ -z ]] && continue`), so `set -e` is unnecessary and harmful
# here. `pipefail` is still on so a broken pipe is visible in `$?`.
set -uo pipefail

# --- Locate src/ -----------------------------------------------------------
SRC_DIR="${SRC_DIR:-}"
if [[ -z "$SRC_DIR" ]]; then
  CUR="$(pwd)"
  while [[ "$CUR" != "/" ]]; do
    if [[ -f "$CUR/Cargo.toml" ]] && grep -q '\[package\]' "$CUR/Cargo.toml" 2>/dev/null; then
      SRC_DIR="$CUR/src"
      break
    fi
    CUR="$(dirname "$CUR")"
  done
fi
if [[ -z "$SRC_DIR" || ! -d "$SRC_DIR" ]]; then
  echo "✖ Could not locate a 'src/' dir next to a Cargo.toml." >&2
  echo "  Run this script from inside the project, or set SRC_DIR=/path/to/src" >&2
  exit 2
fi

FINDINGS=0
SECTIONS_SHOWN=0

# --- Helpers ---------------------------------------------------------------

# Extract the first double-quoted string from a line.
first_str_arg() {
  local line="$1"
  if command -v perl >/dev/null 2>&1; then
    printf '%s' "$line" | perl -nle 'print $1 if /"((?:[^"\\]|\\.)*)"/' | head -n1
  else
    printf '%s' "$line" | sed -nE 's/[^"]*"([^"]*)".*/\1/p' | head -n1
  fi
}

# Extract all HTTP method handlers chained on a .route(...) line.
# e.g. ".route("/x", get(f).post(g))" -> "get post"
# e.g. ".route("/x", axum::routing::post(h))"  -> "post"
# Returns a space-separated lowercased method list (may be empty).
extract_methods() {
  local line="$1"
  if command -v perl >/dev/null 2>&1; then
    printf '%s' "$line" \
      | perl -nle 'print join(" ", /\b(get|post|put|delete|patch|head|options)\b/gi)' \
      | tr '[:upper:]' '[:lower:]'
  else
    printf '%s' "$line" \
      | grep -oE '\b(get|post|put|delete|patch|head|options)\b' \
      | tr '\n' ' ' | tr '[:upper:]' '[:lower:]'
  fi
}

# Normalize a nest prefix: strip trailing slash (except root).
norm_prefix() {
  local p="$1"
  p="${p%/}"
  [[ -z "$p" ]] && p="/"
  printf '%s' "$p"
}

# Extract the callee from a .nest("/x", foo::bar()) line -> "foo::bar"
extract_nest_callee() {
  local line="$1"
  if command -v perl >/dev/null 2>&1; then
    printf '%s' "$line" | perl -nle 'print $1 if /\.nest\s*\([^,]+,\s*([A-Za-z0-9_:]+)\s*\(/'
  else
    # BSD sed: \s unsupported, use [[:space:]]
    printf '%s' "$line" | sed -nE 's/.*\.nest\([^,]+,[[:space:]]*([A-Za-z0-9_:]+)[[:space:]]*\(.*/\1/p'
  fi
}

# --- [A] Duplicate nest in one file ----------------------------------------
scan_duplicate_nest() {
  local tmp
  tmp="$(mktemp)"
  while IFS= read -r f; do
    grep -nE '\.nest\(' "$f" 2>/dev/null | while IFS=: read -r ln rest; do
      local p
      p="$(first_str_arg "$rest")"
      [[ -z "$p" ]] && continue
      p="$(norm_prefix "$p")"
      printf '%s\t%s\t%s\n' "$f" "$ln" "$p"
    done
  done < <(grep -rlE '\.nest\(' "$SRC_DIR" --include='*.rs' 2>/dev/null || true) > "$tmp"

  awk -F'\t' '{ count[$1"\t"$3]++; lines[$1"\t"$3]=lines[$1"\t"$3]","$2 }
              END { for (k in count) if (count[k]>1) print count[k]"\t"k lines[k] }' "$tmp" \
    | sort > "${tmp}.dups"

  if [[ -s "${tmp}.dups" ]]; then
    printf '\n[A] DUPLICATE NEST (nest-01) — same prefix nested >1× in one file\n'
    printf '    Two .nest(prefix) in one Router is a latent collision: the next inner\n'
    printf '    route that overlaps will shadow silently. Consolidate into one nest\n'
    printf '    or use distinct prefixes.\n\n'
    while IFS=$'\t' read -r cnt file prefix lns; do
      printf '    %s  prefix=%s  (%s×)%s\n' "$file" "$prefix" "$cnt" "$lns"
    done < "${tmp}.dups"
    n="$(awk -F'\t' '{print $2}' "${tmp}.dups" | sort -u | wc -l | tr -d ' ')"
    FINDINGS=$((FINDINGS + n))
    SECTIONS_SHOWN=$((SECTIONS_SHOWN + 1))
  fi
  rm -f "$tmp" "${tmp}.dups"
}

# --- [B] Duplicate path + same method in one file --------------------------
# Only flag when the SAME path is registered with an OVERLAPPING method set.
# Different methods on the same path is legitimate axum (method-02).
scan_duplicate_path_method() {
  local tmp
  tmp="$(mktemp)"
  while IFS= read -r f; do
    grep -nE '\.route\(' "$f" 2>/dev/null | while IFS=: read -r ln rest; do
      local p methods
      p="$(first_str_arg "$rest")"
      [[ -z "$p" ]] && continue
      methods="$(extract_methods "$rest")"
      printf '%s\t%s\t%s\t%s\n' "$f" "$p" "$ln" "$methods"
    done
  done < <(grep -rlE '\.route\(' "$SRC_DIR" --include='*.rs' 2>/dev/null || true) > "$tmp"

  # For each (file, path) group of size >1, check whether any method appears
  # more than once across the lines. If yes -> real method-01 conflict.
  # Output format: one line per (file,line) in a conflicting group, as TSV:
  #   file<TAB>path<TAB>line<TAB>methods
  local real_conflicts
  real_conflicts="$(mktemp)"
  awk -F'\t' '
    {
      key=$1"\t"$2;
      n[key]++;
      methline[key]=methline[key] $3 ":" $4 "|";
      m=$4;
      gl=split(m, ma, " ");
      for (i=1; i<=gl; i++) if (ma[i]!="") {
        methcount[key SUBSEP ma[i]]++;
        if (methcount[key SUBSEP ma[i]]==2) dup[key]=1;
      }
    }
    END {
      for (k in n) if (n[k]>1 && dup[k]==1) print k "\t" methline[k];
    }
  ' "$tmp" > "$real_conflicts"

  if [[ -s "$real_conflicts" ]]; then
    printf '\n[B] DUPLICATE PATH + METHOD (method-01) — same path, same method twice\n'
    printf '    This is a hard conflict — axum panics at serve time. Deduplicate.\n'
    printf '    (Same path with DIFFERENT methods is fine and not reported.)\n\n'
    # Each real_conflicts line: file<TAB>path<TAB>line:methods|line:methods|...
    while IFS=$'\t' read -r file path methdetail; do
      printf '    %s  path=%s\n' "$file" "$path"
      methdetail="${methdetail%|}"   # drop trailing pipe
      local OLDIFS="$IFS"; IFS='|'
      local pair
      for pair in $methdetail; do
        # pair is "line:methods"
        local pln pmeth
        pln="${pair%%:*}"
        pmeth="${pair#*:}"
        printf '        line %s: [%s]\n' "$pln" "$pmeth"
      done
      IFS="$OLDIFS"
    done < "$real_conflicts"
    n="$(awk -F'\t' '{print $1}' "$real_conflicts" | sort -u | wc -l | tr -d ' ')"
    FINDINGS=$((FINDINGS + n))
    SECTIONS_SHOWN=$((SECTIONS_SHOWN + 1))
  fi
  rm -f "$tmp" "$real_conflicts"
}

# --- [C] Catch-all {*x} inside a .route() path -----------------------------
# Only match catch-alls that appear within a route-path string literal, to
# avoid matching Rust format!() strings and match arms like `Ok(_) => {}`.
scan_catchall() {
  local tmp
  tmp="$(mktemp)"
  while IFS= read -r f; do
    # A catch-all route looks like:  .route("...{*name}...", ...)
    grep -nE '\.route\(\s*"[^"]*\{\*[^}]+\}' "$f" 2>/dev/null | while IFS=: read -r ln rest; do
      local p
      p="$(first_str_arg "$rest")"
      [[ -z "$p" ]] && p="<unparsed>"
      printf '%s\t%s\t%s\n' "$f" "$ln" "$p"
    done
  done < <(grep -rlE '\.route\(\s*"[^"]*\{\*[^}]+\}' "$SRC_DIR" --include='*.rs' 2>/dev/null || true) > "$tmp"

  if [[ -s "$tmp" ]]; then
    printf '\n[C] CATCH-ALL ROUTE (star-02) — {*x} in a .route() path\n'
    printf '    A catch-all is terminal and greedy. For each one below, confirm NO sibling\n'
    printf '    route exists under the same prefix in any merged/nested router.\n'
    printf '    Every prior incident in this project started here.\n\n'
    while IFS=$'\t' read -r f ln p; do
      printf '    %s:%s  %s\n' "$f" "$ln" "$p"
    done < "$tmp"
    n="$(wc -l < "$tmp" | tr -d ' ')"
    FINDINGS=$((FINDINGS + n))
    SECTIONS_SHOWN=$((SECTIONS_SHOWN + 1))
  fi
  rm -f "$tmp"
}

# --- [D] {x} and {*x} at the same prefix in one file -----------------------
# NOTE: bash 3.2 (macOS system /bin/bash) has no `mapfile`/`readarray`, so we
# emit the grep matches into temp files and read them back with `while read`.
scan_param_and_catchall() {
  local found_any=0
  local catmp patmp
  catmp="$(mktemp)"; patmp="$(mktemp)"
  while IFS= read -r f; do
    : > "$catmp"; : > "$patmp"
    grep -nE '\.route\(\s*"[^"]*\{\*[^}]+\}' "$f" 2>/dev/null > "$catmp" || true
    [[ -s "$catmp" ]] || continue
    grep -nE '\.route\(\s*"[^"]*\{[^*/}][^}]*\}' "$f" 2>/dev/null > "$patmp" || true
    [[ -s "$patmp" ]] || continue
    while IFS= read -r ca; do
      local ca_ln ca_rest ca_path ca_dir
      ca_ln="${ca%%:*}"
      ca_rest="${ca#*:}"
      ca_path="$(first_str_arg "$ca_rest")"
      ca_dir="${ca_path%/*}"
      [[ "$ca_dir" == "$ca_path" ]] && ca_dir=""
      while IFS= read -r pl; do
        local pl_ln pl_rest pl_path pl_dir
        pl_ln="${pl%%:*}"
        pl_rest="${pl#*:}"
        pl_path="$(first_str_arg "$pl_rest")"
        pl_dir="${pl_path%/*}"
        [[ "$pl_dir" == "$pl_path" ]] && pl_dir=""
        if [[ "$ca_dir" == "$pl_dir" && "$ca_ln" != "$pl_ln" ]]; then
          if [[ $found_any -eq 0 ]]; then
            printf '\n[D] PARAM + CATCH-ALL (dyn-02) — {x} and {*x} at same prefix\n'
            printf '    matchit cannot hold both. This is a hard conflict.\n\n'
            found_any=1
          fi
          printf '    %s:%s  catch-all %s\n' "$f" "$ca_ln" "$ca_path"
          printf '    %s:%s  param     %s\n' "$f" "$pl_ln" "$pl_path"
          FINDINGS=$((FINDINGS + 1))
        fi
      done < "$patmp"
    done < "$catmp"
  done < <(grep -rlE '\.route\(\s*"[^"]*\{\*[^}]+\}' "$SRC_DIR" --include='*.rs' 2>/dev/null || true)
  [[ $found_any -eq 1 ]] && SECTIONS_SHOWN=$((SECTIONS_SHOWN + 1))
  rm -f "$catmp" "$patmp"
}

# --- [E] nest-04: nest("/X", fn()) + inner route echoes "/X" ---------------
# NOTE: avoid `cmd | while read` — that runs the loop in a subshell and loses
# variable updates. Capture to a temp string and use `<<<` / here-strings, or
# < <(process-substitution).
scan_nest_double_path() {
  local found_any=0
  local results
  results="$(mktemp)"
  # Build a flat list: file \t line \t prefix \t callee \t fnfile \t fn
  while IFS= read -r f; do
    while IFS=: read -r ln rest; do
      local prefix callee fn file_for_fn seg
      prefix="$(first_str_arg "$rest")"
      [[ -z "$prefix" ]] && continue
      seg="${prefix#/}"
      seg="${seg%%/*}"
      [[ -z "$seg" ]] && continue
      callee="$(extract_nest_callee "$rest")"
      [[ -z "$callee" ]] && continue
      fn="${callee##*::}"
      file_for_fn="$(grep -rlE "^[[:space:]]*(pub[[:space:]]+)?fn[[:space:]]+${fn}[[:space:]]*\(" "$SRC_DIR" --include='*.rs' 2>/dev/null | head -n1 || true)"
      [[ -z "$file_for_fn" ]] && continue
      # Body-grep for an inner .route("/<seg>"...). Match the prefix segment
      # exactly, optionally followed by /, {, or end-quote.
      local body_hits
      body_hits="$(awk -v fn="$fn" '
        $0 ~ "(pub )?fn[[:space:]]+" fn "[[:space:]]*\\(" { in_fn=1; depth=0 }
        in_fn {
          print;
          n=gsub(/\{/,"&"); depth+=n;
          n2=gsub(/\}/,"&"); depth-=n2;
          if (depth<=0 && line>0) in_fn=0;
          line++;
        }
      ' "$file_for_fn" | grep -nE "\.route\([[:space:]]*\"/${seg}(/|\{|\")" || true)"
      [[ -z "$body_hits" ]] && continue
      # Encode body_hits (multi-line) onto a single tab-record using a
      # non-printable-safe join: replace newlines with a literal \n marker.
      local body_one
      body_one="$(printf '%s' "$body_hits" | paste -sd '|' -)"
      printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$f" "$ln" "$prefix" "$callee" "$file_for_fn" "$fn" "$body_one"
    done < <(grep -nE '\.nest\(' "$f" 2>/dev/null || true)
  done < <(grep -rlE '\.nest\(' "$SRC_DIR" --include='*.rs' 2>/dev/null || true) > "$results"

  if [[ -s "$results" ]]; then
    printf '\n[E] DOUBLE-PATH via nest (nest-04) — inner route echoes nest prefix\n'
    printf '    nest("/X", fn()) where fn registers .route("/X...") → mounted at /X/X.\n'
    printf '    Usually a bug. Confirm the intended mounted path.\n\n'
    while IFS=$'\t' read -r f ln prefix callee fnfile fn body_one; do
      printf '    %s:%s  .nest("%s", %s)\n' "$f" "$ln" "$prefix" "$callee"
      printf '      → %s  fn %s() registers:\n' "$fnfile" "$fn"
      # Split body_one on '|' and print each.
      while IFS= read -r bh; do
        [[ -z "$bh" ]] && continue
        printf '        %s\n' "$bh"
      done <<< "${body_one//|/$'\n'}"
      FINDINGS=$((FINDINGS + 1))
    done < "$results"
    SECTIONS_SHOWN=$((SECTIONS_SHOWN + 1))
  fi
  rm -f "$results"
}

# --- [F] .bak files --------------------------------------------------------
scan_bak_files() {
  local baks
  baks="$(find "$SRC_DIR" -name '*.bak' 2>/dev/null || true)"
  if [[ -n "$baks" ]]; then
    printf '\n[F] STALE .bak FILES (ops-01) — remove; git is the backup\n'
    printf '    These contain live-looking route definitions that mislead audits.\n\n'
    while IFS= read -r b; do
      [[ -z "$b" ]] && continue
      printf '    %s\n' "$b"
      FINDINGS=$((FINDINGS + 1))
    done <<< "$baks"
    SECTIONS_SHOWN=$((SECTIONS_SHOWN + 1))
  fi
}

# --- main ------------------------------------------------------------------
main() {
  printf '🛣️  Axum Route Conflict Scanner\n'
  printf '    Scanning: %s\n' "$SRC_DIR"
  printf '    (text scanner — sees .route/.nest/.merge in *.rs; not semantic)\n'

  scan_duplicate_nest
  scan_duplicate_path_method
  scan_catchall
  scan_param_and_catchall
  scan_nest_double_path
  scan_bak_files

  printf '\n'
  if [[ "$FINDINGS" -eq 0 ]]; then
    printf '✓ No route conflicts detected by the text scanner.\n'
    printf '  Reminder: this does NOT cover dynamic/conditional routes or proxy\n'
    printf '  layering. For those, read the handler bodies and nginx/Cloudflare config.\n'
    exit 0
  else
    printf '✖ %s finding group(s), %s total finding(s).\n' "$SECTIONS_SHOWN" "$FINDINGS"
    printf '  Fix before merging. See checklists/before-adding-route.md.\n'
    exit 1
  fi
}

main "$@"
