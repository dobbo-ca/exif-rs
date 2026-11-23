#!/bin/sh
# Writes the fuzz seeds, byte by byte with printf (there are no binary fixtures in git):
# tests/fuzz/make-seeds.sh <out-dir> makes <out-dir>/<target>/ for every fuzz target. The Dockerfile
# fuzz stage runs it with /seeds; fuzz.yml hands those directories to libFuzzer.
#   exif   a TIFF block (little and big endian, truncated)
#   xmp    [selector byte][packet], see fuzz_targets/xmp.rs
#   jpeg, png, webp   small files with EXIF, XMP and more (unknown chunks, bytes after IEND or
#                     the RIFF size, a short RIFF size); the image data is a stub
#   read   all the container seeds
#   strip  [selector byte][file] for every container seed, five selectors each
set -eu
out=${1:?usage: make-seeds.sh <out-dir>}
t=$(mktemp -d)
trap 'rm -rf "$t"' EXIT
mkdir -p "$out/exif" "$out/xmp" "$out/jpeg" "$out/png" "$out/webp" "$out/read" "$out/strip"

# bytes 255 216 writes the bytes with these decimal values.
bytes() { fmt=$(printf '\\%03o' "$@"); printf "$fmt"; }
be16() { bytes $(($1 / 256 % 256)) $(($1 % 256)); }
be32() { bytes $(($1 / 16777216 % 256)) $(($1 / 65536 % 256)) $(($1 / 256 % 256)) $(($1 % 256)); }
le32() { bytes $(($1 % 256)) $(($1 / 256 % 256)) $(($1 / 65536 % 256)) $(($1 / 16777216 % 256)); }
size() { wc -c < "$1" | tr -d ' '; }

# CRC-32 (PNG) of stdin, as a decimal number: bitwise, from the bytes that od prints.
crc32() {
  od -An -v -tu1 | {
    c=$((0xFFFFFFFF))
    while read -r line; do
      for b in $line; do
        c=$((c ^ b))
        i=0
        while [ "$i" -lt 8 ]; do
          c=$(((c >> 1) ^ (0xEDB88320 & -(c & 1))))
          i=$((i + 1))
        done
      done
    done
    echo $((c ^ 0xFFFFFFFF))
  }
}

# ---- TIFF: IFD0 with Orientation 6 and an Exif IFD pointer; the Exif IFD holds ISO 100.
{
  printf 'II*\000'; le32 8
  bytes 2 0
  bytes 18 1 3 0; le32 1; bytes 6 0 0 0
  bytes 105 135 4 0; le32 1; le32 38
  le32 0
  bytes 1 0
  bytes 39 136 3 0; le32 1; bytes 100 0 0 0
  le32 0
} > "$t/le.tiff"
# Big endian, IFD0 with Orientation 6 only.
bytes 77 77 0 42 0 0 0 8 0 1 1 18 0 3 0 0 0 1 0 6 0 0 0 0 0 0 > "$t/be.tiff"
head -c 30 "$t/le.tiff" > "$t/cut.tiff"
cp "$t/le.tiff" "$out/exif/le"
cp "$t/be.tiff" "$out/exif/be"
cp "$t/cut.tiff" "$out/exif/cut"

# ---- XMP packets: attributes on rdf:Description, then elements, an rdf:Alt and hdrgm.
printf '%s' '<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?><x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:tiff="http://ns.adobe.com/tiff/1.0/" xmlns:xmp="http://ns.adobe.com/xap/1.0/" tiff:Orientation="6" xmp:CreateDate="2020-01-02T03:04:05" xmp:Rating="3"/></rdf:RDF></x:xmpmeta><?xpacket end="w"?>' > "$t/a.xmp"
printf '%s' '<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:tiff="http://ns.adobe.com/tiff/1.0/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/" hdrgm:Version="1.0"><tiff:Orientation>3</tiff:Orientation><dc:title><rdf:Alt><rdf:li xml:lang="x-default">A &amp; B</rdf:li></rdf:Alt></dc:title></rdf:Description></rdf:RDF></x:xmpmeta>' > "$t/b.xmp"
for n in a b; do
  { bytes 0; cat "$t/$n.xmp"; } > "$out/xmp/$n-plain"
  { bytes 129; cat "$t/$n.xmp"; } > "$out/xmp/$n-ext"
done

# ---- JPEG: SOI, APP0 JFIF, APP1 Exif, APP1 XMP, APP2 ICC, SOF0, SOS, two entropy bytes, EOI.
seg() { bytes 255 "$1"; be16 $(($(size "$2") + 2)); cat "$2"; }
{ printf 'JFIF\000'; bytes 1 1 0 0 1 0 1 0 0; } > "$t/jfif"
{ printf 'Exif\000\000'; cat "$t/le.tiff"; } > "$t/exif.le"
{ printf 'Exif\000\000'; cat "$t/be.tiff"; } > "$t/exif.be"
{ printf 'http://ns.adobe.com/xap/1.0/\000'; cat "$t/a.xmp"; } > "$t/xmp.pay"
{ printf 'ICC_PROFILE\000'; bytes 1 1; printf 'dummyicc'; } > "$t/icc.pay"
bytes 8 0 1 0 1 3 1 34 0 2 17 1 3 17 1 > "$t/sof"
bytes 3 1 0 2 17 3 17 0 63 0 > "$t/sos"
{
  bytes 255 216
  seg 224 "$t/jfif"; seg 225 "$t/exif.le"; seg 225 "$t/xmp.pay"; seg 226 "$t/icc.pay"
  seg 192 "$t/sof"; seg 218 "$t/sos"
  bytes 0 0 255 217
} > "$out/jpeg/full.jpg"
{ bytes 255 216; seg 225 "$t/exif.be"; bytes 255 217; } > "$out/jpeg/exif-be.jpg"
# EXIF APP1s that extract does not take but strip drops, and keeps the orientation of:
# `EXIF\0\0` and `xxExif\0\0`.
{ printf 'EXIF\000\000'; cat "$t/be.tiff"; } > "$t/exif.upper"
{ printf 'xxExif\000\000'; cat "$t/le.tiff"; } > "$t/exif.xx"
{ bytes 255 216; seg 225 "$t/exif.upper"; seg 192 "$t/sof"; seg 218 "$t/sos"; bytes 0 0 255 217; } > "$out/jpeg/exif-upper.jpg"
{ bytes 255 216; seg 224 "$t/jfif"; seg 225 "$t/exif.xx"; seg 192 "$t/sof"; seg 218 "$t/sos"; bytes 0 0 255 217; } > "$out/jpeg/exif-prefixed.jpg"
head -c 60 "$out/jpeg/full.jpg" > "$out/jpeg/cut.jpg"
# A thumbnail that ends past the APP1 block, which read() takes from the file (as ExifTool does):
# IFD0 with no entries, IFD1 at 14 with offset 44 and length 200, then a 256-byte COM segment.
{
  printf 'II*\000'; le32 8; bytes 0 0; le32 14
  bytes 2 0
  bytes 1 2 4 0; le32 1; le32 44
  bytes 2 2 4 0; le32 1; le32 200
  le32 0; bytes 255 216
} > "$t/thumb.tiff"
{ printf 'Exif\000\000'; cat "$t/thumb.tiff"; } > "$t/exif.thumb"
head -c 256 /dev/zero > "$t/com"
{ bytes 255 216; seg 225 "$t/exif.thumb"; seg 254 "$t/com"; bytes 255 217; } > "$out/jpeg/thumb-past-app1.jpg"

# Ultra HDR and Motion Photo shape: a primary with a container XMP packet (hdrgm:Version, Camera
# fields, a Container:Directory of the primary, the gain map and the video) and an MPF segment
# (big endian, the primary and the gain map), then the gain map JPEG and a 16-byte fake video.
{ bytes 255 216; seg 218 "$t/sos"; bytes 0 0 255 217; } > "$t/gm.jpg"
{ be32 16; printf 'ftypmp42'; be32 0; } > "$t/video"
gml=$(size "$t/gm.jpg")
printf '%s' '<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/" xmlns:GCamera="http://ns.google.com/photos/1.0/camera/" xmlns:GContainer="http://ns.google.com/photos/1.0/container/" xmlns:Item="http://ns.google.com/photos/1.0/container/item/" xmlns:tiff="http://ns.adobe.com/tiff/1.0/" hdrgm:Version="1.0" GCamera:MotionPhoto="1" GCamera:MotionPhotoVersion="1" tiff:Orientation="6"><GContainer:Directory><rdf:Seq><rdf:li rdf:parseType="Resource"><GContainer:Item Item:Semantic="Primary" Item:Mime="image/jpeg" Item:Padding="0"/></rdf:li><rdf:li rdf:parseType="Resource"><GContainer:Item Item:Semantic="GainMap" Item:Mime="image/jpeg" Item:Length="'"$gml"'"/></rdf:li><rdf:li rdf:parseType="Resource"><GContainer:Item Item:Semantic="MotionPhoto" Item:Mime="video/mp4" Item:Length="16"/></rdf:li></rdf:Seq></GContainer:Directory></rdf:Description></rdf:RDF></x:xmpmeta>' > "$t/c.xmp"
{ printf 'http://ns.adobe.com/xap/1.0/\000'; cat "$t/c.xmp"; } > "$t/cxmp.pay"
xs=$(($(size "$t/cxmp.pay") + 4))
p=$((2 + xs + 66 + $(size "$t/sof") + 4 + $(size "$t/sos") + 4 + 4))
{
  printf 'MPF\000MM'; bytes 0 42; be32 8
  bytes 0 1 176 2 0 7; be32 32; be32 26; be32 0
  be32 196608; be32 "$p"; be32 0; be32 0
  be32 0; be32 "$gml"; be32 $((p - 2 - xs - 8)); be32 0
} > "$t/mpf.pay"
{
  bytes 255 216
  seg 225 "$t/cxmp.pay"; seg 226 "$t/mpf.pay"; seg 192 "$t/sof"; seg 218 "$t/sos"
  bytes 0 0 255 217
  cat "$t/gm.jpg" "$t/video"
} > "$out/jpeg/uhdr-motion.jpg"

# ---- PNG: signature, IHDR (1x1 grey), eXIf, iTXt XMP, IDAT (stored zlib block), IEND.
chunk() { { printf '%s' "$1"; cat "$2"; } > "$t/c"; be32 "$(size "$2")"; cat "$t/c"; be32 "$(crc32 < "$t/c")"; }
{ be32 1; be32 1; bytes 8 0 0 0 0; } > "$t/ihdr"
{ printf 'XML:com.adobe.xmp\000\000\000\000\000'; cat "$t/a.xmp"; } > "$t/itxt"
bytes 120 1 1 2 0 253 255 0 0 0 2 0 1 > "$t/idat"
: > "$t/empty"
{
  bytes 137 80 78 71 13 10 26 10
  chunk IHDR "$t/ihdr"; chunk eXIf "$t/le.tiff"; chunk iTXt "$t/itxt"; chunk IDAT "$t/idat"; chunk IEND "$t/empty"
} > "$out/png/full.png"
# An ImageMagick raw profile in a tEXt chunk instead of eXIf: "\nexif\n<spaces><length>\n<hex>".
hex=$({ printf 'Exif\000\000'; cat "$t/le.tiff"; } | od -An -v -tx1 | tr -d ' \n')
{ printf 'Raw profile type exif\000'; printf '\nexif\n%8d\n' $(($(size "$t/le.tiff") + 6)); printf '%s\n' "$hex"; } > "$t/rawp"
{
  bytes 137 80 78 71 13 10 26 10
  chunk IHDR "$t/ihdr"; chunk tEXt "$t/rawp"; chunk IDAT "$t/idat"; chunk IEND "$t/empty"
} > "$out/png/rawprofile.png"
head -c 70 "$out/png/full.png" > "$out/png/cut.png"
# A private chunk, eXIf, Apple's iDOT before IDAT, and bytes after IEND.
bytes 0 0 0 1 0 0 0 0 0 0 0 1 0 0 0 40 > "$t/idot"
printf 'private' > "$t/prvt"
{
  bytes 137 80 78 71 13 10 26 10
  chunk IHDR "$t/ihdr"; chunk prVt "$t/prvt"; chunk eXIf "$t/le.tiff"; chunk iDOT "$t/idot"; chunk IDAT "$t/idat"; chunk IEND "$t/empty"
  printf 'TRAILER'
} > "$out/png/extra.png"

# ---- WebP: RIFF, VP8X (EXIF and XMP flags), VP8L stub, EXIF, XMP.
rchunk() {
  printf '%s' "$1"; le32 "$(size "$2")"; cat "$2"
  [ $(($(size "$2") % 2)) = 0 ] || printf '\000'
}
riff() { { printf 'WEBP'; cat; } > "$t/body"; printf 'RIFF'; le32 "$(size "$t/body")"; cat "$t/body"; }
bytes 12 0 0 0 0 0 0 0 0 0 > "$t/vp8x"
bytes 8 0 0 0 0 0 0 0 0 0 > "$t/vp8x-exif"
bytes 47 0 0 0 0 > "$t/vp8l"
{ rchunk VP8X "$t/vp8x"; rchunk VP8L "$t/vp8l"; rchunk EXIF "$t/le.tiff"; rchunk 'XMP ' "$t/a.xmp"; } | riff > "$out/webp/full.webp"
{ rchunk VP8X "$t/vp8x-exif"; rchunk VP8L "$t/vp8l"; rchunk EXIF "$t/be.tiff"; } | riff > "$out/webp/exif-be.webp"
head -c 40 "$out/webp/full.webp" > "$out/webp/cut.webp"
# An unknown chunk last, odd-sized and without its pad byte (an odd RIFF size).
{ rchunk VP8X "$t/vp8x"; rchunk VP8L "$t/vp8l"; rchunk EXIF "$t/le.tiff"; printf 'prVt'; le32 3; printf 'odd'; } | riff > "$out/webp/unknown.webp"
# full.webp with a RIFF size that ends after VP8L (EXIF, XMP and a trailer past it), and with 0.
{ printf 'RIFF'; le32 36; tail -c +9 "$out/webp/full.webp"; printf 'TRAILER'; } > "$out/webp/short-riff.webp"
{ printf 'RIFF'; le32 0; tail -c +9 "$out/webp/full.webp"; } > "$out/webp/zero-riff.webp"
# A simple file (VP8L only), then past its RIFF size an EXIF chunk with an `Exif\0\0` prefix.
{ rchunk VP8L "$t/vp8l" | riff; rchunk EXIF "$t/exif.be"; } > "$out/webp/exif-past-riff.webp"

# ---- read: every container seed. strip: [selector][file]: 0 strips all, 3 keeps orientation and
# ICC, 7 resets the orientation, 43 keeps all but the MPF images, 59 keeps all (Keep::default()).
for d in jpeg png webp; do
  for f in "$out/$d"/*; do
    n=$d-$(basename "$f")
    cp "$f" "$out/read/$n"
    for s in 0 3 7 43 59; do { bytes "$s"; cat "$f"; } > "$out/strip/$s-$n"; done
  done
done
echo "seeds: $(find "$out" -type f | wc -l) files in $out"
