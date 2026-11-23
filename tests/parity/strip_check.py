#!/usr/bin/env python3
"""Checks the outputs of `oracle strip` with ExifTool and Pillow (docs/parity.md).

usage: strip_check.py <in-dir> <exiftool.json> <strip-dir> <report-dir>

<strip-dir>/<mode>/ holds the outputs of modes all, keep, default and orient1,
<strip-dir>/<mode>.json ExifTool's JSON of them and <strip-dir>/<mode>.errors the files exif-rs
refused. Writes <report-dir>/strip.md and <report-dir>/strip-failures.tsv. Exits 1 on any
failure: a check of an output, or a refusal of a JPEG/PNG/WebP input that strip-refusals.tsv
does not list.
"""

import hashlib
import os
import re
import sys
import warnings
from collections import Counter

from PIL import Image

from compare import Num, canon, container, load_json, by_name, counts

MODES = ("all", "keep", "default", "orient1")
REFUSALS = os.path.join(os.path.dirname(os.path.abspath(__file__)), "strip-refusals.tsv")

# Tags a fully stripped file may still have: file facts and image structure, no metadata.
OPEN_GROUPS = {"ExifTool", "System", "JFIF", "Adobe", "PNG-pHYs", "PNG-cICP"}
FILE_DENY = {"Comment", "ExifByteOrder"}
COMPOSITE_OK = {"ImageSize", "Megapixels", "Duration"}
# IHDR, PLTE, tRNS, cHRM, gAMA, sBIT, sRGB, bKGD, hIST, sPLT and acTL (the kept chunks).
PNG_OK = {
    "ImageWidth", "ImageHeight", "BitDepth", "ColorType", "Compression", "Filter", "Interlace",
    "Palette", "Transparency", "WhitePointX", "WhitePointY", "RedX", "RedY", "GreenX", "GreenY",
    "BlueX", "BlueY", "Gamma", "SignificantBits", "SRGBRendering", "BackgroundColor",
    "PaletteHistogram", "SuggestedPalette", "AnimationFrames", "AnimationPlays",
}
# VP8, VP8L, VP8X, ANIM, ANMF and ALPH.
RIFF_OK = {
    "VP8Version", "ImageWidth", "ImageHeight", "HorizontalScale", "VerticalScale", "AlphaIsUsed",
    "WebP_Flags", "BackgroundColor", "AnimationLoopCount", "Duration", "AlphaPreprocessing",
    "AlphaFiltering", "AlphaCompression",
}
ORIENTATION = ("IFD0:Orientation", "XMP-tiff:Orientation")


def allowed(key):
    group, _, tag = key.partition(":")
    if group in OPEN_GROUPS:
        return True
    if group == "File":
        return tag not in FILE_DENY
    if group == "Composite":
        return tag in COMPOSITE_OK
    if group == "PNG":
        return tag in PNG_OK
    if group == "RIFF":
        return tag in RIFF_OK
    return False


def is_icc(key):
    # PNG:ICC_Profile: ExifTool's tag for an iCCP chunk (or a raw profile text chunk) it cannot
    # decode as ICC; Keep.icc keeps those chunks as stored.
    return key.startswith("ICC") or key in ("PNG:ProfileName", "PNG:ICC_Profile")


def keys(d):
    return [k for k in d if k != "SourceFile"]


def check_file(orig, out):
    """An output of an original ExifTool reads without error must read without error and keep
    its MIME type (File:FileType may change: a WebP file that gets a VP8X chunk is Extended)."""
    if "ExifTool:Error" in orig:
        return []
    fails = []
    if "ExifTool:Error" in out:
        fails.append(f"ExifTool:Error {canon(out['ExifTool:Error'])}"[:300])
    want, got = canon(orig.get("File:MIMEType")), canon(out.get("File:MIMEType"))
    if want != got:
        fails.append(f"File:MIMEType: original {want} output {got}"[:300])
    return fails


def check_all(orig, out):
    return [f"{k} remains" for k in keys(out) if not allowed(k)] + check_file(orig, out)


def check_keep(orig, out):
    kept = {"IFD0:Orientation", "File:ExifByteOrder"}
    fails = [f"{k} remains" for k in keys(out) if not (allowed(k) or k in kept or is_icc(k))]
    for k in ["IFD0:Orientation"] + sorted(k for k in set(orig) | set(out) if is_icc(k)):
        if canon(orig.get(k)) != canon(out.get(k)):
            fails.append(f"{k}: original {canon(orig.get(k))} output {canon(out.get(k))}"[:300])
    if "File:ExifByteOrder" in out and "IFD0:Orientation" not in out:
        fails.append("File:ExifByteOrder without IFD0:Orientation")
    return fails + check_file(orig, out)


def check_orient1(orig, out):
    fails = []
    for k in sorted(set(keys(orig)) | set(keys(out))):
        if k.startswith("System:"):
            continue
        want = canon(Num("1") if k in orig else None) if k in ORIENTATION else canon(orig.get(k))
        if canon(out.get(k)) != want:
            fails.append(f"{k}: want {want} output {canon(out.get(k))}"[:300])
    return fails


# Mode default: the groups of the EXIF block and the Photoshop block, which strip always removes
# (only IFD0:Orientation may stay), and the container XMP keys it writes again.
STRIPPED_GROUPS = {"IFD0", "IFD1", "ExifIFD", "GPS", "InteropIFD", "MakerNotes", "Photoshop"}
GCAMERA = {
    "MotionPhoto", "MotionPhotoVersion", "MotionPhotoPresentationTimestampUs", "MicroVideo",
    "MicroVideoVersion", "MicroVideoOffset", "MicroVideoPresentationTimestampUs",
}
# Values a strip may change: the MPF size of the primary image, every file position ExifTool
# reports (MPImageStart, a kept maker segment's PreviewImageStart, ...: the bytes move with the
# dropped segments before them), and Composite values, which it derives in part from the
# dropped EXIF.
MOVES = re.compile(r"^MPImage\d+:MPImageLength$|^(?!XMP)[^:]+:\w*(Start|Offset)$|^Composite:")


def is_container_xmp(key):
    group, _, tag = key.partition(":")
    return (
        key == "XMP-hdrgm:Version"
        or group == "XMP-GContainer"
        or (group == "XMP-GCamera" and tag in GCAMERA)
    )


def check_default(orig, out):
    """As keep, and every other key must be the original's with its value (kept unknown APPn
    segments and PNG / WebP chunks, MPF, trailers), except file positions, the MPF primary size
    and Composite values; no EXIF, Photoshop or XMP key but IFD0:Orientation and the container
    XMP keys."""
    fails = []
    for k in keys(out):
        group = k.partition(":")[0]
        # XMP-rdf:About: the new packet has rdf:about="".
        kept = ("IFD0:Orientation", "File:ExifByteOrder", "XMP-rdf:About")
        if allowed(k) or is_icc(k) or k in kept:
            continue
        if group in STRIPPED_GROUPS or group.startswith("SubIFD") or (
            group.startswith("XMP") and not is_container_xmp(k)
        ):
            fails.append(f"{k} remains")
        elif k not in orig:
            fails.append(f"{k} added")
        elif not MOVES.match(k) and canon(orig[k]) != canon(out[k]):
            fails.append(f"{k}: original {canon(orig[k])} output {canon(out[k])}"[:300])
    for k in ["IFD0:Orientation"] + sorted(k for k in set(orig) | set(out) if is_icc(k)):
        if canon(orig.get(k)) != canon(out.get(k)):
            fails.append(f"{k}: original {canon(orig.get(k))} output {canon(out.get(k))}"[:300])
    if "File:ExifByteOrder" in out and "IFD0:Orientation" not in out:
        fails.append("File:ExifByteOrder without IFD0:Orientation")
    return fails + check_file(orig, out)


def primary_end(d):
    """The offset after the EOI of the primary image of JPEG bytes `d`, found as exif-rs finds
    it: the segments by their length up to the first SOS, then the scan up to FF D9 (segments
    between scans skipped by their length); len(d) when there is no EOI; None when a byte before
    the first scan is not a marker."""
    n, i = len(d), 2
    while True:
        start = i
        if i >= n or d[i] != 0xFF:
            return None
        while i < n and d[i] == 0xFF:
            i += 1
        if i >= n:
            return None
        m = d[i]
        i += 1
        if m == 0xD9:
            return i
        if m == 0xDA:
            i = start
            break
        if m == 0x01 or 0xD0 <= m <= 0xD8:
            continue
        if m == 0x00 or i + 2 > n:
            return None
        length = d[i] << 8 | d[i + 1]
        if length < 2 or i + length > n:
            return None
        i += length
    while True:
        j = d.find(b"\xff", i)
        if j < 0 or j + 1 >= n:
            return n
        m = d[j + 1]
        if m in (0x00, 0x01, 0xFF) or 0xD0 <= m <= 0xD8:
            i = j + 1
        elif m == 0xD9:
            return j + 2
        elif j + 4 > n:
            return n
        else:
            i = j + 2 + max(d[j + 2] << 8 | d[j + 3], 2)


# Chunks strip always drops: unknown chunks may remain in mode default, these may not (PNG text
# only as a `Raw profile type icc` / `icm` chunk, which Keep.icc keeps).
PNG_META = {b"eXIf", b"zxIf", b"zXIf", b"tIME", b"tEXt", b"zTXt", b"iTXt"}
PNG_TEXT = {b"tEXt", b"zTXt", b"iTXt"}
ICC_KEYWORDS = {b"Raw profile type icc", b"Raw profile type icm"}
RIFF_META = {b"EXIF", b"XMP ", b"XMP\0"}


def check_meta_chunks(out_path, kind, out):
    """Mode default: the PNG or WebP output has no metadata chunk left but one new EXIF chunk
    (`eXIf` / `EXIF`) when it has IFD0:Orientation (check_default checks it holds only that)."""
    with open(out_path, "rb") as f:
        d = f.read()
    left = []
    if kind == "PNG":
        i = 8
        while i + 8 <= len(d):
            n, t = int.from_bytes(d[i:i + 4], "big"), d[i + 4:i + 8]
            icc = t in PNG_TEXT and d[i + 8:i + 8 + n].split(b"\0", 1)[0] in ICC_KEYWORDS
            if t in PNG_META and not icc:
                left.append(t)
            if t == b"IEND":
                break
            i += 12 + n
    else:
        i = 12
        while i + 8 <= len(d):
            t, n = d[i:i + 4], int.from_bytes(d[i + 4:i + 8], "little")
            if t in RIFF_META:
                left.append(t)
            i += 8 + n + (n & 1)
    if "IFD0:Orientation" in out:
        for t in (b"eXIf", b"EXIF"):
            if t in left:
                left.remove(t)
    return [f"{t.decode('latin-1')!r} chunk remains" for t in left]


def check_trailing(in_path, out_path):
    """Mode default: the output ends with the bytes after the original's primary image."""
    with open(in_path, "rb") as f:
        d = f.read()
    end = primary_end(d)
    if end is None:
        return []
    with open(out_path, "rb") as f:
        if not f.read().endswith(d[end:]):
            return [f"the {len(d) - end} bytes after the EOI are not kept"]
    return []


CHECKS = {
    "all": check_all, "keep": check_keep, "default": check_default, "orient1": check_orient1,
}


def frames(path):
    """Mode, size and pixel hash of every frame. JPEG: the first frame only. Pillow opens a
    JPEG with an MPF segment as MPO, whose later frames are the images after EOI (modes all and
    keep drop them; mode default keeps them, checked by check_trailing and gainmap.py)."""
    with Image.open(path) as im:
        out = []
        n = 1 if im.format in ("JPEG", "MPO") else getattr(im, "n_frames", 1)
        for i in range(n):
            im.seek(i)
            im.load()
            out.append((im.mode, im.size, hashlib.sha256(im.tobytes()).hexdigest()))
        return out


def load_refusals(path):
    """file -> error class of the refusals strip makes on purpose (modes all and keep)."""
    out = {}
    if os.path.exists(path):
        with open(path, encoding="utf-8") as f:
            for line in f:
                parts = line.rstrip("\n").split("\t")
                if line.startswith("#") or len(parts) < 2:
                    continue
                out[parts[0]] = parts[1]
    return out


def main():
    in_dir, et_path, strip_dir, report = sys.argv[1:5]
    warnings.simplefilter("ignore")
    orig = by_name(load_json(et_path))
    pinned = load_refusals(REFUSALS)
    orig_pixels = {}
    failures = []
    lines = [
        "## Strip checks",
        "",
        "| mode | outputs | exif-rs errors | unexpected refusals | tag failures | pixel checked"
        " | identical | pixel failures | pixel skipped |",
        "|---|---:|---:|---:|---:|---:|---:|---:|---:|",
    ]
    notes = []
    for mode in MODES:
        out_dir = os.path.join(strip_dir, mode)
        out = by_name(load_json(os.path.join(strip_dir, mode + ".json")))
        refused = {}
        errors_path = os.path.join(strip_dir, mode + ".errors")
        if os.path.exists(errors_path):
            with open(errors_path, encoding="utf-8") as f:
                for line in f:
                    if line.strip():
                        name, _, cls = line.rstrip("\n").rpartition("\t")
                        refused[name] = cls
        errors = Counter(refused.values())
        c, skips = Counter(), Counter()
        # A file without JPEG/PNG/WebP magic is refused as unknown-format; a JPEG/PNG/WebP file
        # only when strip-refusals.tsv lists it with that class (never by reset_orientation).
        for name, cls in sorted(refused.items()):
            if container(os.path.join(in_dir, name)) == "other":
                expected = cls == "unknown-format"
            else:
                expected = mode != "orient1" and pinned.get(name) == cls
            if not expected:
                c["unexpected refusals"] += 1
                failures.append((mode, name, "refused", cls))
        names = sorted(os.listdir(out_dir)) if os.path.isdir(out_dir) else []
        for name in names:
            c["outputs"] += 1
            fails = CHECKS[mode](orig.get(name, {}), out.get(name, {}))
            kind = container(os.path.join(in_dir, name))
            if mode == "default" and kind == "JPEG":
                fails += check_trailing(os.path.join(in_dir, name), os.path.join(out_dir, name))
            elif mode == "default" and kind in ("PNG", "WebP"):
                fails += check_meta_chunks(os.path.join(out_dir, name), kind, out.get(name, {}))
            if name not in out:
                fails.append("no ExifTool output")
            if fails:
                c["tag failures"] += 1
                failures += [(mode, name, "tags", f) for f in fails]
            if name not in orig_pixels:
                try:
                    orig_pixels[name] = ("ok", frames(os.path.join(in_dir, name)))
                except Exception as e:  # any decoder error: PIL cannot read the original
                    orig_pixels[name] = ("skip", f"original not decodable ({type(e).__name__})")
            state, want = orig_pixels[name]
            if state == "skip":
                skips[want] += 1
                continue
            c["pixel checked"] += 1
            try:
                got = frames(os.path.join(out_dir, name))
            except Exception as e:
                got = f"output not decodable ({type(e).__name__})"
            if got == want:
                c["identical"] += 1
            else:
                c["pixel failures"] += 1
                failures.append((mode, name, "pixels", got if isinstance(got, str) else "differ"))
        lines.append(
            f"| {mode} | {c['outputs']} | {sum(errors.values())} | {c['unexpected refusals']}"
            f" | {c['tag failures']} | {c['pixel checked']} | {c['identical']}"
            f" | {c['pixel failures']} | {sum(skips.values())} |"
        )
        notes.append(f"- {mode}: exif-rs errors: {counts(errors)}; pixel skips: {counts(skips)}")
    lines += [""] + notes
    if failures:
        lines += ["", f"Failures: {len(failures)} (first 30; all in strip-failures.tsv)", ""]
        lines += [f"- {m} {n} {kind}: {d}" for m, n, kind, d in failures[:30]]
    text = "\n".join(lines) + "\n"
    with open(os.path.join(report, "strip.md"), "w", encoding="utf-8") as f:
        f.write(text)
    with open(os.path.join(report, "strip-failures.tsv"), "w", encoding="utf-8") as f:
        f.write("mode\tfile\tkind\tdetail\n")
        for row in failures:
            f.write("\t".join(row) + "\n")
    print(text)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
