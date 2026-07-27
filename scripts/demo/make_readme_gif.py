#!/usr/bin/env python3
"""Regenerate the README's demo GIF (`docs/assets/flowproof-demo.gif`).

The GIF is a RECORDING OF A REAL RUN, not a mock-up. This script builds the
CLI, stands up a local OpenAI-compatible upstream (`fake_model.py`) so
`flowproof record` needs no API key and no network, captures the real stdout of

    cat scripts/demo/order-status.flow.yaml
    flowproof record scripts/demo/order-status.flow.yaml
    flowproof run scripts/demo/order-status.flow.yaml

and renders those exact bytes as a terminal animation. Nothing in the frames is
typed by hand: if the CLI's output changes, re-running this changes the GIF.

    python3 scripts/demo/make_readme_gif.py            # capture + render
    python3 scripts/demo/make_readme_gif.py --no-build  # reuse target/release

Needs `pillow` and the `openai` SDK (the agent under test uses the real client).
"""
from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

DEMO_DIR = Path(__file__).resolve().parent
REPO = DEMO_DIR.parent.parent
# Every demo command runs from the repository root, so what the GIF shows is
# copy-pasteable for a reader who just cloned.
SPEC = "scripts/demo/order-status.flow.yaml"
TRACE = "scripts/demo/order-status.trace.jsonl"
OUT_GIF = REPO / "docs" / "assets" / "flowproof-demo.gif"

# GitHub's dark palette, so the GIF sits in a dark README without clashing.
BG = (13, 17, 23)
CHROME = (22, 27, 34)
BORDER = (48, 54, 61)
LIGHTS = ((255, 96, 87), (255, 190, 46), (39, 202, 63))
FG = (201, 209, 217)
DIM = (110, 118, 129)
GREEN = (63, 185, 80)
BRIGHT_GREEN = (126, 231, 135)
BLUE = (121, 192, 255)
STRING = (165, 214, 255)
ORANGE = (240, 136, 62)

WIDTH = 980           # wide enough that no real output line has to wrap
FONT_SIZE = 15
LINE_H = 21
PAD_X = 20
PAD_TOP = 44          # title bar
PAD_BOTTOM = 14
TITLE = "flowproof — agent test: record once, replay with no model"

FONT_PATHS = [
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf",
    "/Library/Fonts/DejaVuSansMono.ttf",
]
BOLD_PATHS = [
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationMono-Bold.ttf",
]


def load_font(paths, size):
    for p in paths:
        if Path(p).exists():
            return ImageFont.truetype(p, size)
    raise SystemExit(
        "no monospace TTF found; install fonts-dejavu-core or edit FONT_PATHS"
    )


# ---------------------------------------------------------------- capture


def build_cli() -> Path:
    subprocess.run(
        ["cargo", "build", "--release", "-p", "flowproof-cli"], cwd=REPO, check=True
    )
    return REPO / "target" / "release" / "flowproof"


def start_fake_model() -> tuple[subprocess.Popen, str]:
    proc = subprocess.Popen(
        [sys.executable, str(DEMO_DIR / "fake_model.py")],
        stdout=subprocess.PIPE,
        text=True,
    )
    url = proc.stdout.readline().strip()
    if not url.startswith("http"):
        raise SystemExit(f"fake model did not start: {url!r}")
    return proc, url


def run(cmd: list[str], env: dict, label: str) -> tuple[str, float]:
    """Run a demo command from the repository root, returning (output, seconds).

    stderr is merged into stdout by the OS rather than concatenated after it, so
    the captured lines are in the order a terminal would show them.
    """
    started = time.monotonic()
    proc = subprocess.run(
        cmd,
        cwd=REPO,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    elapsed = time.monotonic() - started
    out = proc.stdout.rstrip("\n")
    if proc.returncode != 0:
        raise SystemExit(f"{label} failed ({proc.returncode}):\n{out}")
    return out, elapsed


def capture(binary: Path) -> list[dict]:
    """Run the three demo commands for real and return the session transcript."""
    for stale in (REPO / TRACE, DEMO_DIR / ".flowproof"):
        shutil.rmtree(stale) if stale.is_dir() else stale.unlink(missing_ok=True)

    base_env = {k: v for k, v in os.environ.items() if not k.startswith("OPENAI_")}
    base_env.pop("FLOWPROOF_AGENT_UPSTREAM", None)

    spec_text = (REPO / SPEC).read_text().rstrip("\n")
    session = [{"cmd": f"cat {SPEC}", "out": spec_text, "kind": "yaml"}]

    # RECORD against the local upstream: a real model call over a real proxy.
    model, url = start_fake_model()
    try:
        out, secs = run(
            [str(binary), "record", SPEC],
            {**base_env, "FLOWPROOF_AGENT_UPSTREAM": url},
            "record",
        )
    finally:
        model.terminate()
        model.wait(timeout=10)
    session.append(
        {"cmd": f"flowproof record {SPEC}", "out": out, "kind": "cli", "secs": secs}
    )

    # REPLAY with the upstream gone: any stray model call would fail loudly.
    out, secs = run([str(binary), "run", SPEC], base_env, "run")
    session.append(
        {"cmd": f"flowproof run {SPEC}", "out": out, "kind": "cli", "secs": secs}
    )
    return session


# ---------------------------------------------------------------- colouring


def split_comment(line: str) -> tuple[str, str]:
    """Split a YAML line into (code, trailing comment), either part possibly "".

    YAML opens a comment at a `#` that starts the line or follows whitespace,
    and never inside a quoted scalar. The comment keeps the run of spaces that
    precedes it, so the code half still ends at its original column.
    """
    quote = ""
    for i, ch in enumerate(line):
        if quote:
            if ch == quote:
                quote = ""
        elif ch in "\"'":
            quote = ch
        elif ch == "#" and (i == 0 or line[i - 1] in " \t"):
            return line[:i], line[i:]
    return line, ""


def yaml_spans(line: str) -> list[tuple[str, tuple]]:
    """Colour a YAML line: comments dim, keys blue, values light, asserts orange.

    A trailing comment is dimmed like a whole-line one. Without splitting it off
    first it lands in the value span and renders as part of the value - and one
    containing a colon (`# declared: ...`) would even partition as the key.
    """
    code, comment = split_comment(line)
    tail = [(comment, DIM)] if comment else []
    stripped = code.lstrip()
    if not stripped:
        return [(code, DIM)] + tail
    indent = code[: len(code) - len(stripped)]
    body = stripped
    prefix = ""
    if body.startswith("- "):
        prefix, body = "- ", body[2:]
    if ":" in body:
        key, _, value = body.partition(":")
        key_colour = ORANGE if key.startswith(("assert", "prompt")) else BLUE
        spans = [(indent + prefix, DIM), (key + ":", key_colour)]
        if value:
            spans.append((value, STRING))
        return spans + tail
    return [(indent + prefix, DIM), (body, FG)] + tail


def cli_spans(line: str) -> list[tuple[str, tuple]]:
    """Colour a CLI output line: verdict green, the containment tier dim."""
    if line.startswith("PASS"):
        return [(line, BRIGHT_GREEN)]
    if line.startswith(("FAIL", "ERROR")):
        return [(line, LIGHTS[0])]
    if line.startswith("egress containment:"):
        head, _, tail = line.partition(": ")
        return [(head + ": ", DIM), (tail, GREEN if "enforced" in tail else DIM)]
    if line.startswith("Recorded"):
        return [(line, FG)]
    return [(line, DIM)]


def cmd_spans(cmd: str) -> list[tuple[str, tuple]]:
    """Colour a prompt line: `$` green, argv[0] bold-ish, args light."""
    head, _, rest = cmd.partition(" ")
    spans = [("$ ", BRIGHT_GREEN), (head, FG)]
    if rest:
        spans.append((" " + rest, STRING))
    return spans


# ---------------------------------------------------------------- rendering


def wrap(spans: list[tuple[str, tuple]], cols: int) -> list[list[tuple[str, tuple]]]:
    """Soft-wrap a coloured line to `cols` characters, as a terminal does.

    Continuation rows are indented two spaces so a wrapped line still reads as
    one line. Colours survive a break in the middle of a span.
    """
    rows: list[list[tuple[str, tuple]]] = [[]]
    used = 0
    for text, colour in spans:
        while text:
            room = cols - used - (2 if rows[-1] and used == 0 else 0)
            if room <= 0:
                rows.append([("  ", DIM)])
                used = 2
                continue
            head, text = text[:room], text[room:]
            rows[-1].append((head, colour))
            used += len(head)
            if text:
                rows.append([("  ", DIM)])
                used = 2
    return rows


def to_lines(session: list[dict], cols: int) -> list[dict]:
    """Flatten the session into the terminal's line stream, tagged for reveal.

    Each entry is {spans, typed}: a `typed` line animates character by character
    (a command being entered), output rows appear whole, as a terminal does.
    """
    lines: list[dict] = []
    for group, step in enumerate(session):
        if group:
            lines.append({"spans": [], "typed": False})
        lines.append({"spans": cmd_spans(step["cmd"]), "typed": True})
        colour = yaml_spans if step["kind"] == "yaml" else cli_spans
        for raw in step["out"].split("\n"):
            for row in wrap(colour(raw), cols):
                lines.append({"spans": row, "typed": False})
    return lines


def draw_frame(font, bold, lines, reveal: int, partial: str | None, size: tuple[int, int]):
    """One frame: the first `reveal` rows, plus `partial` as a half-typed line."""
    img = Image.new("RGB", size, BG)
    d = ImageDraw.Draw(img)

    # Window chrome: title bar, traffic lights, centred title.
    d.rectangle([0, 0, size[0], PAD_TOP - 12], fill=CHROME)
    d.line([(0, PAD_TOP - 12), (size[0], PAD_TOP - 12)], fill=BORDER)
    for i, colour in enumerate(LIGHTS):
        cx = 20 + i * 20
        d.ellipse([cx - 6, 10, cx + 6, 22], fill=colour)
    d.text(
        ((size[0] - d.textlength(TITLE, font=font)) / 2, 8), TITLE, font=font, fill=DIM
    )

    y = PAD_TOP
    for line in lines[:reveal]:
        x = PAD_X
        for text, colour in line["spans"]:
            f = bold if colour is BRIGHT_GREEN else font
            d.text((x, y), text, font=f, fill=colour)
            x += d.textlength(text, font=f)
        y += LINE_H
    if partial is not None:
        x = PAD_X
        for text, colour in cmd_spans(partial):
            d.text((x, y), text, font=font, fill=colour)
            x += d.textlength(text, font=font)
        d.rectangle([x + 1, y + 3, x + 9, y + FONT_SIZE + 3], fill=FG)
    return img


def render(session: list[dict]) -> None:
    font = load_font(FONT_PATHS, FONT_SIZE)
    bold = load_font(BOLD_PATHS, FONT_SIZE)
    advance = ImageDraw.Draw(Image.new("RGB", (1, 1))).textlength("x" * 100, font=font) / 100
    cols = int((WIDTH - 2 * PAD_X) / advance)
    lines = to_lines(session, cols)
    # Size the canvas to the content: nothing scrolls out of view.
    size = (WIDTH, PAD_TOP + len(lines) * LINE_H + PAD_BOTTOM)

    frames: list[Image.Image] = []
    durations: list[int] = []

    def add(img, ms):
        frames.append(img)
        durations.append(ms)

    revealed = 0
    for idx, line in enumerate(lines):
        if line["typed"]:
            # Type the command out, a few characters per frame.
            text = "".join(t for t, _ in line["spans"])[2:]  # strip the "$ "
            for cut in range(0, len(text) + 1, 3):
                add(draw_frame(font, bold, lines, revealed, text[:cut], size), 45)
            add(draw_frame(font, bold, lines, revealed, text, size), 400)
            revealed = idx + 1
            add(draw_frame(font, bold, lines, revealed, None, size), 260)
        else:
            revealed = idx + 1
            # Output lands as a block; the verdict gets a beat of its own.
            flat = "".join(t for t, _ in line["spans"])
            hold = 1100 if flat.startswith(("PASS", "Recorded")) else 80
            add(draw_frame(font, bold, lines, revealed, None, size), hold)
    add(frames[-1].copy(), 3400)  # rest on the finished screen before looping

    OUT_GIF.parent.mkdir(parents=True, exist_ok=True)
    # One shared palette for every frame, so the encoder can emit each frame as
    # a diff of the one before it (the typing frames differ by a few characters).
    # The palette is derived from the finished screen, with the exact UI colours
    # painted in as large swatches first: the traffic lights are a dozen pixels
    # each and would otherwise lose their slots to text antialiasing blends.
    source = frames[-1].copy()
    swatch = ImageDraw.Draw(source)
    for i, colour in enumerate((*LIGHTS, FG, DIM, GREEN, BRIGHT_GREEN, BLUE, STRING, ORANGE)):
        swatch.rectangle([i * 40, 0, i * 40 + 39, 30], fill=colour)
    palette = source.quantize(colors=64, method=Image.MEDIANCUT)
    quantized = [f.quantize(palette=palette, dither=Image.NONE) for f in frames]
    quantized[0].save(
        OUT_GIF,
        save_all=True,
        append_images=quantized[1:],
        duration=durations,
        loop=0,
        optimize=True,
    )
    kb = OUT_GIF.stat().st_size / 1024
    print(f"{OUT_GIF.relative_to(REPO)}: {len(frames)} frames, {kb:.0f} KiB")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--no-build", action="store_true", help="reuse target/release")
    args = ap.parse_args()

    binary = REPO / "target" / "release" / "flowproof"
    if not args.no_build or not binary.exists():
        binary = build_cli()

    session = capture(binary)
    for step in session:
        secs = step.get("secs")
        print(f"$ {step['cmd']}" + (f"   [{secs:.2f}s]" if secs else ""))
        print("\n".join("  " + l for l in step["out"].split("\n")))
    render(session)


if __name__ == "__main__":
    main()
