#!/bin/sh
# make-bundle.sh --bin <engine binary | CI artifact .tar.gz/.tgz/.zip> --plugin <plugin tree> [--out <dir>]
# Assembles the bundle go-live.sh deploys:  <out>/ah-engine  and  <out>/plugin/  (default <out> = <kit>/bundle).
# No cargo build here: the binary must come from CI (a path to the binary, or the downloaded CI artifact archive).
# Env: AH_BUNDLE_BIN, AH_BUNDLE_PLUGIN, AH_BUNDLE_OUT supply the same values; an argument wins. Nothing is deleted: an existing <out> is
# moved to <out>.old-<time>. The plugin copy omits .git and ah-engine.lock (go-live refuses a bundle that ships a lock).
KIT=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
bin=${AH_BUNDLE_BIN:-}; plugin=${AH_BUNDLE_PLUGIN:-}; out=${AH_BUNDLE_OUT:-$KIT/bundle}
die() { printf 'make-bundle: %s\n' "$*" >&2; exit 1; }
usage() { printf 'usage: make-bundle.sh --bin <binary|artifact archive> --plugin <plugin tree> [--out <dir>]\n' >&2; exit 2; }
while [ $# -gt 0 ]; do
  case "$1" in
    --bin) shift; bin=${1:-} ;; --plugin) shift; plugin=${1:-} ;; --out) shift; out=${1:-} ;;
    -h|--help) usage ;; *) printf 'make-bundle: unknown argument: %s\n' "$1" >&2; usage ;;
  esac; shift
done
[ -n "$bin" ] && [ -n "$plugin" ] && [ -n "$out" ] || usage
[ -e "$bin" ] || die "binary or artifact not found: $bin"
[ -d "$plugin" ] || die "plugin tree not found: $plugin"
[ -f "$plugin/hooks/hooks.json" ] || die "$plugin is not an anti-hall plugin root (no hooks/hooks.json)"
[ -f "$plugin/engine/defaults/index.toml" ] || die "$plugin has no engine/defaults/index.toml"
[ -f "$plugin/hooks/ah-engine-integrity.sh" ] || die "$plugin has no hooks/ah-engine-integrity.sh (needed by go-live for the crash-loop roll-back)"
[ -f "$plugin/.claude-plugin/plugin.json" ] || die "$plugin has no .claude-plugin/plugin.json"

T=$(mktemp -d) || die "cannot create a temp dir"
trap 'rm -rf "$T"' 0
case "$bin" in
  *.tar.gz|*.tgz) mkdir "$T/x" && tar -xzf "$bin" -C "$T/x" || die "cannot extract $bin" ;;
  *.zip) mkdir "$T/x" && unzip -q "$bin" -d "$T/x" || die "cannot extract $bin" ;;
  *) mkdir "$T/x" && cp "$bin" "$T/x/ah-engine" || die "cannot copy $bin" ;;
esac
found=$(find "$T/x" -type f -name ah-engine | head -n 1)
[ -n "$found" ] || die "no file named ah-engine in $bin"
chmod 755 "$found"
# the engine reads its settings from the plugin tree, so the version command only runs against a matching plugin (a mismatch is a real bundle defect)
ver=$(HOME=$T/home AH_ENGINE_DIR=$T/state AH_ENGINE_PLUGIN_ROOT=$plugin "$found" version 2>&1) || die "the binary does not run the version command against $plugin: $ver"
[ -n "$ver" ] || die "the binary printed no version"

if [ -e "$out" ]; then
  old=$out.old-$(date +%Y%m%d-%H%M%S)
  mv "$out" "$old" || die "cannot move the existing $out aside"
  printf 'make-bundle: previous bundle kept at %s\n' "$old"
fi
mkdir -p "$out/plugin" || die "cannot create $out"
cp "$found" "$out/ah-engine" && chmod 755 "$out/ah-engine" || die "cannot place the binary"
# tar pipe keeps modes and skips what must not ship
( cd "$plugin" && tar -cf - --exclude=.git --exclude=ah-engine.lock . ) | ( cd "$out/plugin" && tar -xf - ) || die "cannot copy the plugin tree"
[ -f "$out/plugin/hooks/hooks.json" ] && [ ! -e "$out/plugin/ah-engine.lock" ] || die "bundle is incomplete after copy"
sum=$(shasum -a 256 "$out/ah-engine" 2>/dev/null || sha256sum "$out/ah-engine" 2>/dev/null)
printf 'make-bundle: %s ready\n  engine: %s (sha256 %s)\n  plugin: %s\n' "$out" "$ver" "$(printf %s "$sum" | cut -c1-12)" "$plugin"
