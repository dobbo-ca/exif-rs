#!/bin/sh
# Fetches the two pinned public sources of the parity corpus into <dir>: tests/parity/fetch.sh <dir>.
# Each is fetched once; <dir>/<name>.ok is its stamp (a run with the stamp does nothing, a run
# without it removes what a broken earlier run left and fetches again).
#
#   exiftool  ExifTool 13.64, https://github.com/exiftool/exiftool, commit 3d7a0538...: the oracle
#             (`perl <dir>/exiftool/exiftool`) and, in t/images, corpus files. Licence: same terms
#             as Perl (GPL or Artistic); used as data and oracle only, no code is copied.
#   mei       https://github.com/drewnoakes/metadata-extractor-images, commit 7b56b649...: the
#             jpg/, png/, webp/ and fuzzing/ folders (not the metadata/ folders or *.txt), about
#             1 GB, fetched blobless and sparse. Its README: "You are free to use these media
#             files however you wish."
set -eu
dir=${1:?usage: fetch.sh <dir>}
et_sha=3d7a0538324e9b04aa8930201b007009491321d2
mei_sha=7b56b649039c57903bcd5cbe9846ab81eb518ec4
mkdir -p "$dir"

if [ ! -e "$dir/exiftool.ok" ]; then
  rm -rf "$dir/exiftool"
  mkdir "$dir/exiftool"
  (
    cd "$dir/exiftool"
    git init -q
    git remote add origin https://github.com/exiftool/exiftool
    git fetch -q --depth 1 origin "$et_sha"
    git checkout -q FETCH_HEAD
    [ "$(git rev-parse HEAD)" = "$et_sha" ] || { echo "fetch.sh: exiftool is not $et_sha" >&2; exit 1; }
  )
  touch "$dir/exiftool.ok"
fi

if [ ! -e "$dir/mei.ok" ]; then
  rm -rf "$dir/mei"
  mkdir "$dir/mei"
  (
    cd "$dir/mei"
    git init -q
    git remote add origin https://github.com/drewnoakes/metadata-extractor-images
    # What `git clone --filter=blob:none` records, so the checkout fetches the blobs it needs.
    git config core.repositoryformatversion 1
    git config extensions.partialClone origin
    git config remote.origin.promisor true
    git config remote.origin.partialclonefilter blob:none
    git config core.sparseCheckout true
    # Non-cone patterns, the last match wins: the four folders, minus metadata/ folders and *.txt.
    mkdir -p .git/info
    printf '%s\n' /jpg/ /png/ /webp/ /fuzzing/ '!metadata/' '!*.txt' > .git/info/sparse-checkout
    git fetch -q --depth 1 --filter=blob:none origin "$mei_sha"
    git checkout -q FETCH_HEAD
    [ "$(git rev-parse HEAD)" = "$mei_sha" ] || { echo "fetch.sh: mei is not $mei_sha" >&2; exit 1; }
  )
  touch "$dir/mei.ok"
fi

echo "exiftool t/images: $(find "$dir/exiftool/t/images" -type f | wc -l) files"
for d in jpg png webp fuzzing; do
  echo "mei $d: $(find "$dir/mei/$d" -type f 2>/dev/null | wc -l) files"
done
