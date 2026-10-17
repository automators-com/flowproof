# Where `app: vision` falls short of a computer-use model, and why

Claude computer use, OpenAI's CUA (Operator) and Gemini computer use (Google's
Project Mariner line) all drive a screen the same way: a screenshot goes to the
model, the model answers with a point (pixels, or Gemini's 0-1000 grid), the
client clicks it, and a fresh screenshot goes back. Nothing is matched by name.
The model *sees* which thing is meant.

flowproof cannot do that on replay, because replay makes zero model calls. A
step's target has to be found again from the pixels alone, every run. So the
question for each miss in `vision_bench` is the same: what does the model use
that a replayed step does not carry?

| Miss on the benchmark | What a computer-use model uses | What vision has | Status |
|---|---|---|---|
| The 2nd "Home" (breadcrumb vs. nav pane vs. "Search Home") | Where each one is: "the Home in the left pane" | An ordinal, `the 2nd "Home"`, but the recording scene offered every "Home" as the same `text:Home`, so whichever one the model picked was recorded as the first | **Closed**: the scene names each repeat by ordinal, `nth:2:text:Home`, counted the way replay counts |
| Icon-only buttons (Paint's tools, Save, Copilot) | It recognises the icon: a floppy disk means Save. OmniParser does this as a detector plus a captioning model | A detector and a 64-bit pixel hash (`icon:<hash>`). The recording model picks the icon from the screenshot, and replay finds it by hash | Works for recorded flows. A hand-written `Click "Save"` on an icon still can't be resolved without a model |
| Glyph keys: ⌫, √x, x², ., ± | It reads the glyph | Stroke detection for − = ÷ + × only | Open, small |
| Placeholder text taken for a label ("Search Home") | Grey hint text inside a box reads as a hint | Text is text | Open: detecting low-contrast text would separate the two |
| A click that went nowhere goes unnoticed | A screenshot after every action, and a retry if nothing happened | Before each click, the pointer rests on the target (hover tint drawn) and the frame is kept; for 1.5s after it, anything beyond a caret changing counts as an answer | **Closed** for a click nothing answered: it is made once more, then the step goes on, logged, since a click can rightly change nothing. A click that lands on the *wrong* thing still changes the screen, and is caught by the next step or assertion |
| Small text | `zoom` on a region at full resolution | OCR again at 2x and 3x | At parity |
| No usable anchor at all | Raw coordinates | `click_at` with `x_pct`/`y_pct` | At parity |

The benchmark no longer scores a window's own chrome (System menu,
Minimize, Maximize, Close), which no step means and vision refuses on
purpose. It still asks for every icon-only control by its accessible name,
which pixels never spell, so most of its name-accuracy misses are icons by
construction; how far vision reaches icons shows in coverage instead.

## Not done

Claude's computer-use docs say putting the instruction text *before* the
screenshot improves click accuracy. The recorder sends the screenshot first
(`flowproof-agent/src/llm.rs`), following Anthropic's general advice for
images. The click-accuracy advice is about a model that outputs coordinates;
ours picks a listed token, so the effect is unknown. Changing the order
should wait until a recording eval can measure it.
