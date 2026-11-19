#!/bin/sh
# Makes the flat parity input dir: tests/parity/inputs.sh <corpus> <out-dir> [every-n]
#
# <corpus> is what tests/parity/fetch.sh made. <out-dir> gets one symlink per input (ExifTool and
# the oracle both follow symlinks):
#   exiftool/t/images: *.jpg *.png *.webp (any case) and every other file that starts with JPEG,
#                      PNG or RIFF-WEBP magic bytes;
#   mei:               every file under jpg/ png/ webp/ (not metadata/ dirs, not *.txt) and
#                      fuzzing/*.fuzzed (hostile files of any format).
# Link name: <source>__<path in the source, with / and spaces as _>; a repeated name gets __2,
# __3, ... The list is sorted; with every-n only every n-th input is kept (the 1st, n+1-th, ...).
# Prints the number of inputs.
set -eu
corpus=$(cd "${1:?usage: inputs.sh <corpus> <out-dir> [every-n]}" && pwd)
out=${2:?usage: inputs.sh <corpus> <out-dir> [every-n]}
every=${3:-1}
mkdir -p "$out"

# True when $1 starts with JPEG, PNG or RIFF....WEBP magic.
is_image() {
  case $(od -An -tx1 -N12 "$1" 2>/dev/null | tr -d ' \n') in
    ffd8ff*|89504e470d0a1a0a*|52494646????????57454250) return 0 ;;
  esac
  return 1
}

# "<link name>\t<absolute path>" for <source> <root> <path in root>.
emit() {
  printf '%s__%s\t%s/%s\n' "$1" "$(printf '%s' "$3" | tr '/ ' '__')" "$2" "$3"
}

list() {
  et="$corpus/exiftool"
  (cd "$et" && find t/images -type f) | while IFS= read -r rel; do
    case $(printf '%s' "$rel" | tr 'A-Z' 'a-z') in
      *.jpg|*.png|*.webp) emit exiftool "$et" "$rel" ;;
      *) if is_image "$et/$rel"; then emit exiftool "$et" "$rel"; fi ;;
    esac
  done
  mei="$corpus/mei"
  for d in jpg png webp; do
    [ -d "$mei/$d" ] || continue
    (cd "$mei" && find "$d" -type d -name metadata -prune -o -type f ! -name '*.txt' -print)
  done | while IFS= read -r rel; do emit mei "$mei" "$rel"; done
  if [ -d "$mei/fuzzing" ]; then
    (cd "$mei" && find fuzzing -type f -name '*.fuzzed') | while IFS= read -r rel; do
      emit mei "$mei" "$rel"
    done
  fi
}

list | LC_ALL=C sort | awk -F '\t' -v n="$every" '
  { name = $1; if (seen[name]++) name = name "__" seen[$1] }
  (NR - 1) % n == 0 { print name "\t" $2 }
' | {
  count=0
  while IFS="$(printf '\t')" read -r name path; do
    ln -s "$path" "$out/$name"
    count=$((count + 1))
  done
  echo "$count"
}
