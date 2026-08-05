# Run recording

Every run leaves visual evidence behind: redacted, step-synchronized frames
you can open beside the verdict, and — on request — a GIF of the whole
execution. This page is the reference for controlling that, followed by the
design record for why the pipeline is shaped the way it is.

## Controlling what a run records

Capture density and GIF assembly are execution-time choices, available on both
`record` and `run`:

| Flag | Default | What it does |
| --- | --- | --- |
| `--recording-detail full` | yes | A frame before each step, after each step, and on failure. |
| `--recording-detail low` | | Initial state, every fifth completed step, and final state. |
| `--recording-detail off` | | No screenshots and no GIF. |
| `--video` | off | Assemble the captured frames into a `recording.gif`. |
| `--highlight-cursor` | off | Draw a visible cursor and a bright click halo into the frames. |

```bash
# Default: keep screenshots and skip GIF assembly.
flowproof run checkout.flow.yaml

# Opt in to GIF/video assembly.
flowproof run checkout.flow.yaml --video

# Initial state, every fifth completed step, and final state.
flowproof run checkout.flow.yaml --recording-detail low

# Add a visible cursor and bright click halo at every pointer action.
flowproof run checkout.flow.yaml --highlight-cursor

# Fastest path: no screenshots and no GIF.
flowproof run checkout.flow.yaml --recording-detail off
```

`--recording-detail full` remains the screenshot-density default, while GIF
assembly is disabled unless `--video` is present. `low` deliberately lets
several steps share one visual checkpoint, producing a less fluent but much
cheaper review artifact. `off` changes artifacts only: the same actions,
assertions, redaction-independent safety checks, and verdict still execute.
`--video` is independent of detail, so `low --video` creates a sparse
animation. `--highlight-cursor` adds a pre-action checkpoint for
click, right-click, double-click, and hover actions; drag actions mark both
ends. The bright halo appears on the event checkpoint, while later frames
retain the cursor at its last known position. Because the cursor is rendered
after redaction, it cannot reveal pixels hidden by a mask. Pointer checkpoints
are retained in low-detail mode so important clicks are not lost.

The SDK and the MCP tools take the same three controls with the same
defaults, so an agent does not have to shell out to the CLI to choose them:

```python
from flowproof import Flow

flow = Flow("checkout.flow.yaml")
flow.record(recording_detail="low")
flow.run(video=True, highlight_cursor=True)
```

## Reviewing what it captured

`report.html` (generated from `result.json`) carries a step-synchronized
viewer: the step table is clickable, showing that step's frames
(before/after, failure frame highlighted). Self-contained — frames are
referenced relatively from the bundle, with no external resources. This is
the "jump to the assert step" experience, driven entirely by the structured
timeline, never by scrubbing.

Two things worth knowing before you read a bundle, both detailed below:
frames are named by capture offset and content hash rather than step number
(*Artifact bundle layout*), and masks are applied in memory before any PNG is
encoded, so there is no unredacted intermediate to clean up (*Redaction*).

## Design notes

The rest of this page is the design record: **approved and implemented** (v1
keyframe filmstrip; continuous capture sources remain follow-ups per §3).

### 1. Why and what

flowproof's primary human interaction is oversight: reviewing and approving
agent-authored and agent-healed tests. The visual recording of an execution
is therefore a **first-class review surface**, not a debug artifact. The bar:

> A reviewer can watch exactly what a test did, jump to any step directly,
> and never see data that should have been masked.

Principles this design enforces:

- **One source of truth.** Step→time mappings live inside the artifact that
  already describes the steps of that execution — the trace for the
  recording (authoring) run, `result.json` for replay runs. No side-channel
  timing files that can drift.
- **Structured data is the machine surface; video is the human surface.**
  Agents reason over JSON and (later) scene-graph snapshots. Nothing
  programmatic ever parses pixels. The visual timeline is *generated from*
  the same run data as an additional view.
- **Redaction is part of the capture path.** Frames are masked *before they
  are persisted*, by the same rule set that will govern stored screenshots.
  There is no unredacted intermediate on disk, and no post-processing step
  that could be skipped.

### 2. The execution timeline (shared by record and replay)

Both `record` and `run` execute the same step loop against an `AppDriver`.
This design introduces one shared component, the **RunRecorder**, that both
loops drive:

```
step loop ──▶ RunRecorder
                 ├─ FrameSource  (where pixels come from)
                 ├─ Redactor     (masks applied in-memory, pre-persist)
                 └─ Bundle       (frames on disk + timeline entries)
```

Per step, the executor tells the RunRecorder `step_started(id)` /
`step_finished(id)`; the RunRecorder captures and timestamps frames and
produces a `Timeline`: for each step, `{start_ms, end_ms}` offsets from the
execution start, plus the persisted frame offsets falling in that range.
Timestamps are captured once, by the RunRecorder — the executor and the
recorder cannot disagree, because the executor doesn't keep its own clock.

### 3. Capture pipeline

**`FrameSource` abstraction** (in `flowproof-driver`): produces timestamped
raw frames. Two implementations planned; both feed the identical
redact→persist path, so upgrading capture never touches sync or redaction:

- **v1 — keyframe source** (shipped): captures a full frame *before each
  step*, *after each step*, and *on failure*, via the driver:
  - Web: `Tab::capture_screenshot` (already available in headless_chrome).
  - Windows: GDI `BitBlt` screen grab behind the existing `Capture` trait
    (deliberately simple; correctness over frame rate).
  Keyframes make step sync *exact by construction* and keep the bundle
  small. The visual result is a step-synchronized filmstrip, not 30fps
  video — an accepted v1 tradeoff, stated in the artifact format so
  consumers can distinguish it. With `--video`, the bundle also carries a
  ready-to-play `recording.gif`: the keyframes as one animation, each frame
  shown for the real gap to the next (clamped to stay watchable), so a whole
  run reviews like a video without continuous capture.
- **Later — continuous source** (follow-up PRs): DXGI desktop duplication on
  Windows, CDP screencast for web, feeding the same sink at N fps and
  assembled into WebM. The bundle format below already carries a `format`
  discriminator (`filmstrip/1` now, `webm/1` later) so this lands without
  schema changes.

The user-facing controls over this pipeline are documented under *Controlling
what a run records*, above.

### 4. Artifact bundle layout

Each execution's bundle is self-contained (stateless; safe for future
parallel runs):

```
.flowproof/runs/<run-id>/            # replay bundle (exists today)
  result.json                        # + recording + per-step timing (§6)
  report.html                        # + step-synchronized viewer
  recording/
    frame-<offset_ms>-<sha256-8>.png # redacted BEFORE write; content-named

.flowproof/recordings/<trace_id>/    # authoring bundle, referenced by trace
  recording/frame-...png             # same layout, same code path
```

Frame files are named by capture offset + content hash, so the mapping from
timeline entry → file is derivable from the structured data alone and files
are tamper-evident. There is deliberately **no** `timeline.json` in the
bundle: the timeline lives in `result.json` / the trace (§5–6).

### 5. Trace schema changes (additive, optional, v1-compatible)

The trace carries the *authoring* execution's recording, so reviewing an
agent-authored test needs only the trace + its bundle:

- **Header** gains optional `recording`:
  ```json
  "recording": { "format": "filmstrip/1", "dir": ".flowproof/recordings/<trace_id>", "started_at": "..." }
  ```
- **Step `artifacts`** gains optional `recording`:
  ```json
  "artifacts": { "recording": { "start_ms": 1200, "end_ms": 1730 } }
  ```
  (existing `pre_screenshot`/`post_screenshot` hashes are unchanged and will
  point at the same content-addressed frames once stills land).
- **Header** gains optional `redaction` (§7) — recorded into the trace so
  every future replay redacts identically **without needing the spec**:
  ```json
  "redaction": [ {"target": {"css": "#ssn"}, "mode": "mask"} ]
  ```

Rationale for putting timing in the trace rather than a sidecar: the trace
is already the single reviewed, diffed, healed artifact; its schema is the
one place a step and its evidence can't drift apart.

### 6. Run report changes (replay executions)

- `StepResult` gains `started_ms` (offset from run start; with the existing
  `duration_ms` this *is* the step→time mapping — no new sidecar).
- `RunReport` gains optional `recording { format, dir }`.
- Python `RunResult` mirrors both; MCP/CLI `--json` inherit automatically.

### 7. Redaction (the shared layer for every persisted pixel)

Redaction is the single implementation for **all** persisted pixels (video
frames now, trace screenshots when they land):

- **Rules** (`flowproof-driver::redact`): `{target, mode}` where `target` is
  a selector (css / automation_id) or a fixed rect, `mode: mask` (solid
  fill). Declared in the spec under `redact:`, copied into the trace header
  at record time (§5).
- **Automatic, non-optional rule**: password fields are always masked —
  web `input[type=password]`, UIA `IsPassword` elements — regardless of
  spec. Not configurable off.
- **Application point**: the RunRecorder resolves rule targets to screen
  rects via the driver *at capture time* (elements move; rects are resolved
  per frame batch, not once) and fills them in the in-memory frame before
  the PNG is encoded. No unmasked bytes ever reach disk.
- **Fail closed**: if a rule's target cannot be resolved while the element
  is known to be on screen, the affected frames are dropped (not persisted
  unmasked) and the timeline entry records `frames_dropped: "redaction"`.

### 8. Healing diff seam (built: `<name>.heal.html`)

Heal already produces a proposed trace whose steps are diffed against the
original by position with per-field changes. Because *every* authoring
execution gets its own self-contained bundle keyed by `trace_id`, a healed
trace that is re-authored gets a new `trace_id` + new bundle while the
original keeps its own. A before/after review view is then pure composition:
for each changed step id, show `old trace bundle[step range]` beside
`new trace bundle[step range]` — both sides already exist with exact
step-time mappings. Nothing in this design (content-addressed frames,
per-trace bundles, step-keyed ranges) needed rework for that; heal now
renders exactly this composition as a self-contained `<name>.heal.html`
next to the trace (path surfaced as `diff_html` in the heal report).

### 9. Testing

- **Sync correctness**: mock `FrameSource` emitting deterministic frames;
  assert every step's `[start_ms, end_ms]` brackets exactly its frames and
  ranges are monotonic and non-overlapping.
- **Redaction proof**: capture against the web greeter page extended with a
  password field + a css-masked region; decode the *persisted* PNGs and
  assert the masked rects are uniform fill (and the same test passes through
  the real browser path in the ubuntu live-app CI job). A second test proves the
  fail-closed path drops frames when a rule can't resolve.
- **Schema conformance**: extended fixture trace with `recording` +
  `redaction` blocks validates against the updated JSON Schema; round-trip
  stability as usual.

### 10. Not built

Playback UI beyond the report viewer; continuous-capture sources
(DXGI/screencast) and video-file assembly; any agent-facing video parsing
(never planned). The healing diff view was scoped here as a seam only; it has
since been built, and §8 describes what shipped.
