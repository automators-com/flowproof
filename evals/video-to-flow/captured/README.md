# Captured recordings

Real screen recordings, made on a Mac with a real screen recorder, for what
the generated corpus cannot show: macOS window chrome, an H.264 encode of a
live screen, a pointer moved by real input events, and native menus. Each
was made by a script driving real input, so its answer key comes from the
script rather than from memory. **The videos are not committed**: they show
whatever else is on the screen they were made on. The scripts, the expected
flows and the action timings are.

| Recording | Made by | What it shows |
|---|---|---|
| `tricentis-truck.mp4` | `drive_chrome.py tricentis-truck` | Chrome on the public Tricentis demo app (no account): the Truck tab, ten vehicle fields including three native select menus, Next, then two fields of the next page. 51s, 1920x1080. |
| `tricentis-truck-teams.mp4` | the line below | The same recording at 1280px, 10 fps and about 50 kbps, the way a Teams screen share smears text. |
| `flowproof-app.mp4` | `drive_app.py` | The Flowproof desktop app's own navigation: Runs, Environments, Flows, a search, Settings. Nothing is created or changed. 20s, 1920x1080. |

```bash
cd evals/video-to-flow/captured
uv run --no-project --with pyobjc-framework-Quartz --with websocket-client python drive_chrome.py tricentis-truck
ffmpeg -i tricentis-truck.mp4 -vf "scale=1280:-2,fps=10" -c:v libx264 -b:v 80k -maxrate 100k \
  -bufsize 200k -pix_fmt yuv420p tricentis-truck-teams.mp4   # answer key and timings: copies of the original's
uv run --no-project --with pyobjc-framework-Quartz --with websocket-client python drive_app.py
python3 prepare.py                                            # one scoreable case folder per recording
python3 ../../../scripts/video-draft-eval.py score --cases-dir .   # needs a model key
```

The drive scripts post real mouse and key events through Quartz and record
the main display with ffmpeg's avfoundation input, pointer and clicks
shown, so the terminal needs Screen Recording and Accessibility permission.
They take over the screen while they run. `drive_app.py` clicks by position
in a window it places itself, because the app's web view shows nothing to
macOS's accessibility tree; a change to the app's layout means new
positions.

## Frame selection on the captured set

`--frames-out` over each, scored like layer A (2026-11-24, engine at plan
16's layer B):

| Recording | Action recall | Frames | Kept area |
|---|---|---|---|
| `tricentis-truck` | 0.79 | 24/60 | trims 110px of 1280 on the left |
| `tricentis-truck-teams` | 0.93 | 31/60 | trims 10px on the left |
| `flowproof-app` | 1.00 | 7/60 | trims 90px on the left |

Real recordings keep far more of their actions than the generated corpus
(0.12 to 0.67): a real recorder's click highlight and its encoding noise
cross `CHANGE` where a clean screenshot of a few typed characters does not.
So the generated corpus is the harsher test of frame selection, and its
missed-typing finding is partly a property of its clean pictures. The trim
still cuts into each window from the left. Read these beside the generated
scoreboards, never as one set with them.

What nobody here can record is a real SAP GUI or Fiori session, which needs
the private system and its credentials. That one stays with the person who
has them, and belongs in the hand-scored holdout, outside this repository.
