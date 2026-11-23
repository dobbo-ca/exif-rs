#!/usr/bin/env python3
"""Ultra HDR and Motion Photo checks of `oracle strip` (docs/parity.md).

usage: gainmap.py <exiftool> <oracle> <work-dir> <report-dir>

No public Ultra HDR or Motion Photo sample has a clear licence, so the inputs are made here,
deterministically:
  uhdr.jpg         libultrahdr's ultrahdr_app (built with XMP on) from a 64x64 P010 + YUV420
                   gradient: primary with hdrgm:Version, Container:Directory and MPF, gain map
                   after the EOI.
  uhdr-orient.jpg  uhdr.jpg with an EXIF orientation of 6 written by ExifTool.
  motion.jpg       a Pillow JPEG with a Motion Photo XMP packet (Camera:MotionPhoto and a
                   directory) and a fake mp4 (ftyp and mdat boxes) after the EOI.
  uhdr-motion.jpg  uhdr.jpg with that video after the gain map, a directory of the three items
                   and the MPF entries fixed for the new primary length.

Then `oracle strip` in modes default, keep and orient1, and per output:
  - Pillow pixels of the primary image equal the input's;
  - default and orient1: the MPImage2 bytes ExifTool extracts equal the input's, so do the bytes
    at MPImageStart/MPImageLength of the output and the Pillow pixels of that gain map;
    ultrahdr_app decodes the output to the same raw pixels as the input; MotionPhotoVideo equals
    the appended video;
  - default: hdrgm:Version and the directory as in the input; orient1: IFD0:Orientation 1 (when
    the input has one);
  - keep: no MPF image, no video, nothing after the primary image's EOI.
Writes <report-dir>/gainmap.md; exits 1 when an input cannot be made or a check fails.
"""

import base64
import hashlib
import io
import json
import os
import subprocess
import sys
import warnings

from PIL import Image

from strip_check import primary_end

W = H = 64
XMP_SIG = b"http://ns.adobe.com/xap/1.0/\0"
VIDEO = (
    b"\x00\x00\x00\x18ftypisom\x00\x00\x02\x00isommp42"
    + (1008).to_bytes(4, "big") + b"mdat" + bytes((i * 37 + 11) % 256 for i in range(1000))
)
MODES = ("default", "keep", "orient1")


def run(cmd, **kw):
    return subprocess.run(cmd, capture_output=True, **kw)


def raw_inputs(work):
    """A P010 HDR and a YUV420 SDR gradient, 64x64 (narrow range)."""
    y10, uv10, y8, u8, v8 = bytearray(), bytearray(), bytearray(), bytearray(), bytearray()
    for r in range(H):
        for c in range(W):
            y10 += ((64 + (r + c) * 876 // (W + H - 2)) << 6).to_bytes(2, "little")
            y8.append(16 + (r + c) * 219 // (W + H - 2))
    for r in range(H // 2):
        for c in range(W // 2):
            uv10 += ((448 + 4 * c) << 6).to_bytes(2, "little")
            uv10 += ((576 - 4 * r) << 6).to_bytes(2, "little")
            u8.append(112 + c)
            v8.append(144 - r)
    with open(os.path.join(work, "hdr.p010"), "wb") as f:
        f.write(y10 + uv10)
    with open(os.path.join(work, "sdr.yuv"), "wb") as f:
        f.write(y8 + u8 + v8)


def segments(d):
    """(marker, start, end) of each segment before the first SOS of a well-formed JPEG."""
    out, i = [], 2
    while i + 4 <= len(d) and d[i] == 0xFF and d[i + 1] != 0xDA:
        end = i + 2 + int.from_bytes(d[i + 2:i + 4], "big")
        out.append((d[i + 1], i, end))
        i = end
    return out


def app1_xmp(packet):
    body = XMP_SIG + packet.encode()
    return b"\xff\xe1" + (len(body) + 2).to_bytes(2, "big") + body


def motion_packet(gain_map_len):
    items = '<rdf:li rdf:parseType="Resource"><Container:Item Item:Semantic="Primary" ' \
        'Item:Mime="image/jpeg" Item:Padding="0"/></rdf:li>'
    if gain_map_len:
        items += '<rdf:li rdf:parseType="Resource"><Container:Item Item:Semantic="GainMap" ' \
            f'Item:Mime="image/jpeg" Item:Length="{gain_map_len}"/></rdf:li>'
    items += '<rdf:li rdf:parseType="Resource"><Container:Item Item:Semantic="MotionPhoto" ' \
        f'Item:Mime="video/mp4" Item:Length="{len(VIDEO)}"/></rdf:li>'
    version = ' hdrgm:Version="1.0"' if gain_map_len else ""
    return (
        '<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF '
        'xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" '
        'xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/" '
        'xmlns:Camera="http://ns.google.com/photos/1.0/camera/" '
        'xmlns:Container="http://ns.google.com/photos/1.0/container/" '
        'xmlns:Item="http://ns.google.com/photos/1.0/container/item/" '
        'xmlns:dc="http://purl.org/dc/elements/1.1/"'
        f'{version} Camera:MotionPhoto="1" Camera:MotionPhotoVersion="1" '
        'Camera:MotionPhotoPresentationTimestampUs="0"><dc:creator><rdf:Seq><rdf:li>exif-rs '
        f'parity</rdf:li></rdf:Seq></dc:creator><Container:Directory><rdf:Seq>{items}</rdf:Seq>'
        '</Container:Directory></rdf:Description></rdf:RDF></x:xmpmeta>'
    )


def fix_mpf(primary):
    """Sets the MP Entries of the first MPF segment of `primary` (a bytearray that the gain map
    follows): entry 1 to the primary's length, entry 2 to the gain map's offset."""
    for m, s, _ in segments(primary):
        if m == 0xE2 and primary[s + 4:s + 8] == b"MPF\x00":
            t = s + 8
            order = "big" if primary[t:t + 2] == b"MM" else "little"
            rd = lambda at, n: int.from_bytes(primary[at:at + n], order)
            ifd = t + rd(t + 4, 4)
            for k in range(rd(ifd, 2)):
                e = ifd + 2 + 12 * k
                if rd(e, 2) == 0xB002:
                    at = t + rd(e + 8, 4)
                    primary[at + 4:at + 8] = len(primary).to_bytes(4, order)
                    primary[at + 24:at + 28] = (len(primary) - t).to_bytes(4, order)
                    return
    raise ValueError("no MPF entries")


def exiftool_json(exiftool, paths):
    # -b: the MPImage2 and MotionPhotoVideo bytes as base64.
    r = run(["perl", exiftool, "-j", "-n", "-G1", "-a", "-b", "-MPF:all", "-MPImage2",
             "-MotionPhotoVideo", "-XMP:all", "-IFD0:Orientation"] + paths)
    text = r.stdout.decode("utf-8", "replace")
    return {os.path.basename(o["SourceFile"]): o for o in (json.loads(text) if text.strip() else [])}


def binary(v):
    if v is None:
        return None
    if isinstance(v, str) and v.startswith("base64:"):
        return base64.b64decode(v[7:])
    return str(v).encode()


def pixels(data):
    with Image.open(io.BytesIO(data)) as im:
        im.load()
        return (im.mode, im.size, hashlib.sha256(im.tobytes()).hexdigest())


def decode(path):
    """ultrahdr_app's PQ RGBA1010102 decode of `path`, or None when it fails."""
    out = path + ".raw"
    r = run(["ultrahdr_app", "-m", "1", "-j", path, "-o", "2", "-O", "5", "-z", out])
    if r.returncode != 0 or not os.path.exists(out):
        return None
    with open(out, "rb") as f:
        data = f.read()
    os.remove(out)
    return data


def make_inputs(exiftool, work, gen):
    """The four inputs in `gen`, and what they hold: {name: (gain map bytes, video bytes)}."""
    raw_inputs(work)
    uhdr = os.path.join(gen, "uhdr.jpg")
    r = run(["ultrahdr_app", "-m", "0", "-p", "hdr.p010", "-y", "sdr.yuv", "-a", "0", "-b", "1",
             "-w", str(W), "-h", str(H), "-C", "1", "-c", "1", "-t", "1", "-M", "0", "-z", uhdr],
            cwd=work)
    if r.returncode != 0:
        raise RuntimeError(f"ultrahdr_app encode: {r.stderr.decode('utf-8', 'replace')[-500:]}")
    with open(uhdr, "rb") as f:
        d = f.read()
    end = primary_end(d)
    gm = d[end:]
    if not gm.startswith(b"\xff\xd8"):
        raise RuntimeError("uhdr.jpg: no gain map after the primary image")
    r = run(["perl", exiftool, "-q", "-Orientation#=6", "-o",
             os.path.join(gen, "uhdr-orient.jpg"), uhdr])
    if r.returncode != 0:
        raise RuntimeError(f"exiftool -Orientation#=6: {r.stderr.decode('utf-8', 'replace')}")

    im = Image.new("RGB", (W, H))
    im.putdata([(4 * c, 4 * r, (r + c) * 2 % 256) for r in range(H) for c in range(W)])
    buf = io.BytesIO()
    im.save(buf, "JPEG", quality=90)
    j = buf.getvalue()
    at = 2 + (2 + int.from_bytes(j[4:6], "big") if j[2:4] == b"\xff\xe0" else 0)
    with open(os.path.join(gen, "motion.jpg"), "wb") as f:
        f.write(j[:at] + app1_xmp(motion_packet(0)) + j[at:] + VIDEO)

    primary = bytearray()
    segs = segments(d)
    xmp = next(s for s in segs if s[0] == 0xE1 and d[s[1] + 4:s[1] + 33] == XMP_SIG)
    primary += d[:xmp[1]] + app1_xmp(motion_packet(len(gm))) + d[xmp[2]:end]
    fix_mpf(primary)
    with open(os.path.join(gen, "uhdr-motion.jpg"), "wb") as f:
        f.write(primary + gm + VIDEO)
    return {
        "uhdr.jpg": (gm, b""),
        "uhdr-orient.jpg": (gm, b""),
        "motion.jpg": (b"", VIDEO),
        "uhdr-motion.jpg": (gm, VIDEO),
    }


def check(mode, name, gm, video, in_path, out_path, et_in, et_out, decoded_in):
    """The failed checks of one output, and how many checks ran."""
    fails, n = [], 0

    def expect(ok, what):
        nonlocal n
        n += 1
        if not ok:
            fails.append(what)

    with open(in_path, "rb") as f:
        din = f.read()
    with open(out_path, "rb") as f:
        dout = f.read()
    expect(pixels(din) == pixels(dout), "primary pixels differ")
    mp_in, mp_out = binary(et_in.get("MPImage2:MPImage2")), binary(et_out.get("MPImage2:MPImage2"))
    vid_out = next((binary(v) for k, v in et_out.items() if k.endswith(":MotionPhotoVideo")), None)
    if mode == "keep":
        expect(mp_out is None, "MPImage2 kept")
        expect(vid_out is None, "MotionPhotoVideo kept")
        expect(primary_end(dout) == len(dout), "bytes after the EOI kept")
        return fails, n
    if gm:
        expect(mp_in == gm, "input: MPImage2 is not the gain map")
        expect(mp_out == gm, "MPImage2 differs")
        start = et_out.get("MPImage2:MPImageStart")
        length = et_out.get("MPImage2:MPImageLength")
        expect(
            isinstance(start, int) and isinstance(length, int) and dout[start:start + length] == gm,
            f"bytes at MPImageStart {start} length {length} are not the gain map",
        )
        try:
            same = mp_out is not None and pixels(mp_out) == pixels(gm)
        except Exception as e:  # any decoder error
            same = False
            fails.append(f"gain map not decodable ({type(e).__name__})")
        expect(same, "gain map pixels differ")
        decoded_out = decode(out_path)
        expect(decoded_in is not None, "input: ultrahdr_app decode failed")
        expect(decoded_out is not None, "ultrahdr_app decode failed")
        expect(decoded_out == decoded_in, "ultrahdr_app decode differs")
    if video:
        expect(vid_out == video, "MotionPhotoVideo differs")
    expect(dout.endswith(gm + video), "bytes after the EOI differ")
    if mode == "default":
        for k in ("XMP-hdrgm:Version", "XMP-GContainer:DirectoryItemSemantic",
                  "XMP-GContainer:DirectoryItemLength", "XMP-GCamera:MotionPhoto"):
            expect(et_in.get(k) == et_out.get(k), f"{k}: input {et_in.get(k)} output {et_out.get(k)}")
    else:
        if "IFD0:Orientation" in et_in:
            expect(et_out.get("IFD0:Orientation") == 1, "IFD0:Orientation is not 1")
    return fails, n


def main():
    exiftool, oracle, work, report = sys.argv[1:5]
    warnings.simplefilter("ignore")
    gen = os.path.join(work, "in")
    os.makedirs(gen, exist_ok=True)
    lines = ["## Ultra HDR and Motion Photo checks", ""]
    try:
        files = make_inputs(exiftool, work, gen)
    except Exception as e:  # report it, as a failure
        lines.append(f"Inputs could not be made: {type(e).__name__}: {e}")
        with open(os.path.join(report, "gainmap.md"), "w", encoding="utf-8") as f:
            f.write("\n".join(lines) + "\n")
        print("\n".join(lines))
        return 1
    names = sorted(files)
    et_in = exiftool_json(exiftool, [os.path.join(gen, n) for n in names])
    decoded = {n: decode(os.path.join(gen, n)) for n in names if files[n][0]}
    lines += ["| file | mode | checks | passed |", "|---|---|---:|---:|"]
    failures, total, passed = [], 0, 0
    for mode in MODES:
        out_dir = os.path.join(work, mode)
        r = run([oracle, "strip", mode, gen, out_dir])
        if r.returncode != 0:
            failures.append(f"{mode}: oracle strip failed")
        et_out = exiftool_json(exiftool, [os.path.join(out_dir, n) for n in names
                                          if os.path.exists(os.path.join(out_dir, n))])
        for name in names:
            out_path = os.path.join(out_dir, name)
            if not os.path.exists(out_path):
                failures.append(f"{mode} {name}: no output")
                lines.append(f"| {name} | {mode} | 1 | 0 |")
                total += 1
                continue
            gm, video = files[name]
            fails, n = check(mode, name, gm, video, os.path.join(gen, name), out_path,
                             et_in.get(name, {}), et_out.get(name, {}), decoded.get(name))
            total += n
            passed += n - len(fails)
            failures += [f"{mode} {name}: {f}" for f in fails]
            lines.append(f"| {name} | {mode} | {n} | {n - len(fails)} |")
    lines += ["", f"Files: {len(names)}; checks: {total}; passed: {passed}."]
    if failures:
        lines += ["", f"Failures: {len(failures)}", ""] + [f"- {f}"[:300] for f in failures]
    text = "\n".join(lines) + "\n"
    with open(os.path.join(report, "gainmap.md"), "w", encoding="utf-8") as f:
        f.write(text)
    print(text)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
