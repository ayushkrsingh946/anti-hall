#!/bin/sh
# Fails when plugins/anti-hall passes a hard file-count or per-file-size limit; warns past a soft one.
# Limits: tests/plugin-limits.toml. Run: sh ah-engine/tests/plugin-dir-limits.sh
set -u
here=$(cd "$(dirname "$0")" && pwd)
plugin="$here/../../plugins/anti-hall"
toml="$here/plugin-limits.toml"
val() { sed -n "s/^$1[[:space:]]*=[[:space:]]*\([0-9][0-9]*\).*/\1/p" "$toml" | head -n 1; }
fs=$(val files_soft); fh=$(val files_hard); ss=$(val file_kib_soft); sh_=$(val file_kib_hard)
for v in "$fs" "$fh" "$ss" "$sh_"; do [ -n "$v" ] || { echo "FAIL: missing limit in $toml"; exit 2; }; done
rc=0
n=$(find "$plugin" -type f | wc -l | tr -d ' ')
if [ "$n" -gt "$fh" ]; then echo "FAIL: $n files under plugins/anti-hall, hard limit $fh"; rc=1
elif [ "$n" -gt "$fs" ]; then echo "WARN: $n files under plugins/anti-hall, soft limit $fs"
else echo "ok: $n files (soft $fs, hard $fh)"; fi
find "$plugin" -type f ! -iname '*.png' ! -iname '*.jpg' ! -iname '*.jpeg' ! -iname '*.gif' ! -iname '*.webp' \
  ! -iname '*.woff' ! -iname '*.woff2' ! -iname '*.ttf' ! -iname '*.otf' | while IFS= read -r f; do
  b=$(wc -c <"$f" | tr -d ' ')
  if [ "$b" -gt $((sh_ * 1024)) ]; then echo "FAIL: $f is $b bytes, hard limit $sh_ KiB"
  elif [ "$b" -gt $((ss * 1024)) ]; then echo "WARN: $f is $b bytes, soft limit $ss KiB"; fi
done >"${TMPDIR:-/tmp}/ah-dirlimits.$$"
cat "${TMPDIR:-/tmp}/ah-dirlimits.$$"
grep -q '^FAIL' "${TMPDIR:-/tmp}/ah-dirlimits.$$" && rc=1
rm -f "${TMPDIR:-/tmp}/ah-dirlimits.$$"
exit $rc
