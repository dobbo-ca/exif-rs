#!/usr/bin/env python3
"""Checks the outputs of `oracle strip` with ExifTool and Pillow (docs/parity.md).

usage: strip_check.py <in-dir> <exiftool.json> <strip-dir> <report-dir>

<strip-dir>/<mode>/ holds the outputs of mode all, keep and orient1, <strip-dir>/<mode>.json
ExifTool's JSON of them and <strip-dir>/<mode>.errors the files exif-rs refused. Writes
<report-dir>/strip.md and <report-dir>/strip-failures.tsv. Exits 1 on any failure.
"""

import hashlib
import os
import sys
import warnings
from collections import Counter

from PIL import Image

from compare import Num, canon, load_json, by_name, counts

MODES = ("all", "keep", "orient1")

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


def check_all(orig, out):
    return [f"{k} remains" for k in keys(out) if not allowed(k)]


def check_keep(orig, out):
    kept = {"IFD0:Orientation", "File:ExifByteOrder"}
    fails = [f"{k} remains" for k in keys(out) if not (allowed(k) or k in kept or is_icc(k))]
    for k in ["IFD0:Orientation"] + sorted(k for k in set(orig) | set(out) if is_icc(k)):
        if canon(orig.get(k)) != canon(out.get(k)):
            fails.append(f"{k}: original {canon(orig.get(k))} output {canon(out.get(k))}"[:300])
    if "File:ExifByteOrder" in out and "IFD0:Orientation" not in out:
        fails.append("File:ExifByteOrder without IFD0:Orientation")
    return fails


def check_orient1(orig, out):
    fails = []
    for k in sorted(set(keys(orig)) | set(keys(out))):
        if k.startswith("System:"):
            continue
        want = canon(Num("1") if k in orig else None) if k in ORIENTATION else canon(orig.get(k))
        if canon(out.get(k)) != want:
            fails.append(f"{k}: want {want} output {canon(out.get(k))}"[:300])
    return fails


CHECKS = {"all": check_all, "keep": check_keep, "orient1": check_orient1}


def frames(path):
    """Mode, size and pixel hash of every frame. JPEG: the first frame only. Pillow opens a
    JPEG with an MPF segment as MPO, whose later frames are the images after EOI, which strip
    drops by design (docs/design.md)."""
    with Image.open(path) as im:
        out = []
        n = 1 if im.format in ("JPEG", "MPO") else getattr(im, "n_frames", 1)
        for i in range(n):
            im.seek(i)
            im.load()
            out.append((im.mode, im.size, hashlib.sha256(im.tobytes()).hexdigest()))
        return out


def main():
    in_dir, et_path, strip_dir, report = sys.argv[1:5]
    warnings.simplefilter("ignore")
    orig = by_name(load_json(et_path))
    orig_pixels = {}
    failures = []
    lines = [
        "## Strip checks",
        "",
        "| mode | outputs | exif-rs errors | tag failures | pixel checked | identical"
        " | pixel failures | pixel skipped |",
        "|---|---:|---:|---:|---:|---:|---:|---:|",
    ]
    notes = []
    for mode in MODES:
        out_dir = os.path.join(strip_dir, mode)
        out = by_name(load_json(os.path.join(strip_dir, mode + ".json")))
        errors = Counter()
        errors_path = os.path.join(strip_dir, mode + ".errors")
        if os.path.exists(errors_path):
            with open(errors_path, encoding="utf-8") as f:
                errors.update(line.rstrip("\n").split("\t")[-1] for line in f if line.strip())
        c, skips = Counter(), Counter()
        names = sorted(os.listdir(out_dir)) if os.path.isdir(out_dir) else []
        for name in names:
            c["outputs"] += 1
            fails = CHECKS[mode](orig.get(name, {}), out.get(name, {}))
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
            f"| {mode} | {c['outputs']} | {sum(errors.values())} | {c['tag failures']}"
            f" | {c['pixel checked']} | {c['identical']} | {c['pixel failures']}"
            f" | {sum(skips.values())} |"
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
