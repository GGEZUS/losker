#!/usr/bin/env python3
"""Preview the greeter's ASCII animation in a terminal — no reboot needed.

Plays the exact frames file the greeter loads (/etc/greetd/osk-ascii.txt),
with the same parsing rules and the same `# delay:` cadence, so what you
see here is what spins on the greeter. Pure stdout — spawns nothing.

Usage:  python3 tools/preview-art.py [frames-file]   (Ctrl+C to stop)
"""

import sys
import time

DEFAULT_FRAME_MS = 100


def parse_frames(text):
    """Mirror of src/ui/ascii.rs parse_frames — same rules, same clamp."""
    delay_ms = DEFAULT_FRAME_MS
    frames, cur = [], []
    for line in text.splitlines():
        if line.startswith("# delay:"):
            try:
                delay_ms = min(max(int(line[len("# delay:"):].strip()), 16), 1000)
            except ValueError:
                pass
        elif line.strip() == "---":
            frames.append("\n".join(cur))
            cur = []
        else:
            cur.append(line)
    if cur:
        frames.append("\n".join(cur))
    return [f for f in frames if f.strip()], delay_ms


def main():
    path = sys.argv[1] if len(sys.argv) > 1 else "/etc/greetd/osk-ascii.txt"
    try:
        text = open(path).read()
    except OSError as e:
        sys.exit(f"cannot read {path}: {e}")
    frames, delay = parse_frames(text)
    if not frames:
        sys.exit(f"no frames in {path}")

    rows = len(frames[0].split("\n"))
    cols = max(len(l) for l in frames[0].split("\n"))
    rev = len(frames) * delay / 1000
    print(
        f"{path}: {len(frames)} frames, {cols}x{rows}, "
        f"{delay}ms/frame → {rev:.1f}s per revolution. Ctrl+C to stop."
    )
    time.sleep(1.2)

    try:
        sys.stdout.write("\x1b[?25l\x1b[2J")  # hide cursor, clear
        while True:
            for f in frames:
                sys.stdout.write("\x1b[H" + f + "\n")
                sys.stdout.flush()
                time.sleep(delay / 1000)
    except KeyboardInterrupt:
        pass
    finally:
        sys.stdout.write("\x1b[?25h")  # restore cursor
        sys.stdout.flush()


if __name__ == "__main__":
    main()
