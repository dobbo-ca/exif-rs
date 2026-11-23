#!/bin/sh
# The ExifTool parity run (docs/parity.md): tests/parity/run.sh <corpus> <oracle-binary> <report-dir>
#
# <corpus> is what tests/parity/fetch.sh made; <oracle-binary> is the built examples/oracle.rs.
# Works in /work (deleted at the end). Writes <report-dir>/summary.md, the mismatch and failure
# lists, and <report-dir>/status (0 pass, 1 fail). Exits 0, or 2 when a tool is missing.
# PARITY_EVERY_N=<n> keeps every n-th input only.
set -u
corpus=${1:?usage: run.sh <corpus> <oracle-binary> <report-dir>}
oracle=${2:?usage: run.sh <corpus> <oracle-binary> <report-dir>}
report=${3:?usage: run.sh <corpus> <oracle-binary> <report-dir>}
here=$(cd "$(dirname "$0")" && pwd)
exiftool="$corpus/exiftool/exiftool"

missing() { echo "run.sh: missing $1" >&2; exit 2; }
command -v perl >/dev/null 2>&1 || missing perl
command -v python3 >/dev/null 2>&1 || missing python3
python3 -c 'import PIL.Image' 2>/dev/null || missing "python3 Pillow (PIL)"
command -v ultrahdr_app >/dev/null 2>&1 || missing ultrahdr_app
[ -f "$exiftool" ] || missing "$exiftool"
[ -x "$oracle" ] || missing "$oracle"

# -j JSON, -n no print conversion, -G1 family-1 group names, -a duplicates, -b binary values
# (base64), -ext '*' every file whatever its extension (a directory scan skips unknown ones).
et() { perl "$exiftool" -j -n -G1 -a -b -ext '*' "$@"; }

work=/work
rm -rf "$work"
mkdir -p "$work/strip" "$report"
status=0

every=${PARITY_EVERY_N:-1}
n=$(sh "$here/inputs.sh" "$corpus" "$work/in" "$every") || { echo "run.sh: inputs.sh failed" >&2; status=1; }
case $n in ''|*[!0-9]*) n=0 ;; esac
echo "inputs: $n"
# An empty or partial input set must not pass: both sources contribute, and the pinned corpus
# gives PARITY_MIN_INPUTS inputs (2402 at the fetch.sh commits) divided by every-n.
min=$((${PARITY_MIN_INPUTS:-2402} / every))
if [ "$n" -lt "$min" ]; then echo "run.sh: $n inputs, want at least $min" >&2; status=1; fi
for src in exiftool mei; do
  if [ -z "$(find "$work/in" -name "${src}__*" | head -n 1)" ]; then
    echo "run.sh: no $src inputs" >&2
    status=1
  fi
done

# ExifTool exits non-zero when it cannot read a file; that file's JSON still has its Error.
et "$work/in" > "$work/exiftool.json" 2> "$work/exiftool.stderr"
# EXIF, XMP and ICC_Profile are protected blocks that -b prints only when asked for by name.
et -EXIF -XMP -ICC_Profile "$work/in" > "$work/exiftool-blocks.json" 2>> "$work/exiftool.stderr"

"$oracle" read "$work/in" "$work/oracle.jsonl" || { echo "run.sh: oracle read failed" >&2; status=1; }
python3 "$here/compare.py" "$work/in" "$work/exiftool.json" "$work/exiftool-blocks.json" \
  "$work/oracle.jsonl" "$here/known-differences.tsv" "$report" || status=1
# The full ExifTool and oracle records of each mismatched file, for diagnosis.
python3 -c '
import json, sys
names = {l.split("\t")[0] for l in open(sys.argv[1], encoding="utf-8").read().splitlines()[1:]}
et = [r for r in json.load(open(sys.argv[2], encoding="utf-8")) if r.get("SourceFile", "").rsplit("/", 1)[-1] in names]
orc = [json.loads(l) for l in open(sys.argv[3], encoding="utf-8") if json.loads(l).get("SourceFile", "").rsplit("/", 1)[-1] in names]
json.dump({"exiftool": et, "oracle": orc}, open(sys.argv[4], "w", encoding="utf-8"), indent=1)
' "$report/read-mismatches.tsv" "$work/exiftool.json" "$work/oracle.jsonl" "$report/mismatch-records.json" || true

for mode in all keep default orient1; do
  "$oracle" strip "$mode" "$work/in" "$work/strip/$mode" || { echo "run.sh: oracle strip $mode failed" >&2; status=1; }
  if [ -n "$(ls -A "$work/strip/$mode" 2>/dev/null)" ]; then
    et "$work/strip/$mode" > "$work/strip/$mode.json" 2>> "$work/exiftool.stderr"
  else
    echo '[]' > "$work/strip/$mode.json"
  fi
  cp "$work/strip/$mode.errors" "$report/strip-$mode.errors" 2>/dev/null || true
done
python3 "$here/strip_check.py" "$work/in" "$work/exiftool.json" "$work/strip" "$report" || status=1
# Ultra HDR and Motion Photo files made in CI (no licensed sample exists), stripped and checked.
python3 "$here/gainmap.py" "$exiftool" "$oracle" "$work/gainmap" "$report" || status=1
cp "$work/exiftool.stderr" "$report/exiftool.stderr"

commit() { git -C "$corpus/$1" rev-parse HEAD 2>/dev/null || echo unknown; }
{
  echo "# exif-rs parity run"
  echo
  if [ "$status" -eq 0 ]; then echo "Result: **pass**"; else echo "Result: **FAIL**"; fi
  echo
  echo "Oracle: ExifTool $(perl "$exiftool" -ver) (exiftool $(commit exiftool)); corpus metadata-extractor-images $(commit mei)."
  echo
  cat "$report/compare.md" 2>/dev/null
  echo
  cat "$report/strip.md" 2>/dev/null
  echo
  cat "$report/gainmap.md" 2>/dev/null
} > "$report/summary.md"
echo "$status" > "$report/status"
cat "$report/summary.md"

rm -rf "$work"
exit 0
