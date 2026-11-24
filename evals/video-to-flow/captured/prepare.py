"""Turns each captured recording into a case folder video-draft-eval.py can
score (`score --cases-dir evals/video-to-flow/captured`): the frames
`--frames-out` chooses, and the case, actions and expected flow beside them.

    python3 prepare.py          (uses target/debug/flowproof: cargo build -p flowproof-cli)
"""
import json
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ENGINE = HERE.parents[2] / "target/debug/flowproof"
CASES = {
    "tricentis-truck": ("web", "https://sampleapp.tricentis.com/101",
                        "A real screen recorder: macOS window chrome, a real pointer path and native select menus."),
    "tricentis-truck-teams": ("web", "https://sampleapp.tricentis.com/101",
                              "The same recording squeezed to about 50 kbps at 10 fps, the way a Teams share smears text."),
    "flowproof-app": ("flowproof-desktop", None,
                      "A desktop app rather than a web page: the Flowproof app's own navigation."),
}

if not ENGINE.is_file():
    sys.exit(f"build the engine first: {ENGINE} is not there")

for name, (app, url, catches) in CASES.items():
    video = HERE / f"{name}.mp4"
    if not video.exists():
        print(f"{name}: no recording, skipped")
        continue
    case = HERE / name
    shutil.rmtree(case, ignore_errors=True)
    subprocess.run([str(ENGINE), "author-from-video", str(video), "--frames-out", str(case / "frames")],
                   check=True, stderr=subprocess.DEVNULL)
    shutil.copy2(HERE / f"{name}.expected.flow.yaml", case / "expected.flow.yaml")
    shutil.copy2(HERE / f"{name}.actions.json", case / "actions.json")
    manifest = json.loads((case / "frames/manifest.json").read_text())
    expected = (case / "expected.flow.yaml").read_text()
    title = next(line.split(":", 1)[1].strip().strip('"') for line in expected.splitlines() if line.startswith("name:"))
    (case / "case.json").write_text(json.dumps({
        "id": name, "name": title, "catches": catches, "app": app, "start_url": url,
        "duration_ms": manifest["duration_ms"],
        "expect": {"not_app": False, "out_of_scope": 0, "masked_literals": []},
    }, indent=2) + "\n")
    print(f"{name}: {len(manifest['frames'])} frames")
