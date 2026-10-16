#!/bin/sh
# engine-deploy.sh: put a new ah-engine binary live together with its plugin defaults, in the one safe order.
#
#   sh scripts/engine-deploy.sh --check    <new-binary> <new-defaults>              read-only pre-check
#   sh scripts/engine-deploy.sh --deploy   <new-binary> <new-defaults> [--yes] [--allow-drop]
#   sh scripts/engine-deploy.sh --rollback [--from <backup-dir>] [--yes]
#
# <new-defaults> is the new plugin's defaults: a plugin root, its engine/ directory, or the defaults directory itself (the
# directory that holds index.toml; defaults.pristine is taken from its sibling when there is one, else the same files).
#
# Why: a binary that needs a defaults key the live plugin tree lacks logs "defaults file unusable, built-in values used" and the
# guards fail open. So the keys go in BEFORE the binary, in EVERY plugin root a hook can run from, and the pre-check proves it.
#
# --check   (read-only, no --yes needed) for each live plugin root: copies the root to scratch, applies the new defaults to the copy
#           the way --deploy would, and asks THE NEW BINARY'S OWN validation (`config validate`, which loads the defaults and
#           refuses a plugin missing a key the engine reads) to accept it. Also validates the root as it is now (informational:
#           a failure there is exactly what swapping the binary first would cause) and prints a key diff: keys added, keys
#           the sync would drop (a problem unless --allow-drop: launcher-only keys live there), and changed values.
#           Exits non-zero on any problem.
# --deploy  dry run by default (runs --check and prints the plan); with --yes: check, stop the engine, sync defaults and
#           defaults.pristine into every root (each replaced file is copied to the backup first, never deleted), swap the binary
#           (the old one stays as bin/ah-engine.prev), record it with the integrity script, then verify: sha matches, force-push
#           probe blocked through the plugin root Claude runs, `status --json` running with defaults.total 0, defaults.error not
#           rewritten, roots identical. A failed step prints the exact --rollback command; nothing is rolled back on its own.
# --rollback  dry run by default; with --yes restores the .prev binary and every backed-up defaults file of the chosen deploy
#           (default: the latest). Files the deploy added are moved aside, not deleted; the files being replaced are kept too.
#
# Only engine/defaults and engine/defaults.pristine of each root are synced. Hooks, scripts and the launcher are not touched.
# Nothing is ever removed with rm: replaced files are kept in the backup directory. POSIX sh, no Node.
#
# Environment (all optional; defaults are $HOME-relative):
#   AH_DEPLOY_HOME            base for every default below (default $HOME)
#   AH_DEPLOY_ENGINE_DIR      engine state dir (default $AH_DEPLOY_HOME/.anti-hall/ah-engine); binary = bin/ah-engine in it
#   AH_DEPLOY_ROOTS           colon-separated plugin roots to sync (default: the two roots below)
#   AH_DEPLOY_MKT_ROOT        the engine-live marketplace plugin tree
#                             (default $AH_DEPLOY_HOME/.anti-hall/ah-engine-live/state/marketplace/plugins/anti-hall)
#   AH_DEPLOY_INSTALLED_JSON  installed_plugins.json (default $AH_DEPLOY_HOME/.claude/plugins/installed_plugins.json)
#   AH_DEPLOY_PLUGIN_KEY      its entry whose installPath is the cache root Claude runs hooks from (default anti-hall@anti-hall-engine-live)
#   AH_DEPLOY_BACKUP_ROOT     where backups go (default $AH_DEPLOY_ENGINE_DIR/deploys)
#   AH_DEPLOY_PROBE_N         force-push probe repetitions, all must be blocked (default 3)
#   AH_DEPLOY_CALL_TIMEOUT    seconds allowed per engine call, when timeout(1) exists (default 30)

set -u
LC_ALL=C; export LC_ALL

H=${AH_DEPLOY_HOME:-${HOME:-}}
[ -n "$H" ] || { echo "engine-deploy: no HOME" >&2; exit 2; }
ENGINE_DIR=${AH_DEPLOY_ENGINE_DIR:-$H/.anti-hall/ah-engine}
BIN=$ENGINE_DIR/bin/ah-engine
BK_ROOT=${AH_DEPLOY_BACKUP_ROOT:-$ENGINE_DIR/deploys}
MKT_ROOT=${AH_DEPLOY_MKT_ROOT:-$H/.anti-hall/ah-engine-live/state/marketplace/plugins/anti-hall}
INSTALLED=${AH_DEPLOY_INSTALLED_JSON:-$H/.claude/plugins/installed_plugins.json}
PLUGIN_KEY=${AH_DEPLOY_PLUGIN_KEY:-anti-hall@anti-hall-engine-live}
PROBE_N=${AH_DEPLOY_PROBE_N:-3}
CALL_T=${AH_DEPLOY_CALL_TIMEOUT:-30}
NL='
'

die() { printf 'engine-deploy: %s\n' "$*" >&2; exit 1; }
note() { printf '%s\n' "$*"; }
PROBLEMS=0
problem() { PROBLEMS=$((PROBLEMS + 1)); printf 'PROBLEM: %s\n' "$*"; }

sha() {
  [ -f "$1" ] || { echo absent; return 0; }
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

TB=; command -v timeout >/dev/null 2>&1 && TB=timeout
[ -z "$TB" ] && command -v gtimeout >/dev/null 2>&1 && TB=gtimeout
# lim CMD...: stdin from /dev/null, a time limit when timeout(1) is available
lim() { if [ -n "$TB" ]; then "$TB" -k 5 "$CALL_T" "$@" </dev/null; else "$@" </dev/null; fi; }

SCRATCH=
# the scratch dir is never removed with rm: it is moved to the Trash when there is one, else left for the OS temp reaper
cleanup() { [ -n "$SCRATCH" ] && [ -d "$SCRATCH" ] && [ -d "$H/.Trash" ] && mv "$SCRATCH" "$H/.Trash/engine-deploy.$$" 2>/dev/null; return 0; }
trap cleanup EXIT
mkscratch() { [ -n "$SCRATCH" ] || SCRATCH=$(mktemp -d "${TMPDIR:-/tmp}/engine-deploy.XXXXXX") || die "cannot make a scratch dir"; }

# --- plugin roots -----------------------------------------------------------------------------------------------------
installed_path() {
  [ -f "$1" ] || return 0
  awk -v k="\"$2\"" 'index($0, k) && /: *\[/ {on = 1; next}
    on && /"installPath"/ {gsub(/.*"installPath": *"/, ""); gsub(/".*/, ""); print; exit}' "$1"
}
ROOTS=; PROBE_ROOT=
resolve_roots() {
  if [ -n "${AH_DEPLOY_ROOTS:-}" ]; then
    ROOTS=$(printf '%s' "$AH_DEPLOY_ROOTS" | tr ':' '\n' | grep .)
  else
    c=$(installed_path "$INSTALLED" "$PLUGIN_KEY")
    [ -n "$c" ] || die "no installPath for $PLUGIN_KEY in $INSTALLED (set AH_DEPLOY_ROOTS to name the plugin roots)"
    ROOTS=$MKT_ROOT$NL$c
  fi
  for r in $ROOTS; do
    [ -f "$r/engine/defaults/index.toml" ] || die "plugin root $r has no engine/defaults/index.toml"
  done
  # a hook runs from the cache root Claude installed: that one answers the force-push probe (the last root listed)
  PROBE_ROOT=$(printf '%s\n' "$ROOTS" | tail -n 1)
}

# --- new defaults -----------------------------------------------------------------------------------------------------
SRC_DEF=; SRC_PRI=
resolve_src() {
  d=$1
  [ -d "$d" ] || die "new defaults not found: $d"
  if [ -f "$d/engine/defaults/index.toml" ]; then d=$d/engine; fi
  if [ -f "$d/defaults/index.toml" ]; then
    SRC_DEF=$d/defaults
    SRC_PRI=$d/defaults.pristine; [ -f "$SRC_PRI/index.toml" ] || SRC_PRI=$SRC_DEF
  elif [ -f "$d/index.toml" ]; then
    SRC_DEF=$d
    SRC_PRI=$d
    sib=$(dirname "$d")/defaults.pristine
    [ "$(basename "$d")" = defaults ] && [ -f "$sib/index.toml" ] && SRC_PRI=$sib
  else die "no index.toml under $1 (give a plugin root, its engine/ dir, or the defaults dir)"; fi
}

# keys_of FILE: one "table-header<TAB>first value line" per table
keys_of() {
  awk '/^\[/ {h = $0; hv[h] = ""; next}
       h != "" && hv[h] == "" && /^value[ \t]*=/ {sub(/^value[ \t]*=[ \t]*/, ""); hv[h] = $0}
       END {for (k in hv) print k "\t" hv[k]}' "$1" | sort -t "$(printf '\t')" -k1,1
}

# list_keys LABEL FILE: one line: the count and the first 3 keys of FILE
list_keys() {
  [ -s "$2" ] || return 0
  n=$(wc -l <"$2" | tr -d ' ')
  note "    $1 ($n): $(head -n 3 "$2" | tr '\n' ' ')$([ "$n" -gt 3 ] && echo '...')"
  return 0
}

# diff_dir SRC DST LABEL: print the key diff of every file in SRC against DST; sets DROPPED/ADDED counters
ADDED=0; DROPPED=0; FILES_CHANGED=0
diff_dir() {
  for f in "$1"/*; do
    [ -f "$f" ] || continue
    b=${f##*/}
    case $b in *.bak*) continue ;; esac
    if [ ! -f "$2/$b" ]; then
      keys_of "$f" | cut -f1 >"$SCRATCH/add.k"
      list_keys "+ $3/$b  new file, keys" "$SCRATCH/add.k"; ADDED=$((ADDED + $(wc -l <"$SCRATCH/add.k" | tr -d ' ')))
      FILES_CHANGED=$((FILES_CHANGED + 1)); continue
    fi
    cmp -s "$f" "$2/$b" && continue
    FILES_CHANGED=$((FILES_CHANGED + 1))
    keys_of "$f" >"$SCRATCH/new.kv"; keys_of "$2/$b" >"$SCRATCH/old.kv"
    cut -f1 "$SCRATCH/new.kv" >"$SCRATCH/new.k"; cut -f1 "$SCRATCH/old.kv" >"$SCRATCH/old.k"
    comm -23 "$SCRATCH/new.k" "$SCRATCH/old.k" >"$SCRATCH/add.k"
    list_keys "+ $3/$b  keys added" "$SCRATCH/add.k"; ADDED=$((ADDED + $(wc -l <"$SCRATCH/add.k" | tr -d ' ')))
    comm -13 "$SCRATCH/new.k" "$SCRATCH/old.k" >"$SCRATCH/drop.k"
    while IFS= read -r k; do note "    - $3/$b  key DROPPED by the sync: $k"; DROPPED=$((DROPPED + 1)); done <"$SCRATCH/drop.k"
    join -t "$(printf '\t')" "$SCRATCH/old.kv" "$SCRATCH/new.kv" 2>/dev/null | awk -F'\t' -v f="$3/$b" '$2 != $3 {n++; if (n <= 6) printf "    ~ %s  value changes: %s  %.30s -> %.30s\n", f, $1, $2, $3} END {if (n > 6) printf "    ~ %s  ... and %d more value changes\n", f, n - 6}'
  done
}

# apply_dir SRC DST BACKUP MANIFEST: copy each SRC file into DST. A replaced file is copied to BACKUP first, the new one is
# renamed into place (no moment without the file). Manifest rows: F dst file had backup
apply_dir() {
  mkdir -p "$2" || return 1
  for f in "$1"/*; do
    [ -f "$f" ] || continue
    b=${f##*/}
    case $b in *.bak*) continue ;; esac
    had=0; bk=-
    if [ -e "$2/$b" ]; then
      cmp -s "$f" "$2/$b" && continue
      mkdir -p "$3" || return 1
      cp -p "$2/$b" "$3/$b" || return 1
      had=1; bk=$3/$b
    fi
    [ -n "${4:-}" ] && printf 'F\t%s\t%s\t%s\t%s\n' "$2" "$b" "$had" "$bk" >>"$4"
    cp -p "$f" "$2/.$b.deploy.$$" && mv "$2/.$b.deploy.$$" "$2/$b" || return 1
  done
}

# engine_env_run BIN ROOT ARGS...: the binary against a scratch HOME and state dir (never the live daemon or cache)
eng_scratch() {
  _b=$1; _r=$2; shift 2
  ( HOME=$SCRATCH/home AH_ENGINE_DIR=$SCRATCH/home/st AH_ENGINE_PLUGIN_ROOT=$_r; export HOME AH_ENGINE_DIR AH_ENGINE_PLUGIN_ROOT; lim "$_b" "$@" 2>&1 )
}

# --- check ------------------------------------------------------------------------------------------------------------
ALLOW_DROP=0
do_check() {
  NEW=$1
  [ -f "$NEW" ] && [ -x "$NEW" ] || die "new binary missing or not executable: $NEW"
  resolve_src "$2"; resolve_roots
  mkscratch; mkdir -p "$SCRATCH/home"; : >"$SCRATCH/empty.toml"
  newsha=$(sha "$NEW"); livesha=$(sha "$BIN")
  note "== engine-deploy check"
  note "new binary : $NEW  sha $(printf '%s' "$newsha" | cut -c1-12)"
  note "live binary: $BIN  sha $(printf '%s' "$livesha" | cut -c1-12)$([ "$newsha" = "$livesha" ] && echo '  (IDENTICAL to the new one)')"
  v=$(eng_scratch "$NEW" "$SCRATCH/none" version --json) || true
  note "new binary version: $(printf '%s' "$v" | head -c 120)"
  note "new defaults: $SRC_DEF  (pristine: $SRC_PRI)"
  i=0
  for r in $ROOTS; do
    i=$((i + 1))
    note "-- root $i: $r"
    out=$(eng_scratch "$NEW" "$r" config validate "$SCRATCH/empty.toml"); rc=$?
    if [ $rc -eq 0 ]; then note "  new binary on this root AS IS: loads (no key is missing)"
    else note "  new binary on this root AS IS: REFUSES (rc $rc: $(printf '%s' "$out" | head -n 1 | cut -c1-200)) -- swapping the binary before the keys would cause exactly this"; fi
    ADDED=0; DROPPED=0; FILES_CHANGED=0
    diff_dir "$SRC_DEF" "$r/engine/defaults" "defaults"
    diff_dir "$SRC_PRI" "$r/engine/defaults.pristine" "defaults.pristine"
    note "  summary: $FILES_CHANGED file(s) change, $ADDED key(s) added, $DROPPED key(s) dropped"
    if [ "$DROPPED" -gt 0 ]; then
      if [ "$ALLOW_DROP" = 1 ]; then note "  dropped keys allowed (--allow-drop)"
      else problem "root $r: the sync would drop $DROPPED key(s) the new defaults lack (launcher-only keys?); merge them into the new defaults, or pass --allow-drop"; fi
    fi
    # the root after the sync, in scratch: this is what the new binary will read
    sim=$SCRATCH/sim$i
    mkdir -p "$sim" && cp -R "$r/." "$sim/" || { problem "cannot copy $r to scratch"; continue; }
    apply_dir "$SRC_DEF" "$sim/engine/defaults" "$SCRATCH/simbk$i/d" "" && apply_dir "$SRC_PRI" "$sim/engine/defaults.pristine" "$SCRATCH/simbk$i/p" "" || { problem "cannot apply the new defaults to the scratch copy of $r"; continue; }
    out=$(eng_scratch "$NEW" "$sim" config validate "$SCRATCH/empty.toml"); rc=$?
    if [ $rc -eq 0 ]; then
      j=$(eng_scratch "$NEW" "$sim" config --json)
      le=$(printf '%s' "$j" | grep -o '"last_error":[^,}]*' | head -n 1)
      case $le in ''|'"last_error":null') note "  new binary on this root AFTER the sync: loads, no config error" ;;
        *) problem "root $r after the sync: config reports $le" ;; esac
    else problem "root $r after the sync: the new binary REFUSES it (rc $rc: $(printf '%s' "$out" | head -n 1 | cut -c1-240))"; fi
  done
  if [ "$PROBLEMS" -gt 0 ]; then note "CHECK FAILED: $PROBLEMS problem(s); nothing was changed"; return 1; fi
  note "CHECK OK: the new binary accepts every root once the new defaults are in (nothing was changed)"
  return 0
}

# --- deploy -----------------------------------------------------------------------------------------------------------
DLOG=
step() { printf '%s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*" | tee -a "$DLOG"; }
fail_deploy() {
  step "FAILED: $*"
  printf '\nengine-deploy: %s\nThe deploy stopped part way. To restore the previous state run:\n  sh %s --rollback --from %s --yes\n' "$*" "$0" "$BK" >&2
  exit 1
}

num_in_section() { # JSON SECTION KEY: top-level number KEY inside the object SECTION
  printf '%s' "$1" | awk -v sec="\"$2\":" -v key="\"$3\":" '{
    p = index($0, sec); if (!p) next
    s = substr($0, p + length(sec)); d = 0
    for (i = 1; i <= length(s); i++) { c = substr(s, i, 1)
      if (c == "{") d++
      else if (c == "}") { d--; if (d <= 0) break }
      else if (d == 1 && substr(s, i, length(key)) == key) { t = substr(s, i + length(key)); sub(/[^0-9].*/, "", t); print t; exit } } }'
}

verify_live() { # returns 0 when every check passes; prints each
  ok=0
  ns=$(sha "$BIN")
  if [ "$ns" = "$NEWSHA" ]; then step "verify: binary sha matches the new one"; else step "verify FAILED: binary sha $ns != $NEWSHA"; ok=1; fi
  exp=$ENGINE_DIR/bin/ah-engine.expected
  if [ -f "$exp" ]; then
    if [ "$(tr -d ' \n' <"$exp")" = "$NEWSHA" ]; then step "verify: integrity record is the new sha"; else step "verify FAILED: $exp does not hold the new sha"; ok=1; fi
  else step "verify: no $exp (integrity script absent?)"; fi
  # roots identical (the two trees must serve the same defaults)
  first=; for r in $ROOTS; do
    if [ -z "$first" ]; then first=$r; continue; fi
    for d in defaults defaults.pristine; do
      if diff -rq -x '*.bak*' -x '.*.deploy.*' "$first/engine/$d" "$r/engine/$d" >/dev/null 2>&1; then :; else step "verify FAILED: engine/$d differs between $first and $r"; ok=1; fi
    done
  done
  [ "$ok" -eq 0 ] && step "verify: plugin roots serve identical defaults"
  # force-push probe through the root Claude runs (a first call may start the daemon; it is retried as a warm-up)
  pl='{"session_id":"deploy-probe","cwd":"'"$SCRATCH"'","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"git push --force origin main","description":"Push"},"tool_use_id":"toolu_deploy_probe"}'
  mkdir -p "$SCRATCH"
  # lim reads stdin from /dev/null, so the probe feeds the payload through a file
  printf '%s' "$pl" >"$SCRATCH/probe.json"
  probe() { ( HOME=$H AH_ENGINE_DIR=$ENGINE_DIR CLAUDE_PLUGIN_ROOT=$PROBE_ROOT; export HOME AH_ENGINE_DIR CLAUDE_PLUGIN_ROOT
      if [ -n "$TB" ]; then "$TB" -k 5 "$CALL_T" sh "$PROBE_ROOT/hooks/ah-hook.sh" PreToolUse <"$SCRATCH/probe.json" >/dev/null 2>&1
      else sh "$PROBE_ROOT/hooks/ah-hook.sh" PreToolUse <"$SCRATCH/probe.json" >/dev/null 2>&1; fi ); }
  probe; warm=$?
  blocked=0; n=0
  while [ "$n" -lt "$PROBE_N" ]; do probe && rcp=0 || rcp=$?; [ "$rcp" -eq 2 ] && blocked=$((blocked + 1)); n=$((n + 1)); done
  if [ "$blocked" -eq "$PROBE_N" ]; then step "verify: force-push probe blocked $blocked/$PROBE_N (warm-up rc $warm) via $PROBE_ROOT"
  else step "verify FAILED: force-push probe blocked $blocked/$PROBE_N via $PROBE_ROOT (a guard is failing open)"; ok=1; fi
  # daemon status
  st=$( HOME=$H AH_ENGINE_DIR=$ENGINE_DIR AH_ENGINE_PLUGIN_ROOT=$first; export HOME AH_ENGINE_DIR AH_ENGINE_PLUGIN_ROOT; lim "$BIN" status --json 2>/dev/null )
  case $st in *'"running":true'*) step "verify: status --json running" ;; *) step "verify FAILED: status --json does not say running"; ok=1 ;; esac
  dt=$(num_in_section "$st" defaults total)
  if [ "${dt:-x}" = 0 ]; then step "verify: defaults.total 0 (no built-in-value fallback)"
  else step "verify FAILED: defaults.total ${dt:-unreadable} (was ${DT_BEFORE:-unknown} before the deploy): the engine fell back to built-in values"; ok=1; fi
  de=$ENGINE_DIR/defaults.error
  if [ "$(sha "$de")" = "$DE_BEFORE" ]; then step "verify: defaults.error not rewritten"
  else step "verify FAILED: $de was written during the deploy: $(head -c 200 "$de" 2>/dev/null)"; ok=1; fi
  return $ok
}

do_deploy() {
  YES=$3
  do_check "$1" "$2" || exit 1
  NEWSHA=$(sha "$NEW")
  resolve_roots
  note ""
  note "== plan"
  note "1 stop the engine ($BIN stop)"
  i=0; for r in $ROOTS; do i=$((i + 1)); note "2.$i sync engine/defaults + engine/defaults.pristine of $r (replaced files copied to the backup first)"; done
  note "3 binary: $BIN -> keep as $BIN.prev; install $NEW"
  note "4 integrity record (accept) with the first root's hooks/ah-engine-integrity.sh"
  note "5 verify: sha, integrity record, roots identical, force-push probe x$PROBE_N via $PROBE_ROOT, status running, defaults.total 0, defaults.error unchanged"
  if [ "$YES" != 1 ]; then note ""; note "dry run: nothing was changed. Add --yes to deploy."; exit 0; fi
  [ "$NEWSHA" = "$(sha "$BIN")" ] && note "(the new binary is identical to the live one: only the defaults change)"

  ts=$(date -u +%Y%m%dT%H%M%SZ)
  BK=$BK_ROOT/$ts
  mkdir -p "$BK" || die "cannot create the backup dir $BK"
  DLOG=$BK/deploy.log; MAN=$BK/manifest.tsv; : >"$MAN"
  printf '%s\n' "$ts" >"$BK_ROOT/latest" 2>/dev/null
  DE_BEFORE=$(sha "$ENGINE_DIR/defaults.error")
  DT_BEFORE=$(num_in_section "$( HOME=$H AH_ENGINE_DIR=$ENGINE_DIR; export HOME AH_ENGINE_DIR; lim "$BIN" status --json 2>/dev/null )" defaults total)
  step "deploy start: new sha $NEWSHA, backup $BK"

  step "stop the engine"
  ( HOME=$H AH_ENGINE_DIR=$ENGINE_DIR AH_ENGINE_PLUGIN_ROOT=$(printf '%s\n' "$ROOTS" | head -n 1); export HOME AH_ENGINE_DIR AH_ENGINE_PLUGIN_ROOT; lim "$BIN" stop >/dev/null 2>&1 ) || step "stop returned non-zero (engine not running?); continuing"

  i=0
  for r in $ROOTS; do
    i=$((i + 1))
    step "sync defaults into root $i: $r"
    apply_dir "$SRC_DEF" "$r/engine/defaults" "$BK/root$i/defaults" "$MAN" || fail_deploy "could not sync $r/engine/defaults"
    apply_dir "$SRC_PRI" "$r/engine/defaults.pristine" "$BK/root$i/defaults.pristine" "$MAN" || fail_deploy "could not sync $r/engine/defaults.pristine"
  done

  step "swap the binary"
  mkdir -p "$ENGINE_DIR/bin" || fail_deploy "cannot create $ENGINE_DIR/bin"
  had=0
  if [ -e "$BIN" ]; then
    had=1
    if [ -e "$BIN.prev" ]; then cp -p "$BIN.prev" "$BK/ah-engine.prev.before" || fail_deploy "cannot back up the old .prev"; fi
    cp -p "$BIN" "$BIN.prev.new.$$" && mv "$BIN.prev.new.$$" "$BIN.prev" || fail_deploy "cannot keep the old binary as .prev"
  fi
  printf 'B\t%s\t%s\n' "$BIN" "$had" >>"$MAN"
  cp -p "$NEW" "$BIN.new.$$" && chmod +x "$BIN.new.$$" && mv "$BIN.new.$$" "$BIN" || fail_deploy "cannot install the new binary at $BIN"

  integ=$(printf '%s\n' "$ROOTS" | head -n 1)/hooks/ah-engine-integrity.sh
  if [ -f "$integ" ]; then
    ( HOME=$H AH_ENGINE_DIR=$ENGINE_DIR AH_ENGINE_PLUGIN_ROOT=$(printf '%s\n' "$ROOTS" | head -n 1); export HOME AH_ENGINE_DIR AH_ENGINE_PLUGIN_ROOT; lim sh "$integ" accept >/dev/null 2>&1 )
    step "integrity record accepted ($integ)"
  else step "no $integ: integrity record not updated"; fi

  mkscratch
  if verify_live; then step "DEPLOY OK: new binary and defaults live; undo with: sh $0 --rollback --yes"; exit 0; fi
  fail_deploy "verification failed (see the lines above)"
}

# --- rollback ---------------------------------------------------------------------------------------------------------
do_rollback() {
  from=$1; YES=$2
  [ -n "$from" ] || { [ -f "$BK_ROOT/latest" ] && from=$BK_ROOT/$(cat "$BK_ROOT/latest"); }
  [ -n "$from" ] && [ -f "$from/manifest.tsv" ] || die "no deploy backup to roll back (looked in $BK_ROOT); use --from <backup-dir>"
  note "== rollback of $from"
  nf=$(grep -c '^F' "$from/manifest.tsv"); note "restore $nf defaults file(s) from the backup (files the deploy added are moved aside, not deleted)"
  if [ -e "$BIN.prev" ]; then note "restore the binary: $BIN.prev -> $BIN"; else note "no $BIN.prev: the binary stays as it is"; fi
  if [ "$YES" != 1 ]; then note "dry run: nothing was changed. Add --yes to roll back."; exit 0; fi
  RB=$from/rollback-$(date -u +%Y%m%dT%H%M%SZ); mkdir -p "$RB/replaced" "$RB/added" || die "cannot create $RB"
  DLOG=$RB/rollback.log
  step "rollback start"
  rr=
  while IFS="$(printf '\t')" read -r kind a b c d; do
    [ "$kind" = F ] && rr=${rr:-$a}
  done <"$from/manifest.tsv"
  ( HOME=$H AH_ENGINE_DIR=$ENGINE_DIR; export HOME AH_ENGINE_DIR; [ -n "$rr" ] && AH_ENGINE_PLUGIN_ROOT=$(dirname "$(dirname "$rr")") && export AH_ENGINE_PLUGIN_ROOT; lim "$BIN" stop >/dev/null 2>&1 ) || step "stop returned non-zero; continuing"
  k=0
  while IFS="$(printf '\t')" read -r kind dst file had bk; do
    [ "$kind" = F ] || continue
    k=$((k + 1))
    mkdir -p "$RB/replaced/$k" "$RB/added/$k"
    if [ -e "$dst/$file" ]; then
      if [ "$had" = 1 ]; then cp -p "$dst/$file" "$RB/replaced/$k/$file" || die "cannot keep $dst/$file"
      else mv "$dst/$file" "$RB/added/$k/$file" || die "cannot move $dst/$file aside"; fi
    fi
    if [ "$had" = 1 ]; then
      cp -p "$bk" "$dst/.$file.rollback.$$" && mv "$dst/.$file.rollback.$$" "$dst/$file" || die "cannot restore $dst/$file from $bk"
    fi
  done <"$from/manifest.tsv"
  step "defaults restored ($k file(s))"
  if [ -e "$BIN.prev" ]; then
    cp -p "$BIN" "$RB/replaced/ah-engine" 2>/dev/null
    cp -p "$BIN.prev" "$BIN.rb.$$" && mv "$BIN.rb.$$" "$BIN" || die "cannot restore the binary from $BIN.prev"
    step "binary restored from $BIN.prev (sha $(sha "$BIN" | cut -c1-12))"
    r1=${rr:+$(dirname "$(dirname "$rr")")}
    if [ -n "$r1" ] && [ -f "$r1/hooks/ah-engine-integrity.sh" ]; then
      ( HOME=$H AH_ENGINE_DIR=$ENGINE_DIR AH_ENGINE_PLUGIN_ROOT=$r1; export HOME AH_ENGINE_DIR AH_ENGINE_PLUGIN_ROOT; lim sh "$r1/hooks/ah-engine-integrity.sh" accept >/dev/null 2>&1 )
      step "integrity record accepted for the restored binary"
    fi
  fi
  step "ROLLBACK OK: the next hook call starts the restored engine"
  exit 0
}

# --- main -------------------------------------------------------------------------------------------------------------
usage() {
  sed -n '2,6p' "$0" | sed 's/^# \{0,1\}//' >&2
  exit 2
}
mode=; YES=0; FROM=; pos1=; pos2=
while [ $# -gt 0 ]; do
  case $1 in
    --check|--deploy|--rollback) [ -z "$mode" ] || usage; mode=$1 ;;
    --yes) YES=1 ;;
    --allow-drop) ALLOW_DROP=1 ;;
    --from) shift; FROM=${1:-}; [ -n "$FROM" ] || usage ;;
    -h|--help) usage ;;
    -*) usage ;;
    *) if [ -z "$pos1" ]; then pos1=$1; elif [ -z "$pos2" ]; then pos2=$1; else usage; fi ;;
  esac
  shift
done
case $mode in
  --check) [ -n "$pos2" ] || usage; do_check "$pos1" "$pos2"; exit $? ;;
  --deploy) [ -n "$pos2" ] || usage; do_deploy "$pos1" "$pos2" "$YES" ;;
  --rollback) do_rollback "$FROM" "$YES" ;;
  *) usage ;;
esac
