#!/bin/sh
# Wall time of ExifTool and of examples/bench.rs over the same files, as /report/bench.md:
# tests/parity/bench.sh [in-dir] [exiftool] [bench-binary] [report-dir]. Information only (no speed
# bar). ExifTool runs once (`perl <exiftool> -j -n -G1 -a -r <in-dir>`, output discarded, start-up
# of perl included); the Rust bench example reads and strips the files with one thread, the files
# already in memory, best of three rounds.
set -eu
in=${1:-/work/in}
et=${2:-/corpus/exiftool/exiftool}
bench=${3:-target/release/examples/bench}
out=${4:-/report}
command -v perl >/dev/null
mkdir -p "$out"

"$bench" "$in" > "$out/rust.md"
cat "$out/rust.md"

t0=$(date +%s%N)
perl "$et" -j -n -G1 -a -r "$in" > /dev/null 2> "$out/exiftool.err" || true
t1=$(date +%s%N)
et_ms=$(( (t1 - t0) / 1000000 ))

# the last row of rust.md: | files | MB | read ms | strip ms |
row=$(sed -n '$p' "$out/rust.md")
files=$(echo "$row" | awk -F'|' '{ gsub(/ /, "", $2); print $2 }')
mb=$(echo "$row" | awk -F'|' '{ gsub(/ /, "", $3); print $3 }')
read_ms=$(echo "$row" | awk -F'|' '{ gsub(/ /, "", $4); print $4 }')
strip_ms=$(echo "$row" | awk -F'|' '{ gsub(/ /, "", $5); print $5 }')
ratio=$(awk -v a="$et_ms" -v b="$read_ms" 'BEGIN { if (b > 0) printf "%.0f", a / b; else print "n/a" }')

{
  echo "## ExifTool 13.64 and exif-rs, same files (information only)"
  echo
  echo "$files files, $mb MB. exif-rs: one thread, files in memory, best of 3 rounds. ExifTool: one run, perl start-up included."
  echo
  echo "| | wall ms |"
  echo "|---|---|"
  echo "| ExifTool \`-j -n -G1 -a -r\` | $et_ms |"
  echo "| exif-rs \`read\` | $read_ms |"
  echo "| exif-rs \`strip\` (\`Keep::default()\`) | $strip_ms |"
  echo
  echo "ExifTool / exif-rs read: ${ratio}x. ExifTool stderr lines: $(wc -l < "$out/exiftool.err")."
} > "$out/bench.md"
cat "$out/bench.md"
