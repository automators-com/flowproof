"""Drives a visible Chrome window with real OS input while the screen is
recorded, for the captured set (README.md).

The pointer is the real macOS pointer, moved and clicked through Quartz
events, and keys are real key events, so the recording has the cursor, the
click highlights and the pacing a screen recorder sees. Chrome's debugging
protocol is only used to read where an element is and what its label says.
The expected flow is written from the steps this script performs.

    uv run --no-project --with pyobjc-framework-Quartz --with websocket-client \\
        python drive_chrome.py tricentis-truck

Needs Accessibility permission for the terminal (to post input events).
"""
from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import time
import http.client
from pathlib import Path

import Quartz
import websocket

CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
PORT = 9333
HERE = Path(__file__).resolve().parent

# Each script: where it starts, and its steps. ("click", id), ("type", id, text),
# ("select", id, option).
SCRIPTS = {
    "tricentis-truck": {
        "name": "Truck quote, vehicle data",
        "url": "https://sampleapp.tricentis.com/101",
        "steps": [
            ("click", "nav_truck"),
            ("select", "make", "BMW"),
            ("type", "engineperformance", "150"),
            ("type", "dateofmanufacture", "01/15/2020"),
            ("select", "numberofseats", "2"),
            ("select", "fuel", "Diesel"),
            ("type", "payload", "500"),
            ("type", "totalweight", "3500"),
            ("type", "listprice", "50000"),
            ("type", "licenseplatenumber", "TRUCK1"),
            ("type", "annualmileage", "12000"),
            ("click", "nextenterinsurantdata"),
            ("type", "firstname", "Nikolai"),
            ("type", "lastname", "Tester"),
        ],
    },
}

# Where an element's centre is on screen, in points, and its visible label.
FIND = r"""(() => {
  const e = document.getElementById(%s);
  if (!e) return JSON.stringify(null);
  e.scrollIntoView({block: "center"});
  const r = e.getBoundingClientRect();
  // A field's own <label for>, else the label in the same field row;
  // anything else by its visible text.
  let label = e.id && document.querySelector(`label[for="${e.id}"]`);
  const field = ["INPUT", "SELECT", "TEXTAREA"].includes(e.tagName);
  for (let up = e.parentElement, n = 0; field && !label && up && n < 4; up = up.parentElement, n++) {
    label = up.querySelector("label");
  }
  const name = (label ? label.textContent : (e.textContent || e.value || "")).replace(/\s+/g, " ").trim();
  const top = window.screenY + (window.outerHeight - window.innerHeight);
  const left = window.screenX + (window.outerWidth - window.innerWidth) / 2;
  return JSON.stringify({x: left + r.left + r.width / 2, y: top + r.top + r.height / 2, label: name});
})()"""


class Page:
    def __init__(self):
        # Chrome's debugging endpoint, on the loopback address only.
        conn = http.client.HTTPConnection("127.0.0.1", PORT, timeout=5)
        conn.request("GET", "/json")
        targets = json.loads(conn.getresponse().read())
        conn.close()
        page = next(t for t in targets if t["type"] == "page")
        self.ws = websocket.create_connection(page["webSocketDebuggerUrl"])
        self.id = 0

    def eval(self, expression: str):
        self.id += 1
        self.ws.send(json.dumps({"id": self.id, "method": "Runtime.evaluate",
                                 "params": {"expression": expression, "returnByValue": True}}))
        while True:
            reply = json.loads(self.ws.recv())
            if reply.get("id") == self.id:
                return reply["result"]["result"].get("value")

    def find(self, element_id: str) -> dict:
        for _ in range(40):
            found = json.loads(self.eval(FIND % json.dumps(element_id)) or "null")
            if found:
                time.sleep(0.3)  # let a scroll settle, then measure again
                return json.loads(self.eval(FIND % json.dumps(element_id)))
            time.sleep(0.25)
        raise SystemExit(f"no element #{element_id} on the page")


def pointer() -> tuple[float, float]:
    p = Quartz.CGEventGetLocation(Quartz.CGEventCreate(None))
    return p.x, p.y


def move(x: float, y: float, seconds: float = 0.6):
    x0, y0 = pointer()
    steps = max(1, int(seconds * 60))
    for k in range(1, steps + 1):
        t = k / steps
        t = t * t * (3 - 2 * t)
        point = (x0 + (x - x0) * t, y0 + (y - y0) * t)
        event = Quartz.CGEventCreateMouseEvent(None, Quartz.kCGEventMouseMoved, point, Quartz.kCGMouseButtonLeft)
        Quartz.CGEventPost(Quartz.kCGHIDEventTap, event)
        time.sleep(seconds / steps)


def click(x: float, y: float):
    move(x, y)
    time.sleep(0.15)
    for kind in (Quartz.kCGEventLeftMouseDown, Quartz.kCGEventLeftMouseUp):
        Quartz.CGEventPost(Quartz.kCGHIDEventTap,
                           Quartz.CGEventCreateMouseEvent(None, kind, (x, y), Quartz.kCGMouseButtonLeft))
        time.sleep(0.08)


def key(code: int):
    for down in (True, False):
        Quartz.CGEventPost(Quartz.kCGHIDEventTap, Quartz.CGEventCreateKeyboardEvent(None, code, down))
        time.sleep(0.03)


def type_text(text: str, per_key: float = 0.14):
    for ch in text:
        for down in (True, False):
            event = Quartz.CGEventCreateKeyboardEvent(None, 0, down)
            Quartz.CGEventKeyboardSetUnicodeString(event, len(ch), ch)
            Quartz.CGEventPost(Quartz.kCGHIDEventTap, event)
        time.sleep(per_key)


RETURN = 36


def quote(s: str) -> str:
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


def main(name: str):
    script = SCRIPTS[name]
    profile = tempfile.mkdtemp(prefix="drive-chrome-")
    chrome = subprocess.Popen([CHROME, f"--user-data-dir={profile}", f"--remote-debugging-port={PORT}",
                               "--no-first-run", "--no-default-browser-check", "--window-position=160,40",
                               "--window-size=1440,1000", script["url"]],
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        for _ in range(50):
            try:
                page = Page()
                break
            except (OSError, StopIteration):
                time.sleep(0.2)
        subprocess.run(["osascript", "-e", 'tell application "Google Chrome" to activate'], check=False)
        time.sleep(2.5)
        # The recording starts here and is cut to length afterwards, so an
        # action's time below is its time in the video.
        # ffmpeg rather than screencapture -V, which stopped on its own
        # half-way through the first take.
        recorder = subprocess.Popen(
            ["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-f", "avfoundation",
             "-capture_cursor", "1", "-capture_mouse_clicks", "1", "-framerate", "30", "-pixel_format", "nv12",
             "-i", "Capture screen 0:none", "-c:v", "libx264", "-preset", "veryfast", "-pix_fmt", "yuv420p",
             str(HERE / f"{name}.mp4")], stdin=subprocess.PIPE)
        started = time.monotonic()
        time.sleep(2)
        steps = [f"Go to {script['url']}"]
        timeline = []
        for action, element_id, *value in script["steps"]:
            target = page.find(element_id)
            at = round((time.monotonic() - started) * 1000)
            if action == "click":
                click(target["x"], target["y"])
                steps.append(f"Click {quote(target['label'])}")
            elif action == "type":
                click(target["x"], target["y"])
                time.sleep(0.3)
                type_text(value[0])
                steps.append(f"Type {value[0]} into the {quote(target['label'])} field")
            elif action == "select":
                click(target["x"], target["y"])
                time.sleep(0.6)
                type_text(value[0], per_key=0.05)
                key(RETURN)
                steps.append(f"Select {value[0]} from the {quote(target['label'])} field")
            timeline.append({"step": steps[-1], "at_ms": at,
                             "end_ms": round((time.monotonic() - started) * 1000)})
            time.sleep(1.2)
        time.sleep(2)
        recorder.communicate(b"q", timeout=30)
    finally:
        chrome.terminate()
    (HERE / f"{name}.expected.flow.yaml").write_text(
        f"# Expected draft for the captured {name} recording, written by drive_chrome.py\n"
        f"# from the steps it performed. Regenerate rather than edit it.\n"
        f"name: {quote(script['name'])}\napp: \"web\"\nsteps:\n" + "".join(f"  - {quote(s)}\n" for s in steps))
    (HERE / f"{name}.actions.json").write_text(json.dumps(timeline, indent=2) + "\n")


if __name__ == "__main__":
    main(sys.argv[1])
