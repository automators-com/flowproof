"""Drives the Flowproof desktop app with real OS input while the screen is
recorded, for the captured set (README.md). Navigation only: nothing is
created, changed or run.

The app's web view shows nothing to AppleScript's accessibility tree, so
its controls are clicked by position: the window is moved to a fixed place
on the main display first, and the positions below are in that window, in
points, read off a screenshot of it. The expected flow is written from the
steps performed.

    uv run --no-project --with pyobjc-framework-Quartz python drive_app.py
"""
from __future__ import annotations

import json
import subprocess
import time
from pathlib import Path

from drive_chrome import HERE, click, quote, type_text

WINDOW = (120, 60, 1440, 960)
NAME = "flowproof-app"
# (label, x, y in the window, text to type or None)
STEPS = [
    ("Runs", 67, 194, None),
    ("Environments", 96, 232, None),
    ("Flows", 69, 156, None),
    ("Search flows", 380, 133, "kanban"),
    ("Settings", 77, 270, None),
]


def place_window():
    x, y, w, h = WINDOW
    subprocess.run(["osascript", "-e", f'''tell application "System Events" to tell (first process whose bundle identifier contains "flowproof")
set frontmost to true
set position of window 1 to {{{x}, {y}}}
set size of window 1 to {{{w}, {h}}}
end tell'''], check=True)


def main():
    subprocess.run(["open", "-a", "/Applications/flowproof.app"], check=True)
    time.sleep(3)
    place_window()
    time.sleep(1)
    recorder = subprocess.Popen(
        ["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-f", "avfoundation",
         "-capture_cursor", "1", "-capture_mouse_clicks", "1", "-framerate", "30", "-pixel_format", "nv12",
         "-i", "Capture screen 0:none", "-c:v", "libx264", "-preset", "veryfast", "-pix_fmt", "yuv420p",
         str(HERE / f"{NAME}.mp4")], stdin=subprocess.PIPE)
    started = time.monotonic()
    time.sleep(2.5)
    steps, timeline = [], []
    for label, x, y, text in STEPS:
        at = round((time.monotonic() - started) * 1000)
        click(WINDOW[0] + x, WINDOW[1] + y)
        if text is None:
            steps.append(f"Click {quote(label)}")
        else:
            time.sleep(0.3)
            type_text(text)
            steps.append(f"Type {text} into the {quote(label)} field")
        timeline.append({"step": steps[-1], "at_ms": at, "end_ms": round((time.monotonic() - started) * 1000)})
        time.sleep(2)
    time.sleep(1.5)
    recorder.communicate(b"q", timeout=30)
    (HERE / f"{NAME}.expected.flow.yaml").write_text(
        f"# Expected draft for the captured {NAME} recording, written by drive_app.py\n"
        f"# from the steps it performed. Regenerate rather than edit it.\n"
        f"name: \"Look around the Flowproof app\"\napp: \"flowproof-desktop\"\nsteps:\n"
        + "".join(f"  - {quote(s)}\n" for s in steps))
    (HERE / f"{NAME}.actions.json").write_text(json.dumps(timeline, indent=2) + "\n")


if __name__ == "__main__":
    main()
