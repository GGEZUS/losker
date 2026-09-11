#!/usr/bin/env python3
"""Generate losker ASCII frame files from areofyl/fetch.

Runs `fetch -l <logo> --no-info --no-color --rotate-y` under a pty (it sizes
its canvas from the TTY ioctl), splits the frame stream, finds the rotation
loop by near-match, and writes the losker frame format:

    # delay: 50
    <frame 0>
    ---
    <frame 1>
    ...

Every frame is normalized to one fixed canvas (the union bounding box of the
whole loop): if each frame were trimmed to its own silhouette, the label
would resize/shift per frame and the animation would jitter — boot-proven.

`--logo KEY` is any fastfetch builtin logo key (`fastfetch --list-logos`;
case-insensitive match). Pre-baked distros live in assets/logos/<key>.txt.

Requires: fetch built from https://github.com/areofyl/fetch (found via
$LOSKER_GENART_FETCH, /usr/local/lib/losker/fetch, then the vendored
tools/vendor/fetch/fetch) and fastfetch installed (logo source). Defaults
reproduce the deployed greeter art: one revolution ~10.5 s at 20 fps
(native fetch cadence, speed 1).
"""

import argparse
import fcntl
import os
import pty
import re
import select
import struct
import subprocess
import sys
import termios

ANSI = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]")


def find_fetch(explicit):
    """First usable fetch binary: explicit arg, $LOSKER_GENART_FETCH, the
    installed location, then the repo-relative vendored default."""
    candidates = [
        explicit,
        os.environ.get("LOSKER_GENART_FETCH"),
        "/usr/local/lib/losker/fetch",
        "tools/vendor/fetch/fetch",
    ]
    for c in candidates:
        if c and os.path.isfile(c) and os.access(c, os.X_OK):
            return c
    return candidates[-1]


def capture(cols, rows, args):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
    p = subprocess.Popen(
        args, stdin=slave, stdout=slave, stderr=slave, close_fds=True
    )
    os.close(slave)
    out = b""
    while True:
        r, _, _ = select.select([master], [], [], 15)
        if not r:
            break
        try:
            data = os.read(master, 1 << 20)
        except OSError:
            break
        if not data:
            break
        out += data
    p.wait()
    os.close(master)
    return out.decode("utf-8", "replace")


def parse_frames(raw):
    """Split the frame stream into verbatim canvas rows (ANSI/NUL stripped).

    Rows keep their leading padding and row count — the geometry fix happens
    later, on the union box, so the canvas stays identical frame to frame.
    """
    frames = []
    for chunk in raw.split("\x1b[H")[1:]:
        lines = [ANSI.sub("", ln).replace("\x00", "").rstrip() for ln in chunk.split("\r\n")]
        while lines and not lines[-1]:
            lines.pop()
        if lines:
            frames.append(lines)
    return frames


def hamming(a, b):
    if len(a) != len(b):
        return 1 << 30
    return sum(
        abs(len(x) - len(y)) + sum(cx != cy for cx, cy in zip(x, y))
        for x, y in zip(a, b)
    )


def find_loop(frames, lo, hi):
    """Smallest offset in [lo, hi) whose frame nearly repeats frames[1]."""
    base = frames[1]
    best = min(((hamming(base, frames[i]), i) for i in range(lo, min(hi, len(frames)))))
    return best[1], best[0]


def normalize(loop):
    """Crop all frames to the union bounding box → identical dimensions."""
    n = min(len(f) for f in loop)
    loop = [f[:n] for f in loop]
    first = min(next(i for i, l in enumerate(f) if l) for f in loop)
    last = max(max(i for i, l in enumerate(f) if l) for f in loop)
    lead = min(len(l) - len(l.lstrip(" ")) for f in loop for l in f if l)
    width = max(len(l) - lead for f in loop for l in f if l)

    out = []
    height = last - first + 1
    for f in loop:
        rows = f[first : last + 1]
        while len(rows) < height:  # blank bottom canvas rows were popped
            rows.append("")
        rows = [(l[lead : lead + width] if l else "").ljust(width) for l in rows]
        out.append("\n".join(rows))
    return out, width, height


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--logo", default="arch", help="fastfetch builtin logo key (see `fastfetch --list-logos`)")
    ap.add_argument("--fetch-bin", default=None, help="path to the fetch binary (default: search, see module doc)")
    ap.add_argument("--size", default="1", help="logo render scale passed to fetch (--size); 2.0 doubles the "
                 "canvas so the same on-screen logo is drawn with half-size, finer characters")
    ap.add_argument("--speed", default="1", help="rotation speed (frames/rev ~ 188/speed at speed 1)")
    ap.add_argument("--capture", type=int, default=260, help="frames to capture")
    ap.add_argument("--min-loop", type=int, default=160)
    ap.add_argument("--max-loop", type=int, default=220)
    ap.add_argument("--delay", type=int, default=50, help="ms per frame written to header")
    ap.add_argument("--out", default="assets/losker-ascii.txt")
    a = ap.parse_args()

    fetch_bin = find_fetch(a.fetch_bin)
    args = [
        fetch_bin, "-l", a.logo, "--no-info", "--no-color", "--rotate-y",
        "-s", a.speed, "--frames", str(a.capture),
    ]
    if a.size != "1":
        args += ["--size", a.size]
    print(f"capturing {a.capture} frames: {' '.join(args)}", file=sys.stderr)
    # the pty is fetch's canvas ceiling — scale it with the logo or the
    # render gets clipped, not bigger
    scale = max(1.0, float(a.size))
    raw = capture(int(120 * scale), int(60 * scale), args)
    frames = parse_frames(raw)
    if len(frames) < a.max_loop + 10:
        sys.exit(f"only got {len(frames)} frames — fetch failed or exited early")

    period, dist = find_loop(frames, a.min_loop, a.max_loop)
    loop = frames[1 : period + 1]  # frames[1] == frames[period] (near-exact)
    loop, cols, rows = normalize(loop)
    if cols > 110:
        print(
            f"WARNING: {a.logo} canvas is {cols} cols — the 120-col capture pty "
            "may have clipped it; consider a wider capture or a _small logo key",
            file=sys.stderr,
        )
    rev = len(loop) * a.delay / 1000
    print(
        f"loop: {len(loop)} frames, wrap mismatch {dist} chars, "
        f"{cols}x{rows} canvas, {rev:.1f}s per revolution",
        file=sys.stderr,
    )

    with open(a.out, "w") as fh:
        fh.write(f"# delay: {a.delay}\n")
        for i, f in enumerate(loop):
            if i:
                fh.write("---\n")
            fh.write(f + "\n")
    size = os.path.getsize(a.out)
    print(f"wrote {a.out}: {len(loop)} frames, {size} bytes", file=sys.stderr)


if __name__ == "__main__":
    main()
