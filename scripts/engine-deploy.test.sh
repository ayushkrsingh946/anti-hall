#!/bin/sh
# Tests for engine-deploy.sh. Everything runs against a scratch HOME (fake ~/.claude, ~/.anti-hall, stub engine binaries): the
# real home, the real plugin roots and the live engine are never read or written. Run: sh scripts/engine-deploy.test.sh
set -u
HERE=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
SCRIPT=$HERE/engine-deploy.sh
T=$(mktemp -d "${TMPDIR:-/tmp}/engine-deploy-test.XXXXXX") || exit 1
PASS=0; FAIL=0
ok() { # name, then a command whose success is the assertion
  n=$1; shift
  if "$@" >/dev/null 2>&1; then PASS=$((PASS + 1)); printf 'ok   %s\n' "$n"; else FAIL=$((FAIL + 1)); printf 'FAIL %s\n' "$n"; fi
}
has() { grep -q -- "$1" "$2"; }
hasnt() { ! grep -q -- "$1" "$2"; }
sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }

# a stub engine: validates a plugin root the way the real binary does (refuses one lacking a key it reads); REQUIRES lists them
mk_engine() { # path, required keys
  cat >"$1" <<EOF
#!/bin/sh
# REQUIRES: $2
req=\$(sed -n 's/^# REQUIRES: //p' "\$0")
D=\${AH_ENGINE_DIR:-}
has_key() { grep -qs "^\\[\$2\\]" "\$1"/engine/defaults/*.toml "\$1"/engine/defaults.pristine/*.toml; }
missing() { for k in \$req; do has_key "\$1" "\$k" || { echo "\$k"; return 1; }; done; return 0; }
case \$1 in
  version) echo '{"version":"stub"}' ;;
  config) if [ "\$2" = validate ]; then m=\$(missing "\$AH_ENGINE_PLUGIN_ROOT") || { echo "ah-engine: defaults unavailable: missing_key key=\$m"; exit 70; }; echo ok
          else echo '{"last_error":null}'; fi ;;
  stop) [ -e "\$D/daemon.run" ] && mv "\$D/daemon.run" "\$D/daemon.run.stopped"; exit 0 ;;
  hook) : >"\$D/daemon.run"
        if missing "\$CLAUDE_PLUGIN_ROOT" >/dev/null; then exit 2; fi
        echo fallback >>"\$D/defaults.events"; exit 0 ;;
  status) if [ -e "\$D/daemon.run" ]; then n=0; [ -f "\$D/defaults.events" ] && n=\$(wc -l <"\$D/defaults.events" | tr -d ' '); echo "{\"defaults\":{\"by_reason\":{},\"last\":null,\"total\":\${n:-0}},\"running\":true}"
          else echo '{"running":false}'; fi ;;
esac
EOF
  chmod +x "$1"
}
mk_root() { # dir: a plugin root with defaults, pristine, hook wrapper and integrity stubs
  mkdir -p "$1/engine/defaults" "$1/engine/defaults.pristine" "$1/hooks"
  for d in defaults defaults.pristine; do
    printf '[index]\nvalue = 1\n' >"$1/engine/$d/index.toml"
    printf '[a.old]\nvalue = 1\n' >"$1/engine/$d/a.toml"
  done
  printf '#!/bin/sh\nexec "$HOME/.anti-hall/ah-engine/bin/ah-engine" hook "$@"\n' >"$1/hooks/ah-hook.sh"
  cat >"$1/hooks/ah-engine-integrity.sh" <<'EOF'
#!/bin/sh
sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }
[ "$1" = accept ] && sha "$AH_ENGINE_DIR/bin/ah-engine" >"$AH_ENGINE_DIR/bin/ah-engine.expected"
exit 0
EOF
}
mk_new_defaults() { # dir, with-wire(1|0)
  mkdir -p "$1"
  printf '[index]\nvalue = 1\n' >"$1/index.toml"
  printf '[a.old]\nvalue = 2\n' >"$1/a.toml"
  [ "$2" = 1 ] && printf '[wire.ack]\nvalue = 1\n' >"$1/wire.toml"
  return 0
}
setup() { # fresh fake machine under $T/$1
  W=$T/$1; H=$W/home
  E=$H/.anti-hall/ah-engine
  MKT=$H/.anti-hall/ah-engine-live/state/marketplace/plugins/anti-hall
  CACHE=$H/.claude/plugins/cache/live/anti-hall/1
  mkdir -p "$E/bin" "$H/.claude/plugins"
  mk_engine "$E/bin/ah-engine" "a.old"
  mk_root "$MKT"; mk_root "$CACHE"
  printf '{\n "plugins": {\n  "anti-hall@other": [{"installPath": "%s/wrong"}],\n  "anti-hall@anti-hall-engine-live": [\n   {\n    "scope": "user",\n    "installPath": "%s",\n    "version": "1"\n   }\n  ]\n }\n}\n' "$H" "$CACHE" >"$H/.claude/plugins/installed_plugins.json"
  mkdir -p "$W/new"
  mk_engine "$W/new/ah-engine" "a.old wire.ack"
  mk_new_defaults "$W/new/defaults" 1
  OLDSHA=$(sha "$E/bin/ah-engine"); NEWSHA=$(sha "$W/new/ah-engine")
}
run() { # args...: the script against the scratch home only
  HOME=$H AH_DEPLOY_HOME=$H AH_DEPLOY_PROBE_N=2 sh "$SCRIPT" "$@"
}
tree_sum() { find "$H" -type f -exec cksum {} + 2>/dev/null | sort | cksum; }

# --- 1. good path -----------------------------------------------------------------------------------------------------
setup good
run --check "$W/new/ah-engine" "$W/new/defaults" >"$W/check.out" 2>&1; rc=$?
ok "good: --check exits 0" test $rc -eq 0
ok "good: --check says OK" has "CHECK OK" "$W/check.out"
ok "good: --check shows the new binary refuses a root as-is (swap-first would break)" has "REFUSES" "$W/check.out"
ok "good: --check lists the added key" has "wire.ack" "$W/check.out"
ok "good: --check lists the changed value" has "value changes: \[a.old\]" "$W/check.out"
ok "good: --check found both roots" test "$(grep -c '^-- root' "$W/check.out")" -eq 2
ok "good: --check reads the installPath of the right key" has "$CACHE" "$W/check.out"
before=$(tree_sum)
ok "good: --check changed nothing" test "$before" = "$(tree_sum)"
run --deploy "$W/new/ah-engine" "$W/new/defaults" >"$W/dry.out" 2>&1; rc=$?
ok "good: --deploy without --yes exits 0" test $rc -eq 0
ok "good: --deploy without --yes is a dry run" has "dry run" "$W/dry.out"
ok "good: dry run changed nothing" test "$before" = "$(tree_sum)"
run --deploy "$W/new/ah-engine" "$W/new/defaults" --yes >"$W/deploy.out" 2>&1; rc=$?
ok "good: --deploy --yes exits 0" test $rc -eq 0
ok "good: --deploy reports OK" has "DEPLOY OK" "$W/deploy.out"
ok "good: live binary is the new one" test "$(sha "$E/bin/ah-engine")" = "$NEWSHA"
ok "good: old binary kept as .prev" test "$(sha "$E/bin/ah-engine.prev")" = "$OLDSHA"
ok "good: integrity record is the new sha" test "$(tr -d ' \n' <"$E/bin/ah-engine.expected")" = "$NEWSHA"
for r in "$MKT" "$CACHE"; do
  ok "good: defaults of $(basename "$r" | cut -c1-9)... have the new key" has "wire.ack" "$r/engine/defaults/wire.toml"
  ok "good: pristine has the new key too" has "wire.ack" "$r/engine/defaults.pristine/wire.toml"
  ok "good: the new a.toml is in" has "value = 2" "$r/engine/defaults/a.toml"
done
BK=$E/deploys/$(cat "$E/deploys/latest")
ok "good: replaced file kept in the backup" has "value = 1" "$BK/root1/defaults/a.toml"
ok "good: keys were synced before the binary was swapped" test "$(grep -n 'sync defaults into root 2' "$BK/deploy.log" | cut -d: -f1)" -lt "$(grep -n 'swap the binary' "$BK/deploy.log" | cut -d: -f1)"
ok "good: the engine was stopped before the sync" test "$(grep -n 'stop the engine' "$BK/deploy.log" | cut -d: -f1)" -lt "$(grep -n 'sync defaults into root 1' "$BK/deploy.log" | cut -d: -f1)"
ok "good: probe verified" has "force-push probe blocked 2/2" "$BK/deploy.log"
ok "good: defaults.total 0 verified" has "defaults.total 0" "$BK/deploy.log"
ok "good: no defaults.error written" test ! -e "$E/defaults.error"

# --- 4. rollback (on the good deploy) -------------------------------------------------------------------------------
after=$(tree_sum)
run --rollback >"$W/rbdry.out" 2>&1; rc=$?
ok "rollback: without --yes exits 0 and changes nothing" test $rc -eq 0 -a "$after" = "$(tree_sum)"
run --rollback --yes >"$W/rb.out" 2>&1; rc=$?
ok "rollback: --yes exits 0" test $rc -eq 0
ok "rollback: reports OK" has "ROLLBACK OK" "$W/rb.out"
ok "rollback: binary is the old one again" test "$(sha "$E/bin/ah-engine")" = "$OLDSHA"
ok "rollback: integrity record is the old sha" test "$(tr -d ' \n' <"$E/bin/ah-engine.expected")" = "$OLDSHA"
for r in "$MKT" "$CACHE"; do
  ok "rollback: a.toml is back to the old value" has "value = 1" "$r/engine/defaults/a.toml"
  ok "rollback: the file the deploy added is gone from the root" test ! -e "$r/engine/defaults/wire.toml"
done
ok "rollback: the added file was moved aside, not deleted" test -n "$(find "$BK"/rollback-* -name wire.toml 2>/dev/null)"

# --- 2. missing key ---------------------------------------------------------------------------------------------------
setup missing
mv "$W/new/defaults/wire.toml" "$W/new/wire.toml.moved"       # the new binary needs wire.ack, the new defaults do not have it
run --check "$W/new/ah-engine" "$W/new/defaults" >"$W/check.out" 2>&1; rc=$?
ok "missing key: --check exits non-zero" test $rc -ne 0
ok "missing key: --check names the key" has "wire.ack" "$W/check.out"
ok "missing key: --check says FAILED" has "CHECK FAILED" "$W/check.out"
before=$(tree_sum)
run --deploy "$W/new/ah-engine" "$W/new/defaults" --yes >"$W/deploy.out" 2>&1; rc=$?
ok "missing key: --deploy --yes refuses" test $rc -ne 0
ok "missing key: nothing changed" test "$before" = "$(tree_sum)"
ok "missing key: no backup dir was created" test ! -e "$E/deploys"

# --- 3. extra key (live-only, e.g. launcher keys) -------------------------------------------------------------------
setup extra
for r in "$MKT" "$CACHE"; do for d in defaults defaults.pristine; do printf '[launcher.only]\nvalue = 5\n' >>"$r/engine/$d/a.toml"; done; done
run --check "$W/new/ah-engine" "$W/new/defaults" >"$W/check.out" 2>&1; rc=$?
ok "extra key: --check exits non-zero" test $rc -ne 0
ok "extra key: --check names the key the sync would drop" has "DROPPED by the sync: \[launcher.only\]" "$W/check.out"
run --check "$W/new/ah-engine" "$W/new/defaults" --allow-drop >"$W/check2.out" 2>&1; rc=$?
ok "extra key: --allow-drop accepts it" test $rc -eq 0
before=$(tree_sum)
run --deploy "$W/new/ah-engine" "$W/new/defaults" --yes >"$W/deploy.out" 2>&1; rc=$?
ok "extra key: --deploy --yes without --allow-drop refuses and changes nothing" test $rc -ne 0 -a "$before" = "$(tree_sum)"

# --- 5. a guard failing open in the cache root is caught -------------------------------------------------------------
setup failopen
printf '#!/bin/sh\nexit 0\n' >"$CACHE/hooks/ah-hook.sh"       # the root Claude runs lets the force-push through
run --deploy "$W/new/ah-engine" "$W/new/defaults" --yes >"$W/deploy.out" 2>&1; rc=$?
ok "fail-open: the deploy reports failure" test $rc -ne 0
ok "fail-open: the probe failure is named" has "force-push probe blocked 0/2" "$W/deploy.out"
ok "fail-open: the exact rollback command is printed" has "engine-deploy.sh --rollback --from" "$W/deploy.out"

printf '\n%s passed, %s failed (scratch: %s)\n' "$PASS" "$FAIL" "$T"
[ "$FAIL" -eq 0 ]
