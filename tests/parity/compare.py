#!/usr/bin/env python3
"""Compares exif-rs (examples/oracle.rs) with ExifTool 13.64, field by field (docs/parity.md).

usage: compare.py <in-dir> <exiftool.json> <exiftool-blocks.json> <oracle.jsonl>
                  <known-differences.tsv> <report-dir>

A key matches when both sides have it with the same JSON type (number, string, boolean, list)
and the same exact text. Writes <report-dir>/compare.md and <report-dir>/read-mismatches.tsv.
Exits 1 on any mismatch that known-differences.tsv does not list.
"""

import json
import os
import sys
from collections import Counter

KEYS = [
    "IFD0:Orientation", "IFD0:Make", "IFD0:Model",
    "ExifIFD:DateTimeOriginal", "ExifIFD:SubSecTimeOriginal", "ExifIFD:OffsetTimeOriginal",
    "ExifIFD:ExposureTime", "ExifIFD:FNumber", "ExifIFD:ISO", "ExifIFD:FocalLength",
    "ExifIFD:ExifImageWidth", "ExifIFD:ExifImageHeight", "ExifIFD:LensMake", "ExifIFD:LensModel",
    "GPS:GPSLatitudeRef", "GPS:GPSLatitude", "GPS:GPSLongitudeRef", "GPS:GPSLongitude",
    "GPS:GPSAltitudeRef", "GPS:GPSAltitude", "GPS:GPSTimeStamp", "GPS:GPSDateStamp",
    "Composite:GPSLatitude", "Composite:GPSLongitude", "Composite:GPSAltitude",
    "Composite:GPSDateTime", "Composite:SubSecDateTimeOriginal",
    "IFD1:ThumbnailImage",
    "XMP-tiff:Orientation", "XMP-xmp:CreateDate", "XMP-xmp:Rating",
]
PREFIXES = ("XMP-hdrgm:",)
# Printed by ExifTool only when requested by name (the second ExifTool command).
BLOCK_KEYS = ["EXIF:EXIF", "XMP:XMP", "ICC_Profile:ICC_Profile"]
READ_TYPES = {"JPEG", "PNG", "WEBP"}


class Num(str):
    """A JSON number, kept as its exact text."""


def loads(text):
    return json.loads(text, parse_float=Num, parse_int=Num)


def load_json(path):
    with open(path, "rb") as f:
        text = f.read().decode("utf-8", "replace")
    return loads(text) if text.strip() else []


def by_name(objs):
    return {os.path.basename(o["SourceFile"]): o for o in objs}


def canon(v):
    """The JSON type and exact text of a value; ("absent",) for a missing key."""
    if v is None:
        return ("absent",)
    if isinstance(v, list):
        return ("list", tuple(canon(x) for x in v))
    if isinstance(v, bool):
        return ("bool", v)
    if isinstance(v, Num):
        return ("num", str(v))
    if isinstance(v, str):
        return ("str", v)
    return (type(v).__name__, json.dumps(v, sort_keys=True))


def show(v, limit=200):
    s = repr(canon(v))
    return s if len(s) <= limit else s[:limit] + "..."


def compared(key):
    return key in KEYS or key in BLOCK_KEYS or key.startswith(PREFIXES)


def container(path):
    with open(path, "rb") as f:
        head = f.read(12)
    if head.startswith(b"\xff\xd8\xff"):
        return "JPEG"
    if head.startswith(b"\x89PNG\r\n\x1a\n"):
        return "PNG"
    if head[:4] == b"RIFF" and head[8:12] == b"WEBP":
        return "WebP"
    return "other"


def load_known(path):
    known = {}
    if os.path.exists(path):
        with open(path, encoding="utf-8") as f:
            for line in f:
                parts = line.rstrip("\n").split("\t")
                if line.startswith("#") or len(parts) < 2:
                    continue
                known[(parts[0], parts[1])] = parts[2] if len(parts) > 2 else ""
    return known


def counts(c):
    return ", ".join(f"{k} {n}" for k, n in sorted(c.items())) or "none"


def main():
    in_dir, et_path, blocks_path, oracle_path, known_path, report = sys.argv[1:7]
    et = by_name(load_json(et_path))
    for name, obj in by_name(load_json(blocks_path)).items():
        et.setdefault(name, {}).update({k: v for k, v in obj.items() if k in BLOCK_KEYS})
    oracle = {}
    with open(oracle_path, encoding="utf-8") as f:
        for line in f:
            if line.strip():
                obj = loads(line)
                oracle[obj["SourceFile"]] = obj
    known = load_known(known_path)

    names = sorted(os.listdir(in_dir))
    containers, file_types, errors, skipped = Counter(), Counter(), Counter(), 0
    stats = {k: Counter() for k in KEYS + BLOCK_KEYS}
    mismatches, used_known = [], set()
    for name in names:
        containers[container(os.path.join(in_dir, name))] += 1
        e = et.get(name, {})
        o = oracle.get(name, {"Error": "missing-from-oracle"})
        file_type = e.get("File:FileType")
        file_types[str(file_type)] += 1
        err = o.get("Error")
        if err is not None:
            errors[err] += 1
            if err == "unknown-format" and file_type not in READ_TYPES:
                skipped += 1
                continue
        for key in sorted(k for k in set(e) | set(o) if compared(k)):
            s = stats.setdefault(key, Counter())
            s["compared"] += 1
            if key in e and key in o and canon(e[key]) == canon(o[key]):
                s["match"] += 1
            elif (name, key) in known:
                s["known"] += 1
                used_known.add((name, key))
            else:
                s["mismatch"] += 1
                mismatches.append((name, key, show(e.get(key)), show(o.get(key))))

    read_types = sum(n for t, n in file_types.items() if t in READ_TYPES)
    lines = [
        "## Read parity",
        "",
        f"Inputs: {len(names)}",
        f"Inputs per container (magic bytes): {counts(containers)}",
        f"Inputs ExifTool reads as JPEG/PNG/WEBP (File:FileType): {read_types}",
        f"ExifTool File:FileType: {counts(file_types)}",
        f"exif-rs errors: {counts(errors)}",
        f"Skipped (not JPEG/PNG/WEBP for ExifTool, unknown-format for exif-rs): {skipped}",
        f"Mismatches: {len(mismatches)} (known differences used: {len(used_known)},"
        f" listed but unused: {len(set(known) - used_known)})",
        "",
        "| key | compared | match | mismatch | known |",
        "|---|---:|---:|---:|---:|",
    ]
    for key, s in stats.items():
        lines.append(f"| {key} | {s['compared']} | {s['match']} | {s['mismatch']} | {s['known']} |")
    if mismatches:
        lines += ["", "First mismatches (all in read-mismatches.tsv):", ""]
        lines += [f"- {n} {k}: exiftool {ev} oracle {ov}" for n, k, ev, ov in mismatches[:30]]
    text = "\n".join(lines) + "\n"
    with open(os.path.join(report, "compare.md"), "w", encoding="utf-8") as f:
        f.write(text)
    with open(os.path.join(report, "read-mismatches.tsv"), "w", encoding="utf-8") as f:
        f.write("file\tkey\texiftool\toracle\n")
        for row in mismatches:
            f.write("\t".join(row) + "\n")
    print(text)
    return 1 if mismatches else 0


if __name__ == "__main__":
    sys.exit(main())
