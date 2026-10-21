#!/bin/sh
# thin launcher shim: hand the call to the Rust launcher when it is installed; otherwise (or for --doctor, which the launcher
# itself delegates back here) run this wrapper unchanged. Builtins only (no dirname/cd/pwd forks) on the hot path.
ah_l=${HOME:-}/.anti-hall/ah-engine/bin/ah-hook
if [ -x "$ah_l" ]; then
  case " $* " in
    *" --doctor "*) ;;
    *)
      case $0 in */*) ah_r=${0%/*}/.. ;; *) ah_r=.. ;; esac
      exec "$ah_l" "$@" --plugin-root "$ah_r"
      ;;
  esac
fi
# Reliability wrapper for `ah-engine hook --event <Event>`.
#
# Outcome table (this wrapper launches no hook command; the engine decides every event):
# - engine exit 0: exact answer, pass stdout/stderr and exit 0.
# - engine exit 2: exact block, pass stdout/stderr and exit 2.
# - any other engine exit after it marked the dispatch done (AH_ENGINE_DONE_FILE): a hook's own exit code (a non-blocking error),
#   passed through with its stdout/stderr; without the mark it is an engine failure and the call passes through.
# - engine unreachable (missing, crashed, timed out, signal death, spawn failure, exit 75 or any other unmarked exit): the call
#   passes through unchecked (exit 0) with ONE stderr notice per session (msg.engine_unreachable_notice).
# Arguments: the event, then optionally `--host claude|codex` (default claude: picks the host's table in the engine and the
# per-event timeout list) and `--tool-from-payload` (implied on PreToolUse, PostToolUse, PostToolUseFailure and PermissionRequest).
# Tool selection is payload-only: the tool argument is --tool-from-payload, which reads tool_name structurally from
# stdin. There is no --tool X: a caller-named tool could narrow the checks run and skip a guard the payload would select.
# Any other argument is ignored.
# Test-only knobs (honored ONLY when AH_WRAPPER_TEST=1 is also set; otherwise ignored with a one-line
# stderr note): AH_ENGINE_BIN, AH_FALLBACK_LIST, AH_HOOK_TIMEOUT_S, AH_HOOK_TIMEOUT_MS, AH_KILL_GRACE_S,
# AH_HOOK_SWEEP_AGE_S. The engine is otherwise located only at $HOME/.anti-hall/ah-engine/bin/ah-engine
# or on PATH. Honored values are validated so bad values cannot turn a guard into a silent allow.

# `ah-hook.sh --doctor [flags]`: the shell doctor (hooks/ah-doctor.sh), the diagnosis that needs neither the engine nor Node. It is
# not an event, so it is handled before anything else (the test-only variables below are not read for it).
if [ "${1:-}" = --doctor ]; then
  shift
  doctor_dir=$(CDPATH= cd -- "$(dirname "$0")" 2>/dev/null && pwd) || doctor_dir=.
  if [ -f "$doctor_dir/ah-doctor.sh" ]; then
    exec sh "$doctor_dir/ah-doctor.sh" "$@"
  fi
  printf 'anti-hall: ah-doctor.sh is missing next to ah-hook.sh; reinstall the plugin\n' >&2
  exit 1
fi

event=$1
shift 1 2>/dev/null || true

# The test-only knobs are honored only when AH_WRAPPER_TEST=1 is also set (the test suite exports it).
# A stray AH_ENGINE_BIN=/usr/bin/true in a user's shell must not turn the safety net into a silent allow.
if [ "${AH_WRAPPER_TEST:-}" != 1 ]; then
  ignored_knobs=
  for knob in AH_ENGINE_BIN AH_FALLBACK_LIST AH_HOOK_TIMEOUT_S AH_HOOK_TIMEOUT_MS AH_KILL_GRACE_S AH_HOOK_SWEEP_AGE_S; do
    eval "knob_val=\${$knob:-}"
    if [ -n "$knob_val" ]; then
      ignored_knobs="$ignored_knobs $knob"
    fi
    unset "$knob"
  done
  if [ -n "$ignored_knobs" ]; then
    printf 'anti-hall: ignoring test-only variables (need AH_WRAPPER_TEST=1):%s\n' "$ignored_knobs" >&2
  fi
fi
# D87: the hooks.json of each host runs this wrapper once per event, `ah-hook.sh <Event> [--host codex]`, and the engine
# decides which checks apply. If the engine cannot answer, this wrapper passes the call through with one notice per session.
# On the events whose hooks are matched by tool name the tool is read from the payload structurally, as if --tool-from-payload
# had been given and handed to the engine.
tool_from_payload=0
case "$event" in
  PreToolUse|PostToolUse|PostToolUseFailure|PermissionRequest) tool_from_payload=1 ;;
esac
host=claude
while [ "$#" -gt 0 ]; do
  case "$1" in
    --tool-from-payload) tool_from_payload=1 ;;
    --host)
      case "${2:-}" in
        claude|codex) host=$2; shift ;;
        *) printf 'anti-hall: ignoring unknown --host value\n' >&2 ;;
      esac
      ;;
  esac
  shift
done

dir=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
# The engine reads its settings, tables, messages and rules from THIS plugin's engine/ directory at run time (nothing is compiled
# into the binary), so it must be told which plugin is running: the one this wrapper lives in. An explicit
# AH_ENGINE_PLUGIN_ROOT (an operator override) wins.
if [ -z "${AH_ENGINE_PLUGIN_ROOT:-}" ] && [ -f "$dir/../engine/defaults/index.toml" ]; then
  AH_ENGINE_PLUGIN_ROOT=$(CDPATH= cd -- "$dir/.." && pwd)
  export AH_ENGINE_PLUGIN_ROOT
fi
# the per-event hook timeouts of the host (the `@<Event>` rows of its list file); no command in it is ever run
case "$host" in
  codex) list_default=$dir/ah-fallback.codex.list ;;
  *) list_default=$dir/ah-fallback.list ;;
esac
list=${AH_FALLBACK_LIST:-"$list_default"}
uid=$(id -u 2>/dev/null || printf '0')
self_pid=$$
tmp=
payload=
children=

guard_event=0
case "$event" in
  PreToolUse|PermissionRequest|Stop|SubagentStop) guard_event=1 ;;
esac

cleanup() {
  trap - 0 HUP INT TERM
  for p in $children; do
    terminate_process_group "$p" 1 2>/dev/null || true
  done
  if [ -n "$tmp" ] && [ -d "$tmp" ]; then
    rm -rf "$tmp"
  fi
}
trap 'cleanup; exit 129' HUP
trap 'cleanup; exit 130' INT
trap 'cleanup; exit 143' TERM
trap 'cleanup' 0

fail_closed() {
  if is_stop_active_payload 2>/dev/null; then
    printf 'anti-hall: fail open for %s because stop_hook_active is true: %s\n' "$event" "$1" >&2
    exit 0
  fi
  printf 'anti-hall: fail closed for %s: %s\n' "$event" "$1" >&2
  exit 2
}

# The engine is unreachable (missing, crashed, timed out, no verdict at all): the call passes through unchecked, with one stderr
# notice per session (the state dir remembers the session id that already got it). Never blocks, never launches anything.
unreachable_passthrough() {
  un_text=$(engine_default_text msg.engine_unreachable_notice "${defaults_file%/*}/messages.toml" || true)
  if [ -n "$un_text" ]; then
    un_name=$(engine_default_text files.unreachable_notice || true)
    un_sid=$(sed -n 's/.*"session_id"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$payload" 2>/dev/null | head -n 1)
    [ -n "$un_sid" ] || un_sid=none
    un_show=1
    if [ -n "$un_name" ]; then
      un_dir=${AH_ENGINE_DIR:-"$HOME/.anti-hall/ah-engine"}
      if [ "$(cat "$un_dir/$un_name" 2>/dev/null)" = "$un_sid" ]; then
        un_show=0
      else
        mkdir -p "$un_dir" 2>/dev/null && printf '%s\n' "$un_sid" >"$un_dir/$un_name" 2>/dev/null
      fi
    fi
    [ "$un_show" -eq 0 ] || printf '%s\n' "$un_text" | sed "s/{event}/$event/g; s/{why}/$1/g" >&2
  fi
  exit 0
}

validate_positive_int() {
  value=$1
  name=$2
  case "$value" in
    ""|*[!0-9]*|0)
      [ "$guard_event" -eq 1 ] && fail_closed "$name must be a positive integer"
      printf 'anti-hall: %s must be a positive integer; skipping non-guard hook\n' "$name" >&2
      exit 0
      ;;
  esac
}

defaults_file=${AH_ENGINE_PLUGIN_ROOT:-"$dir/.."}/engine/defaults/engine.toml
engine_default_num_field() {
  key=$1
  field=$2
  [ -f "$defaults_file" ] || return 1
  awk -v want="[$key]" -v field="$field" '
    $0 == want { in_key = 1; next }
    in_key && /^\[/ { exit }
    in_key && $1 == field && $2 == "=" {
      v = $3
      gsub(/[^0-9].*/, "", v)
      if (v != "") {
        print v
        exit
      }
    }
  ' "$defaults_file"
}

engine_default_num() {
  engine_default_num_field "$1" value
}

engine_default_min() {
  engine_default_num_field "$1" min
}

engine_default_max() {
  engine_default_num_field "$1" max
}

engine_default_env() {
  key=$1
  [ -f "$defaults_file" ] || return 1
  awk -v want="[$key]" '
    $0 == want { in_key = 1; next }
    in_key && /^\[/ { exit }
    in_key && $1 == "env" && $2 == "=" {
      v = $0
      sub(/^[^"]*"/, "", v)
      sub(/".*$/, "", v)
      if (v != "") {
        print v
        exit
      }
    }
  ' "$defaults_file"
}

engine_default_text() {
  key=$1
  text_file=${2:-$defaults_file}
  [ -f "$text_file" ] || return 1
  awk -v want="[$key]" '
    $0 == want { in_key = 1; next }
    in_key && /^\[/ { exit }
    in_key && $1 == "value" && $2 == "=" {
      v = $0
      sub(/^[^"]*"/, "", v)
      sub(/".*$/, "", v)
      if (v != "") {
        print v
        exit
      }
    }
  ' "$text_file"
}

engine_effective_num() {
  key=$1
  env_name=$(engine_default_env "$key" || true)
  if [ -n "$env_name" ]; then
    eval "env_value=\${$env_name:-}"
    # Rust (defaults::num) trims the override and takes it only when it parses as a number; anything else keeps the default
    env_value=$(printf '%s' "$env_value" | tr -d ' \t\r\n')
    case "$env_value" in
      ""|*[!0-9]*) v=$(engine_default_num "$key") || return 1 ;;
      *) v=$env_value ;;
    esac
  else
    v=$(engine_default_num "$key") || return 1
  fi
  case "$v" in
    ""|*[!0-9]*)
      printf '%s\n' "$v"
      return 0
      ;;
  esac
  min_v=$(engine_default_min "$key" || true)
  max_v=$(engine_default_max "$key" || true)
  case "$min_v" in
    ""|*[!0-9]*) ;;
    *) [ "$v" -lt "$min_v" ] && v=$min_v ;;
  esac
  case "$max_v" in
    ""|*[!0-9]*) ;;
    *) [ "$v" -gt "$max_v" ] && v=$max_v ;;
  esac
  printf '%s\n' "$v"
  return 0
}

engine_default_sleep() {
  ms=$1
  awk -v ms="$ms" 'BEGIN { printf "%.3f", ms / 1000 }'
}

now_ms() {
  perl -MTime::HiRes=time -e 'printf "%.0f\n", time() * 1000' 2>/dev/null || awk 'BEGIN { srand(); print systime() * 1000 }'
}

# True while the state-dir marker named by the files.<key> default holds a time (ms since the epoch) still in the future.
fresh_state_marker() {
  marker_name=$(engine_default_text "files.$1" || true)
  [ -n "$marker_name" ] || return 1
  marker_dir=${AH_ENGINE_DIR:-"$HOME/.anti-hall/ah-engine"}
  [ -r "$marker_dir/$marker_name" ] || return 1
  # the engine writes the time without a newline: `s///p` alone prints nothing when there is no non-digit to strip
  marker_until=$(sed -n '1{s/[^0-9].*$//;p;}' "$marker_dir/$marker_name" 2>/dev/null)
  case "$marker_until" in
    ""|*[!0-9]*) return 1 ;;
  esac
  [ "$marker_until" -gt "$(now_ms)" ]
}

fresh_daemon_down_marker() {
  fresh_state_marker daemon_down_marker
}

case "${AH_KILL_GRACE_S:-1}" in
  0|1|2|3|4|5) kill_grace=${AH_KILL_GRACE_S:-1} ;;
  *) kill_grace=1 ;;
esac

timeout_env=${AH_HOOK_TIMEOUT_S:-10}
validate_positive_int "$timeout_env" AH_HOOK_TIMEOUT_S
timeout_ms_env=${AH_HOOK_TIMEOUT_MS:-}
if [ -n "$timeout_ms_env" ]; then
  validate_positive_int "$timeout_ms_env" AH_HOOK_TIMEOUT_MS
elif [ -n "${AH_HOOK_TIMEOUT_S:-}" ]; then
  # an explicit seconds override beats the shipped client deadline (the test suite pins a load-proof budget this way)
  timeout_ms_env=$((timeout_env * 1000))
else
  client_deadline_ms=$(engine_effective_num client.deadline_ms || true)
  client_deadline_slack_ms=$(engine_default_num client.deadline_slack_ms || true)
  wrapper_extra_ms=$(engine_default_num client.wrapper_extra_ms || true)
  if [ -n "$client_deadline_ms$client_deadline_slack_ms$wrapper_extra_ms" ]; then
    validate_positive_int "$client_deadline_ms" client.deadline_ms
    validate_positive_int "$client_deadline_slack_ms" client.deadline_slack_ms
    validate_positive_int "$wrapper_extra_ms" client.wrapper_extra_ms
    timeout_ms_env=$((client_deadline_ms + client_deadline_slack_ms + wrapper_extra_ms))
  else
    timeout_ms_env=$((timeout_env * 1000))
  fi
fi
# The poll interval comes from the shipped defaults. An incomplete install (defaults unreadable) must not block a guard event before
# the engine-absent fail-open is reached: it falls back to the same last-resort 1 s granularity the timeouts above use
# (AH_HOOK_TIMEOUT_S default), and an unusable value in a readable file is still rejected by validation.
watchdog_poll_ms=$(engine_default_num client.wrapper_watchdog_poll_ms || true)
if [ -z "$watchdog_poll_ms" ] && [ ! -r "$defaults_file" ]; then
  watchdog_poll_ms=1000
fi
validate_positive_int "$watchdog_poll_ms" client.wrapper_watchdog_poll_ms
watchdog_poll_s=$(engine_default_sleep "$watchdog_poll_ms")
sweep_age=${AH_HOOK_SWEEP_AGE_S:-1800}
validate_positive_int "$sweep_age" AH_HOOK_SWEEP_AGE_S

# Detect the stat flavour ONCE and use only that form. GNU/busybox stat accepts `-c`; BSD/macOS stat does not. Never try the
# BSD form first: on GNU `stat -f` means --file-system, takes the format string as a FILE operand and prints filesystem info
# for the real path to stdout before failing, which corrupts the captured output.
if stat -c %u / >/dev/null 2>&1; then
  stat_gnu=1
else
  stat_gnu=0
fi

stat_pair() {
  if [ "$stat_gnu" -eq 1 ]; then
    stat -c '%u %a' "$1" 2>/dev/null
  else
    stat -f '%u %Mp%Lp' "$1" 2>/dev/null
  fi
}

stat_mtime() {
  if [ "$stat_gnu" -eq 1 ]; then
    stat -c '%Y' "$1" 2>/dev/null
  else
    stat -f '%m' "$1" 2>/dev/null
  fi
}

# Verify and create a private directory: final component not a symlink, owned by us, mode 700.
# $1 = physical parent (already canonical), $2 = final component name.
private_dir_in() {
  pd_parent=$1
  pd_base=$pd_parent/$2
  [ ! -L "$pd_base" ] || return 1
  old_umask=$(umask)
  umask 077
  mkdir -p -m 700 "$pd_base" 2>/dev/null || {
    umask "$old_umask"
    return 1
  }
  umask "$old_umask"
  [ -d "$pd_base" ] && [ ! -L "$pd_base" ] || return 1
  set -- $(stat_pair "$pd_base") || return 1
  [ "${1:-}" = "$uid" ] || return 1
  pd_mode=${2:-}
  [ "$pd_mode" = 700 ] || [ "$pd_mode" = 0700 ] || return 1
  printf '%s\n' "$pd_base"
}

# A shared parent (XDG dir, TMPDIR, /tmp): canonicalise it physically (macOS /tmp is a symlink), then
# refuse a world-writable parent unless it has the sticky bit.
shared_parent_ok() {
  sp_parent=$1
  set -- $(stat_pair "$sp_parent") || return 1
  sp_mode=${2:-}
  case "$sp_mode" in
    *[2367]) ;;
    *) return 0 ;;
  esac
  [ "${#sp_mode}" -eq 4 ] || return 1
  case "$sp_mode" in
    [1357]*) return 0 ;;
  esac
  return 1
}

private_tmp_base() {
  # 1. XDG_RUNTIME_DIR (per-user, private by definition)
  if [ -n "${XDG_RUNTIME_DIR:-}" ] && [ -d "$XDG_RUNTIME_DIR" ]; then
    cand=$(CDPATH= cd -- "$XDG_RUNTIME_DIR" 2>/dev/null && pwd -P) || cand=
    if [ -n "$cand" ] && shared_parent_ok "$cand"; then
      private_dir_in "$cand" "ah-hook-$uid" && return 0
    fi
  fi
  # 2. $HOME/.anti-hall/tmp (our own tree, never shared)
  case "${HOME:-}" in
    /*)
      old_umask=$(umask)
      umask 077
      mkdir -p -m 700 "$HOME/.anti-hall" 2>/dev/null || true
      umask "$old_umask"
      cand=$(CDPATH= cd -- "$HOME/.anti-hall" 2>/dev/null && pwd -P) || cand=
      if [ -n "$cand" ]; then
        set -- $(stat_pair "$cand") || set --
        if [ "${1:-}" = "$uid" ]; then
          private_dir_in "$cand" tmp && return 0
        fi
      fi
      ;;
  esac
  # 3. TMPDIR or /tmp: shared parent, so check it carefully
  parent=${TMPDIR:-/tmp}
  cand=$(CDPATH= cd -- "$parent" 2>/dev/null && pwd -P) || return 1
  shared_parent_ok "$cand" || return 1
  private_dir_in "$cand" "ah-hook-$uid"
}

# Remove stale run dirs: older than AH_HOOK_SWEEP_AGE_S, or (name carries the owner pid) whose owning
# wrapper is gone, e.g. SIGKILLed before it could clean up. Bounded: at most 200 dirs are examined.
sweep_old_runs() {
  base=$1
  now=$(date +%s)
  seen_dirs=0
  for d in "$base"/ah-wrapper-run.*; do
    [ -d "$d" ] || continue
    [ ! -L "$d" ] || continue
    seen_dirs=$((seen_dirs + 1))
    [ "$seen_dirs" -le 200 ] || break
    set -- $(stat_pair "$d") || continue
    [ "${1:-}" = "$uid" ] || continue
    owner=${d##*/ah-wrapper-run.}
    owner=${owner%%.*}
    case "$owner" in
      ""|*[!0-9]*) ;;
      *)
        if [ "$owner" != "$self_pid" ] && ! kill -0 "$owner" 2>/dev/null; then
          rm -rf "$d"
          continue
        fi
        ;;
    esac
    mt=$(stat_mtime "$d" 2>/dev/null || printf '')
    case "$mt" in ""|*[!0-9]*) continue ;; esac
    age=$((now - mt))
    if [ "$age" -gt "$sweep_age" ]; then
      rm -rf "$d"
    fi
  done
}

make_payload_file() {
  base=$(private_tmp_base) || return 1
  sweep_old_runs "$base"
  old_umask=$(umask)
  umask 077
  tmp=$(mktemp -d "$base/ah-wrapper-run.$self_pid.XXXXXXXXXX" 2>/dev/null) || {
    umask "$old_umask"
    return 1
  }
  payload=$tmp/payload
  : >"$payload" || {
    umask "$old_umask"
    return 1
  }
  chmod 600 "$payload" 2>/dev/null || true
  cat >"$payload"
  umask "$old_umask"
  return 0
}

# Degraded path: the private base is unavailable. mktemp -d makes an unpredictable name, O_EXCL, mode 700, so it is safe even in
# a shared sticky dir; owner and mode are still verified.
make_payload_file_degraded() {
  for dbase in "${TMPDIR:-}" /tmp /var/tmp /dev/shm "${HOME:-}"; do
    [ -n "$dbase" ] && [ -d "$dbase" ] && [ -w "$dbase" ] || continue
    old_umask=$(umask)
    umask 077
    tmp=$(mktemp -d "$dbase/ah-wrapper-run.$self_pid.XXXXXXXXXX" 2>/dev/null) || {
      umask "$old_umask"
      tmp=
      continue
    }
    set -- $(stat_pair "$tmp")
    if [ "${1:-}" = "$uid" ] && { [ "${2:-}" = 700 ] || [ "${2:-}" = 0700 ]; }; then
      payload=$tmp/payload
      if : >"$payload" 2>/dev/null && cat >"$payload"; then
        umask "$old_umask"
        return 0
      fi
    fi
    umask "$old_umask"
    rm -rf "$tmp" 2>/dev/null
    tmp=
    payload=
  done
  return 1
}

payload_mem=
mem_mode=0

if ! make_payload_file; then
  printf 'anti-hall: no private temp dir for %s; trying a degraded temp dir\n' "$event" >&2
  if ! make_payload_file_degraded; then
    # Last resort: no file can be made anywhere. Hold the payload in memory and pipe it to each hook. The guards still decide.
    printf 'anti-hall: no temp dir at all for %s; running hooks with an in-memory payload\n' "$event" >&2
    payload_mem=$(cat; printf x) || payload_mem=x
    payload_mem=${payload_mem%x}
    mem_mode=1
    tool_from_payload=0
  fi
fi

json_tool_name() {
  # byte semantics: gawk in a UTF-8 locale makes sprintf("%c",239) a two-byte character, so the BOM compare never matched
  LC_ALL=C awk '
    function hx(c) { return index("0123456789abcdef", tolower(c)) - 1 }
    function hexnum(h,    i,v,n) { n=0; for(i=1;i<=length(h);i++){ v=hx(substr(h,i,1)); if(v<0) return -1; n=n*16+v } return n }
    function ws(c) { return c==" " || c=="\t" || c=="\r" || c=="\n" }
    function skip_string(    j,k,bs) {
      j=i+1
      while(j<=n) {
        k=index(substr(s,j), "\"")
        if(k==0){ bad=1; i=n+1; return }
        j += k - 1
        bs=0
        while(j-bs-1>=i+1 && substr(s,j-bs-1,1)=="\\") bs++
        if(bs%2==0){ i=j; return }
        j++
      }
      bad=1; i=n+1
    }
    function esc(c,    h,n) {
      if (c=="\"" || c=="\\" || c=="/") return c
      if (c=="b") return "\b"; if (c=="f") return "\f"; if (c=="n") return "\n"; if (c=="r") return "\r"; if (c=="t") return "\t"
      if (c=="u") { h=substr(s,i+1,4); n=hexnum(h); if(length(h)!=4 || n<0){ bad=1; return "" }; i+=4; if(n<128) return sprintf("%c", n); return "?" }
      bad=1; return ""
    }
    { s=s $0 "\n" }
    END {
      sub(/\n$/, "", s); n=length(s); i=1; bad=0
      while(i<=n && ws(substr(s,i,1))) i++
      bom=sprintf("%c%c%c", 239, 187, 191)
      if(substr(s,i,3)==bom) i += 3
      if(substr(s,i,1)!="{"){ print "FAIL"; exit }
      depth=0; expect_key=0; after_key=0; want_value=0; key=""; found=0
      for(; i<=n; i++){
        c=substr(s,i,1)
        if(instr){
          if(back){ str=str esc(c); back=0; continue }
          if(c=="\\"){ back=1; continue }
          if(c=="\""){
            instr=0
            if(depth==1 && expect_key){ key=str; expect_key=0; after_key=1; continue }
            if(depth==1 && want_value){
              if(key=="tool_name"){ if(!found){ val=str; found=1 } else if(val!=str) bad=1 }
              want_value=0; key=""; continue
            }
            continue
          }
          str=str c; continue
        }
        if(ws(c)) continue
        if(c=="\""){
          if(depth==1 && expect_key){ instr=1; str=""; continue }
          if(depth==1 && want_value){
            if(key=="tool_name"){ instr=1; str=""; continue }
            skip_string(); want_value=0; key=""; continue
          }
          skip_string(); continue
        }
        if(c=="{"){ depth++; if(depth==1) expect_key=1; continue }
        if(c=="}"){ depth--; if(depth==0){ i++; break }; if(depth<0) bad=1; continue }
        if(c=="["){ if(depth==1 && want_value && key=="tool_name") bad=1; depth++; continue }
        if(c=="]"){ depth--; if(depth<1) bad=1; continue }
        if(depth==1 && after_key){ if(c!=":"){ bad=1; break }; after_key=0; want_value=1; continue }
        if(depth==1 && want_value){ if(key=="tool_name") bad=1; want_value=0; key="" }
        if(depth==1 && c==","){ expect_key=1; continue }
      }
      while(i<=n && ws(substr(s,i,1))) i++
      if(i<=n || depth!=0 || instr || back || bad || !found) print "FAIL"; else print "OK\t" val
    }
  ' "$payload"
}

is_stop_active_payload() {
  case "$event" in Stop|SubagentStop) ;; *) return 1 ;; esac
  # byte semantics: gawk in a UTF-8 locale makes sprintf("%c",239) a two-byte character, so the BOM compare never matched
  LC_ALL=C awk '
    function ws(c) { return c==" " || c=="\t" || c=="\r" || c=="\n" }
    function skip_string(    j,k,bs) {
      j=i+1
      while(j<=n) {
        k=index(substr(s,j), "\"")
        if(k==0){ bad=1; i=n+1; return }
        j += k - 1
        bs=0
        while(j-bs-1>=i+1 && substr(s,j-bs-1,1)=="\\") bs++
        if(bs%2==0){ i=j; return }
        j++
      }
      bad=1; i=n+1
    }
    { s=s $0 "\n" }
    END {
      sub(/\n$/, "", s); n=length(s); i=1
      while(i<=n && ws(substr(s,i,1))) i++
      bom=sprintf("%c%c%c", 239, 187, 191)
      if(substr(s,i,3)==bom) i += 3
      if(substr(s,i,1)!="{") exit 1
      depth=0; expect_key=0; after_key=0; want_value=0; key=""
      for(; i<=n; i++){
        c=substr(s,i,1)
        if(instr){
          if(back){ back=0; continue }
          if(c=="\\"){ back=1; continue }
          if(c=="\""){
            instr=0
            if(depth==1 && expect_key){ key=str; expect_key=0; after_key=1; continue }
            if(depth==1 && want_value){ want_value=0; key=""; continue }
          }
          str=str c
          continue
        }
        if(ws(c)) continue
        if(c=="\""){
          if(depth==1 && expect_key){ instr=1; str=""; continue }
          skip_string(); continue
        }
        if(c=="{"){ depth++; if(depth==1) expect_key=1; continue }
        if(c=="}"){ depth--; if(depth==0) break; continue }
        if(c=="[" ){ depth++; continue }
        if(c=="]" ){ depth--; continue }
        if(depth==1 && after_key){ if(c!=":") exit 1; after_key=0; want_value=1; continue }
        if(depth==1 && want_value){
          if(key=="stop_hook_active" && substr(s,i,4)=="true"){ exit 0 }
          want_value=0; key=""
        }
        if(depth==1 && c==","){ expect_key=1; continue }
      }
      exit 1
    }
  ' "$payload"
}

tool=
if [ "$tool_from_payload" -eq 1 ]; then
  extracted=$(json_tool_name 2>/dev/null || printf 'FAIL')
  case "$extracted" in
    OK*) tool=$(printf '%s\n' "$extracted" | sed 's/^OK	//') ;;
  esac
fi

engine=
if [ -n "${AH_ENGINE_BIN:-}" ]; then
  if [ -x "$AH_ENGINE_BIN" ]; then
    engine=$AH_ENGINE_BIN
  else
    engine=
  fi
elif [ -x "$HOME/.anti-hall/ah-engine/bin/ah-engine" ]; then
  engine=$HOME/.anti-hall/ah-engine/bin/ah-engine
else
  engine=$(command -v ah-engine 2>/dev/null || true)
fi

# The integrity script (ah-engine-integrity.sh) counts engine failures and rolls a damaged, never-healthy binary back to the known-good
# one. Detached and silent: it never delays the hook. Only for the installed engine, never for an AH_ENGINE_BIN test double.
integrity_run() {
  [ -z "${AH_ENGINE_BIN:-}" ] && [ -f "$dir/ah-engine-integrity.sh" ] || return 0
  { sh "$dir/ah-engine-integrity.sh" "$1" </dev/null >/dev/null 2>&1 & } 2>/dev/null
  return 0
}

[ "$event" = SessionStart ] && integrity_run check

# Keep the engine installed and current: once per SessionStart, detached (never delays or fails the hook). The script
# compares the plugin's ah-engine.lock with what is installed and does nothing when they match; see ah-engine-bootstrap.sh.
if [ "$event" = SessionStart ] && [ -z "${AH_ENGINE_BIN:-}" ] && [ -f "$dir/../ah-engine.lock" ] && [ -f "$dir/ah-engine-bootstrap.sh" ]; then
  { sh "$dir/ah-engine-bootstrap.sh" </dev/null >/dev/null 2>&1 & } 2>/dev/null
fi

event_timeout() {
  t=$(awk -F '	' -v ev="$event" '$1 == "@" ev { print $2; found = 1; exit } END { if (!found) print "" }' "$list" 2>/dev/null)
  case "$t" in
    ""|*[!0-9]*|0) printf '%s\n' "$t" ;;
    *) if [ "$t" -gt "$timeout_env" ]; then printf '%s\n' "$timeout_env"; else printf '%s\n' "$t"; fi ;;
  esac
}

event_timeout_ms() {
  t=$(event_timeout)
  case "$t" in
    ""|*[!0-9]*|0) printf '%s\n' "$timeout_ms_env" ;;
    *)
      ms=$((t * 1000))
      if [ "$ms" -gt "$timeout_ms_env" ]; then printf '%s\n' "$timeout_ms_env"; else printf '%s\n' "$ms"; fi
      ;;
  esac
}

# the hook's own timeout in seconds: the listed event timeout, capped by the wrapper's seconds budget
event_timeout_secs() {
  t=$(event_timeout)
  case "$t" in
    ""|*[!0-9]*|0) printf '%s\n' "$timeout_env" ;;
    *) printf '%s\n' "$t" ;;
  esac
}

collect_descendants() {
  root_pid=$1
  seen=$tmp/kill.seen.$root_pid.$$
  new=$tmp/kill.new.$root_pid.$$
  desc=$tmp/kill.desc.$root_pid.$$
  scan_count=0
  max_scans=64
  printf '%s\n' "$root_pid" >"$seen"
  : >"$desc"
  while [ "$scan_count" -lt "$max_scans" ]; do
    scan_count=$((scan_count + 1))
    ps -Ao pid=,ppid= 2>/dev/null | awk '
      NR == FNR { seen[$1] = 1; next }
      seen[$2] && !seen[$1] { print $1 }
    ' "$seen" - >"$new"
    [ -s "$new" ] || break
    cat "$new" >>"$seen"
    cat "$new" >>"$desc"
  done
  cat "$desc"
  rm -f "$seen" "$new" "$desc"
}

kill_process_tree() {
  root_pid=$1
  descendants=$(collect_descendants "$root_pid")
  leaf_first=$(printf '%s\n' "$descendants" | awk 'NF { p[NR] = $1 } END { for (i = NR; i >= 1; i--) print p[i] }')
  for child_pid in $leaf_first "$root_pid"; do
    kill -TERM "$child_pid" 2>/dev/null || true
  done
  sleep "$kill_grace"
  for child_pid in $leaf_first "$root_pid"; do
    kill -KILL "$child_pid" 2>/dev/null || true
  done
}

terminate_process_group() {
  root_pid=$1
  group_pid=$2
  if [ "$group_pid" -eq 1 ]; then
    kill -TERM "-$root_pid" 2>/dev/null || true
  fi
  kill_process_tree "$root_pid"
  if [ "$group_pid" -eq 1 ]; then
    kill -KILL "-$root_pid" 2>/dev/null || true
  fi
}

# The host stderr parked on fd 4 by run_engine stays in the wrapper: a child holding it would keep the host's stderr pipe open
# (host waits for EOF) until a detached daemon exits, defeating the client deadline. Every child launch closes it (4>&-).
launch_argv_group() {
  out=$1
  err=$2
  pid_file=$3
  group_file=$4
  shift 4
  if command -v setsid >/dev/null 2>&1; then
    setsid "$@" <"$payload" >"$out" 2>"$err" 4>&- &
    printf '%s\n' "$!" >"$pid_file"
    printf '1\n' >"$group_file"
    return
  fi
  if ( set -m ) 2>/dev/null; then
    set -m 2>/dev/null || true
    { "$@" <"$payload" >"$out" 2>"$err" 4>&- & } 2>/dev/null
    printf '%s\n' "$!" >"$pid_file"
    printf '1\n' >"$group_file"
    set +m 2>/dev/null || true
    return
  fi
  "$@" <"$payload" >"$out" 2>"$err" 4>&- &
  printf '%s\n' "$!" >"$pid_file"
  printf '0\n' >"$group_file"
}

remove_child() {
  gone=$1
  children=$(printf '%s\n' "$children" | awk -v p="$gone" '{ for (i=1;i<=NF;i++) if ($i != p) printf "%s%s", sep, $i; print "" }')
}

# Optional 5th/6th args: a wait-marker file and an absolute bound (ms from the start). When the budget runs out and the marker
# exists (the engine client is waiting on the daemon or its Node hooks are deciding), the deadline moves once to the bound.
start_watchdog_ms() {
  w_pid=$1; w_timeout_ms=$2; w_group=$3; w_timed=$4; w_slow=${5:-}; w_slow_max=${6:-0}
  (
    trap - 0 HUP INT TERM
    w_t0=$(now_ms)
    deadline_ms=$((w_t0 + w_timeout_ms))
    w_extended=0
    while :; do
      remaining_ms=$((deadline_ms - $(now_ms)))
      if [ "$remaining_ms" -le 0 ]; then
        if [ "$w_extended" -eq 0 ] && [ -n "$w_slow" ] && [ "$w_slow_max" -gt "$w_timeout_ms" ] && [ -e "$w_slow" ] && kill -0 "$w_pid" 2>/dev/null; then
          w_extended=1
          deadline_ms=$((w_t0 + w_slow_max))
          continue
        fi
        break
      fi
      if [ "$remaining_ms" -lt "$watchdog_poll_ms" ]; then
        sleep "$(engine_default_sleep "$remaining_ms")"
      else
        sleep "$watchdog_poll_s"
      fi
      kill -0 "$w_pid" 2>/dev/null || exit 0
      if ! kill -0 "$self_pid" 2>/dev/null; then
        terminate_process_group "$w_pid" "$w_group"
        [ -n "$tmp" ] && rm -rf "$tmp"
        exit 0
      fi
    done
    : >"$w_timed"
    terminate_process_group "$w_pid" "$w_group"
  ) </dev/null >/dev/null 2>&1 4>&- &
  watch=$!
}

# After the watched pid exited: if the watchdog already fired, let it finish its kill; else just leave it.
settle_watchdog() {
  [ -f "$1" ] && wait "$watch" 2>/dev/null
  return 0
}

wrapper_diag() {
  [ "${AH_WRAPPER_TEST:-}" = 1 ] || return 0
  [ -n "${AH_ENGINE_WRAPPER_DIAG:-}" ] || return 0
  case $AH_ENGINE_WRAPPER_DIAG in
    /*) ;;
    *) return 0 ;;
  esac
  printf '%s\n' "$*" >>"$AH_ENGINE_WRAPPER_DIAG" 2>/dev/null || true
}

run_engine() {
  eng_out=$tmp/engine.out; eng_err=$tmp/engine.err; eng_timed=$tmp/engine.timed; eng_done=$tmp/engine.done
  rm -f "$eng_done"
  AH_ENGINE_DONE_FILE=$eng_done
  export AH_ENGINE_DONE_FILE
  timeout=$(event_timeout_ms)
  validate_positive_int "$timeout" engine_timeout_ms
  # A call that is waiting on the daemon, or running the Node hooks of a late/refused daemon reply, gets a bounded extension: the
  # engine client creates the wait file for exactly that time and the watchdog then waits up to client.slow_fallback_max_ms from the
  # start, never past the hook's own timeout. Anything else (no daemon, hung engine) keeps the fail-fast budget.
  eng_wait=$tmp/engine.wait
  rm -f "$eng_wait"
  wait_env=$(engine_default_text env.wait_file || true)
  slow_max=$(engine_effective_num client.slow_fallback_max_ms || true)
  case "$slow_max" in ""|*[!0-9]*) slow_max=0 ;; esac
  hook_cap_ms=$(( $(event_timeout_secs) * 1000 ))
  [ "$slow_max" -le "$hook_cap_ms" ] || slow_max=$hook_cap_ms
  if [ -n "$wait_env" ] && [ "$slow_max" -gt "$timeout" ]; then
    export "$wait_env=$eng_wait"
  else
    slow_max=0
  fi
  set -- "$engine" hook --event "$event"
  [ -n "$tool" ] && set -- "$@" --tool "$tool"
  set -- "$@" --host "$host"
  pid_file=$tmp/engine.pid; group_file=$tmp/engine.group
  # A signal-killed engine makes the shell print "Killed: 9" for the job to ITS stderr, at reap time (which can precede our
  # `wait`, so `wait 2>/dev/null` is not enough). The fail-open contract is silent, so the shell's stderr is parked while the
  # engine job runs and restored before anything is passed through.
  exec 4>&2 2>/dev/null
  launch_argv_group "$eng_out" "$eng_err" "$pid_file" "$group_file" "$@" || { exec 2>&4 4>&-; unreachable_passthrough "the engine could not be started"; }
  pid=$(cat "$pid_file"); group_pid=$(cat "$group_file")
  children="$children $pid"
  rm -f "$pid_file" "$group_file"
  start_watchdog_ms "$pid" "$timeout" "$group_pid" "$eng_timed" "$eng_wait" "$slow_max"
  wait "$pid" 2>/dev/null
  rc=$?
  settle_watchdog "$eng_timed"
  remove_child "$pid"
  exec 2>&4 4>&-
  if [ -f "$eng_timed" ]; then
    wrapper_diag "engine_timed event=$event timeout_ms=$timeout rc=$rc done=$([ -e "$eng_done" ] && printf 1 || printf 0)"
    integrity_run fail
    unreachable_passthrough "the engine did not answer in time"
  fi
  if [ "$group_pid" -eq 1 ] && kill -0 "-$pid" 2>/dev/null; then
    wrapper_diag "engine_group_alive event=$event timeout_ms=$timeout rc=$rc done=$([ -e "$eng_done" ] && printf 1 || printf 0)"
    integrity_run fail
    terminate_process_group "$pid" "$group_pid"
    unreachable_passthrough "the engine did not finish"
  fi
  case "$rc" in
    0|2) cat "$eng_out"; cat "$eng_err" >&2; exit "$rc" ;;
    75) integrity_run fail; unreachable_passthrough "engine handed the call back" ;;
    *)
      # The engine finished dispatching the event and its exit code is a hook's own (1, 3, ...): the host reads it as a non-blocking
      # hook error with its stderr, exactly as it would from the hook; answering it by running the hooks again would run each twice.
      if [ -e "$eng_done" ]; then
        cat "$eng_out"; cat "$eng_err" >&2; exit "$rc"
      fi
      wrapper_diag "engine_silent_exit event=$event timeout_ms=$timeout rc=$rc done=0"
      integrity_run fail
      unreachable_passthrough "the engine exited without an answer" ;;
  esac
}

# No temp dir at all: the payload is held in memory and piped to the engine; if the engine does not answer, the call passes through.
if [ "$mem_mode" -eq 1 ]; then
  if [ -n "$engine" ]; then
    mm_out=$(printf '%s' "$payload_mem" | "$engine" hook --event "$event" --host "$host"; echo "rc=$?")
    mm_rc=${mm_out##*rc=}
    mm_out=${mm_out%rc=*}
    case "$mm_rc" in
      0|2) printf '%s' "$mm_out"; exit "$mm_rc" ;;
    esac
  fi
  unreachable_passthrough "no temp dir"
fi

if [ "${AH_ENGINE_DISPATCH_IN_PROCESS:-}" != 1 ] && fresh_daemon_down_marker; then
  unreachable_passthrough "the engine is marked down"
fi

# Cold start (owner decision): a guard event does not run against a daemon that is still starting, where the engine would answer
# from its failure mode. It starts the daemon with an empty kick request, waits for the daemon socket up to
# client.cold_guard_wait_ms (never past what the hook's own timeout leaves after the engine attempt), then runs the real
# attempt. Not ready by the cap: exit 0 with one stderr notice per session. Prompts and advisories never wait. Without a
# readable config, a derivable socket path or a temp dir, there is no wait at all.
cold_guard_wait() {
  [ "$guard_event" -eq 1 ] && [ -n "$engine" ] && [ -n "$tmp" ] || return 0
  cg_cap=$(engine_default_num client.cold_guard_wait_ms || true)
  case "$cg_cap" in ""|*[!0-9]*|0) return 0 ;; esac
  cg_poll=$(engine_default_num client.cold_guard_poll_ms || true)
  cg_sock_max=$(engine_default_num paths.socket_max_len || true)
  cg_margin=$(engine_default_num client.wrapper_extra_ms || true)
  cg_kick=$(engine_default_text client.cold_guard_kick_event || true)
  cg_sock_name=$(engine_default_text paths.socket_file || true)
  for cg_v in "$cg_poll" "$cg_sock_max" "$cg_margin"; do
    case "$cg_v" in ""|*[!0-9]*|0) return 0 ;; esac
  done
  [ -n "$cg_kick" ] && [ -n "$cg_sock_name" ] || return 0
  # spawning is switched off (the engine's env.nospawn): no daemon can come up, so there is nothing to wait for
  cg_nospawn=$(engine_default_text env.nospawn || true)
  if [ -n "$cg_nospawn" ] && printenv "$cg_nospawn" >/dev/null 2>&1; then return 0; fi
  cg_sock=${AH_ENGINE_DIR:-"$HOME/.anti-hall/ah-engine"}/$cg_sock_name
  # a path the engine shortens through a hashed private dir cannot be derived here
  [ "${#cg_sock}" -le "$cg_sock_max" ] || return 0
  [ -S "$cg_sock" ] && return 0
  cg_eng=$(event_timeout_ms)
  validate_positive_int "$cg_eng" engine_timeout_ms
  cg_total=$(event_timeout)
  case "$cg_total" in ""|*[!0-9]*|0) cg_total=$timeout_env ;; esac
  cg_room=$((cg_total * 1000 - cg_eng - cg_margin))
  [ "$cg_room" -gt 0 ] || return 0
  [ "$cg_cap" -le "$cg_room" ] || cg_cap=$cg_room
  cg_start=$(now_ms)
  cold_guard_halted && return 0
  # the kick: the engine client finds no daemon and starts one (single-flight, crash-loop and breaker rules intact)
  real_payload=$payload
  payload=$tmp/cold-kick.json
  printf '{"hook_event_name":"%s"}' "$cg_kick" >"$payload" 2>/dev/null || { payload=$real_payload; return 0; }
  # the kick is the cold client: it parses the defaults before it starts the daemon (a release build needs ~0.6 s on a fresh state
  # directory), so it gets the whole wait, not the guard engine attempt's budget, or the watchdog kills it before the daemon exists
  # One absolute cold deadline: the kick, its kill grace and the socket poll all end by cg_start + cg_cap, so the whole cold path is
  # at most the wait plus the normal engine attempt. A kick cut by its watchdog costs the kill grace on top of its budget, so the
  # budget is the cap minus the grace; a cap that does not leave room for the grace kills the kick at once (no grace) instead.
  cg_grace_ms=$((kill_grace * 1000))
  cg_saved_grace=$kill_grace
  if [ "$cg_cap" -gt "$cg_grace_ms" ]; then
    cg_kick_ms=$((cg_cap - cg_grace_ms))
  else
    cg_kick_ms=$cg_cap
    kill_grace=0
  fi
  cg_kick_rc=1
  cg_kick_cut=0
  exec 4>&2 2>/dev/null
  if launch_argv_group "$tmp/kick.out" "$tmp/kick.err" "$tmp/kick.pid" "$tmp/kick.group" "$engine" hook --event "$cg_kick" --host "$host"; then
    cg_pid=$(cat "$tmp/kick.pid"); cg_group=$(cat "$tmp/kick.group")
    children="$children $cg_pid"
    rm -f "$tmp/kick.pid" "$tmp/kick.group" "$tmp/kick.timed"
    start_watchdog_ms "$cg_pid" "$cg_kick_ms" "$cg_group" "$tmp/kick.timed"
    wait "$cg_pid" 2>/dev/null
    cg_kick_rc=$?
    settle_watchdog "$tmp/kick.timed"
    [ -f "$tmp/kick.timed" ] && cg_kick_cut=1
    remove_child "$cg_pid"
    if [ "$cg_group" -eq 1 ] && kill -0 "-$cg_pid" 2>/dev/null; then
      terminate_process_group "$cg_pid" "$cg_group"
    fi
  fi
  exec 2>&4 4>&-
  kill_grace=$cg_saved_grace
  payload=$real_payload
  # the kick was cut by the watchdog: the single cold deadline (the kick budget plus its kill grace) has expired, so the call ends
  # here with the cold-start notice; a second, real engine attempt would run past the deadline
  if [ "$cg_kick_cut" -eq 1 ]; then
    wrapper_diag "cold_guard_gave_up event=$event cap_ms=$cg_cap kick_cut=1"
    cold_guard_notice
    exit 0
  fi
  # the kick failed (a binary that is missing, cannot run or exits at once): no daemon is coming, so the wait would only cost the
  # cap; the real attempt below fails open the usual way
  [ "$cg_kick_rc" -eq 0 ] || return 0
  while :; do
    [ -S "$cg_sock" ] && return 0
    cold_guard_halted && return 0
    [ $(( $(now_ms) - cg_start )) -lt "$cg_cap" ] || break
    sleep "$(engine_default_sleep "$cg_poll")"
  done
  wrapper_diag "cold_guard_gave_up event=$event cap_ms=$cg_cap"
  cold_guard_notice
  exit 0
}

# The engine itself will not start or answer (down marker, open breaker, crash-loop stop): waiting would only cost the cap.
cold_guard_halted() {
  fresh_daemon_down_marker || fresh_state_marker breaker_until || fresh_state_marker crashloop_until
}

# One stderr notice per session: the state dir remembers the session id that already got it.
cold_guard_notice() {
  cg_text=$(engine_default_text msg.cold_guard_notice "${defaults_file%/*}/messages.toml" || true)
  [ -n "$cg_text" ] || return 0
  cg_name=$(engine_default_text files.cold_guard_notice || true)
  cg_sid=$(sed -n 's/.*"session_id"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$payload" 2>/dev/null | head -n 1)
  [ -n "$cg_sid" ] || cg_sid=none
  if [ -n "$cg_name" ]; then
    cg_dir=${AH_ENGINE_DIR:-"$HOME/.anti-hall/ah-engine"}
    [ "$(cat "$cg_dir/$cg_name" 2>/dev/null)" = "$cg_sid" ] && return 0
    mkdir -p "$cg_dir" 2>/dev/null && printf '%s\n' "$cg_sid" >"$cg_dir/$cg_name" 2>/dev/null
  fi
  printf '%s\n' "$cg_text" | sed "s/{event}/$event/g" >&2
}

cold_guard_wait

if [ -n "$engine" ]; then
  run_engine
fi

unreachable_passthrough "no engine is installed"

