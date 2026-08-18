//! Browser adapter: drives a page in headless Chromium over the DevTools
//! protocol, implementing the same [`AppDriver`] surface the UIA driver
//! exposes — so the recorder and replayer work unchanged.
//!
//! Selector mapping: `css` payload key, else `#<automation_id>`. `launch`
//! interprets `command` as the URL to open. The Chromium binary is found via
//! the `CHROME` env var or platform auto-detection.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use flowproof_driver::{
    AppDriver, DriverError, KeyMod, PixelRect, ScrollTo, UiaSelector, WebSession,
};
use headless_chrome::browser::tab::{ModifierKey, Tab};
use headless_chrome::protocol::cdp::Target::CreateTarget;
use headless_chrome::protocol::cdp::{Emulation, Input, Network, Page};
use headless_chrome::types::Bounds;
use headless_chrome::{Browser, LaunchOptions};

use crate::AdapterError;

const FIND_TIMEOUT: Duration = Duration::from_secs(5);

/// Read a checkbox-like control's state from an element that may be the
/// control itself OR a wrapper around it. Covers the three shapes real apps
/// use: a native `input[type=checkbox|radio]`, an ARIA widget carrying
/// `aria-checked` (`role=checkbox|radio|switch`), and the MUI pattern of a
/// visually hidden input inside a styled span. Returns null when the target
/// is none of those, so the caller can say so precisely.
const CHECKED_STATE_JS: &str = r#"function() {
    const native = (el) =>
        el && el.tagName === 'INPUT' && (el.type === 'checkbox' || el.type === 'radio')
            ? el : null;
    let control = native(this) || this.querySelector('input[type=checkbox],input[type=radio]');
    if (control) { return !!control.checked; }
    const role = this.getAttribute('role');
    const aria = this.hasAttribute('aria-checked')
        ? this
        : this.querySelector('[role=checkbox],[role=radio],[role=switch]');
    if (aria && aria.hasAttribute('aria-checked')) {
        return aria.getAttribute('aria-checked') === 'true';
    }
    if (role === 'checkbox' || role === 'radio' || role === 'switch') { return false; }
    return null;
}"#;

/// Wrap a browser-driver failure. Transport faults (a dead CDP websocket,
/// a dropped event) are classified apart from app observations: an
/// assertion polling inside its recorded wait budget tolerates the former
/// as a miss, because a call that never reached the page learned nothing
/// about it.
/// The cell resolver, run in the page. Implements Fable's algorithm:
/// column by header text (exact-after-trim, then unique-contains, then a
/// `column_field` fallback), row by the four-branch id/anchor resolution,
/// then tags the winning cell with `data-flowproof-cell`. Returns a status
/// string: `ok`, `no_column`/`no_row`/`no_header`/`no_match`,
/// `ambiguous_row:<n>`, or `dup_header`.
const CELL_HINTS: &str = r#"function(){
  var c = document.querySelector('[data-flowproof-cell]');
  if (!c) return 'null';
  function fieldOf(e){
    if (e.getAttribute){
      var f = e.getAttribute('data-field') || e.getAttribute('col-id');
      if (f) return f;
      var cls = (e.className||'').toString().match(/column-([\w-]+)/);
      if (cls) return cls[1];
    }
    return null;
  }
  var row = c.closest ? c.closest('tr, [role=row]') : null;
  var id = row && row.getAttribute ? (row.getAttribute('id') || row.getAttribute('data-id') || row.getAttribute('row-id')) : null;
  return JSON.stringify({ field: fieldOf(c), id: id });
}"#;

const CELL_RESOLVER: &str = r#"function(COL, ANCHOR, COLFIELD, ROWID, ALSO){
  document.querySelectorAll('[data-flowproof-cell]').forEach(function(e){
    e.removeAttribute('data-flowproof-cell');
  });
  function txt(e){ return (e.textContent||'').trim(); }
  function fieldOf(e){
    if (e.getAttribute){
      var f = e.getAttribute('data-field') || e.getAttribute('col-id');
      if (f) return f;
      var cls = (e.className||'').toString().match(/column-([\w-]+)/);
      if (cls) return cls[1];
    }
    return null;
  }
  function idOf(e){
    return (e.getAttribute && (e.getAttribute('id') || e.getAttribute('data-id') || e.getAttribute('row-id'))) || null;
  }
  // Every cell of a row, header cells INCLUDED. Counting `td` alone on the
  // data side and `th` alone on the header side silently misaligns the two
  // whenever a row mixes them.
  function cellsIn(r){
    return r.querySelectorAll('td, th, [role=gridcell], [role=cell], [role=columnheader], [role=rowheader]');
  }
  var tables = document.querySelectorAll('table, [role=grid], [role=table], [role=treegrid]');
  var sawHeader = false;
  for (var t=0; t<tables.length; t++){
    var table = tables[t];
    var headers = table.querySelectorAll('th, [role=columnheader]');
    if (!headers.length) continue;
    sawHeader = true;
    var exact=[], part=[], byField=-1;
    for (var i=0;i<headers.length;i++){
      var h = txt(headers[i]);
      if (h === COL) exact.push(i);
      else if (h.indexOf(COL) !== -1) part.push(i);
      if (COLFIELD && fieldOf(headers[i]) === COLFIELD) byField = i;
    }
    if (exact.length > 1) return 'dup_header';
    var colIdx = exact.length===1 ? exact[0] : (part.length===1 ? part[0] : byField);
    if (colIdx < 0) continue;
    // A column's POSITION is its header's index inside the header's OWN row,
    // not its index among the table's header elements. A schedule-style grid
    // opens its header row with a plain stub above the row-label column
    // (`<tr><td></td><th>Monday</th>…`), so the Nth `th` sits over the N+1th
    // cell of every data row. Indexing `th`s against `td`s reads one column
    // to the left - and returns a real cell, which passes as confidently as
    // the right one. Both sides count th+td together, so they line up.
    var hdr = headers[colIdx];
    var hdrRow = hdr.closest ? hdr.closest('tr, [role=row]') : null;
    var colPos = colIdx;
    if (hdrRow){
      var hcells = cellsIn(hdrRow);
      for (var k=0;k<hcells.length;k++){
        if (hcells[k] === hdr) { colPos = k; break; }
      }
    }
    var rows = [];
    table.querySelectorAll('tr, [role=row]').forEach(function(r){
      if (r.querySelectorAll('td, [role=gridcell], [role=cell]').length) rows.push(r);
    });
    if (!rows.length) continue;
    var idRow = ROWID ? rows.find(function(r){ return idOf(r) === ROWID; }) : null;
    // EVERY anchor must be in the SAME row. One column is often not
    // unique - two people called John, two called Doe - and requiring the
    // conjunction is how a row is named without falling back to position.
    var anchorRows = rows.filter(function(r){
      var t = txt(r);
      if (t.indexOf(ANCHOR) === -1) return false;
      for (var a = 0; a < (ALSO || []).length; a++) {
        if (t.indexOf(ALSO[a]) === -1) return false;
      }
      return true;
    });
    var chosen = null;
    if (idRow){
      if (txt(idRow).indexOf(ANCHOR) !== -1) chosen = idRow;
      else if (anchorRows.length === 0) chosen = idRow;
      else if (anchorRows.length === 1) chosen = anchorRows[0];
      else return 'ambiguous_row:'+anchorRows.length;
    } else {
      if (anchorRows.length === 0) continue;
      if (anchorRows.length > 1) return 'ambiguous_row:'+anchorRows.length;
      chosen = anchorRows[0];
    }
    var cells = cellsIn(chosen);
    if (colPos >= cells.length) continue;
    cells[colPos].setAttribute('data-flowproof-cell','1');
    return 'ok';
  }
  return sawHeader ? 'no_match' : 'no_header';
}"#;

/// The rung-2 container list: what the bare word `item` means. CLOSED by
/// design - a heuristic ("the nearest ancestor that looks like a card")
/// resolves differently as the DOM drifts, which is exactly the silent
/// wrong-element class this target exists to remove. Anything else is
/// named explicitly with `css:`.
const ITEM_CONTAINERS: &str = "li, [role=listitem], [role=row], [role=option], [role=article], tr";

/// The container resolver, run in the page. Candidates come from ONE of two
/// rungs (explicit selector, or the closed `item` list); survivors are those
/// whose trimmed subtree text CONTAINS the anchor - the same substring rule
/// the row resolver uses - and a survivor that CONTAINS another survivor is
/// discarded, so the INNERMOST wins. Exactly one must remain. Returns a
/// status: `ok`, `no_match`, `anchor_without_container`, `ambiguous:<n>`,
/// or `bad_container`.
const SCOPE_RESOLVER: &str = r#"function(CONTAINER, ANCHOR, CONTAINERID, ITEMS, ALSO){
  document.querySelectorAll('[data-flowproof-scope]').forEach(function(e){
    e.removeAttribute('data-flowproof-scope');
  });
  function txt(e){ return (e.textContent||'').trim(); }
  function idOf(e){
    if (!e.getAttribute) return null;
    return e.getAttribute('id') || e.getAttribute('data-id')
      || e.getAttribute('data-test') || e.getAttribute('data-testid') || null;
  }
  var selector = null;
  if (CONTAINER === 'item') selector = ITEMS;
  else if (CONTAINER.indexOf('css:') === 0) selector = CONTAINER.slice(4);
  else if (CONTAINER.indexOf('id:') === 0) selector = '#' + CONTAINER.slice(3).replace(/([^\w-])/g, '\\$1');
  if (!selector) return 'bad_container';
  var candidates;
  try { candidates = Array.prototype.slice.call(document.querySelectorAll(selector)); }
  catch (e) { return 'bad_container'; }
  // Every anchor in the SAME container - see the cell resolver.
  var matching = candidates.filter(function(c){
    var t = txt(c);
    if (t.indexOf(ANCHOR) === -1) return false;
    for (var a = 0; a < (ALSO || []).length; a++) {
      if (t.indexOf(ALSO[a]) === -1) return false;
    }
    return true;
  });
  // Innermost wins: drop any survivor that contains another survivor.
  var inner = matching.filter(function(c){
    return !matching.some(function(o){ return o !== c && c !== o && c.contains(o); });
  });
  var idEl = CONTAINERID
    ? candidates.filter(function(c){ return idOf(c) === CONTAINERID; })[0]
    : null;
  var chosen = null;
  if (idEl){
    if (txt(idEl).indexOf(ANCHOR) !== -1) chosen = idEl;
    else if (inner.length === 0) chosen = idEl;
    else if (inner.length === 1) chosen = inner[0];
    else return 'ambiguous:'+inner.length;
  } else {
    if (inner.length > 1) return 'ambiguous:'+inner.length;
    if (inner.length === 0){
      var surface = (document.body && document.body.textContent) || '';
      return surface.indexOf(ANCHOR) !== -1 ? 'anchor_without_container' : 'no_match';
    }
    chosen = inner[0];
  }
  chosen.setAttribute('data-flowproof-scope','1');
  return 'ok';
}"#;

/// Read the container's record-time hint off the tagged element: the first
/// present of id/data-id/data-test/data-testid (the `row_id` analog).
const SCOPE_HINTS: &str = r#"function(){
  var c = document.querySelector('[data-flowproof-scope]');
  if (!c || !c.getAttribute) return 'null';
  var id = c.getAttribute('id') || c.getAttribute('data-id')
    || c.getAttribute('data-test') || c.getAttribute('data-testid') || null;
  return JSON.stringify({ id: id });
}"#;

/// The frame prober, run in the page. It answers THREE distinct states in
/// one round trip, so the caller never has to conflate them:
/// `no_frame:<names>`, `cross_origin`, or `ok:<json>` with the inner
/// target's presence and text. The inner lookup happens INSIDE the frame's
/// own document: there is deliberately no fallback to the main document, so
/// a target that is not in the frame reads as absent rather than silently
/// matching a same-named element on the page outside it.
/// The frame-miss wording lives in the driver crate so record time and
/// replay report the same reason.
fn cross_origin(frame: &str) -> DriverError {
    DriverError::Browser(flowproof_driver::frame_miss(
        frame,
        &flowproof_driver::FrameProbe::CrossOrigin,
    ))
}

/// Act on an element INSIDE a same-origin frame, in ONE round trip.
///
/// Resolve, guard, act and read back happen together deliberately: an
/// iframe can navigate between calls, and a setter write into a detached
/// document succeeds and reads back correctly while touching nothing.
///
/// No coordinates are computed, so the false green the v1 refusal named -
/// an action dispatched at a point resolved against the MAIN document -
/// cannot occur. The other channels each get a guard here instead:
///
/// - the frame must be RENDERED (a `display:none` iframe has a perfectly
///   reachable `contentDocument`, and driving it would be acting on a
///   surface no user can see);
/// - the target must not be `disabled`/`readonly`. This is not theoretical:
///   `input.value = x` succeeds on a disabled control and reads back green,
///   where the trusted keyboard input the main document uses would simply
///   have been ignored;
/// - the value is read BACK from the element after the write, and a
///   mismatch fails.
///
/// Returns a STATUS STRING, never a throw: an exception inside
/// `call_js_fn` does not reach Rust as an `Err`, which has produced a
/// silent green in this adapter before.
const FRAME_ACT: &str = r#"function(FRAME, CSS, ID, TEXT, OP, ARG){
  function nameOf(f){
    return f.getAttribute('title') || f.getAttribute('name') || f.getAttribute('id')
      || f.getAttribute('aria-label') || '';
  }
  var frames = Array.prototype.slice.call(document.querySelectorAll('iframe, frame'));
  var chosen = null;
  if (FRAME.indexOf('css:') === 0){
    try { chosen = document.querySelector(FRAME.slice(4)); } catch (e) { chosen = null; }
    if (chosen && chosen.tagName !== 'IFRAME' && chosen.tagName !== 'FRAME') chosen = null;
  } else {
    var exact = frames.filter(function(f){ return nameOf(f) === FRAME; });
    var loose = frames.filter(function(f){ return nameOf(f).indexOf(FRAME) !== -1; });
    chosen = exact.length ? exact[0] : (loose.length === 1 ? loose[0] : null);
  }
  if (!chosen){
    return 'no_frame:' + JSON.stringify(frames.map(nameOf).filter(function(n){ return n; }));
  }
  // A frame nobody can see is not one an action may drive.
  if (typeof chosen.checkVisibility === 'function' && !chosen.checkVisibility()){
    return 'frame_hidden';
  }
  var doc = null;
  try { doc = chosen.contentDocument; } catch (e) { doc = null; }
  if (!doc) { return 'cross_origin'; }
  var el = null;
  if (CSS) { try { el = doc.querySelector(CSS); } catch (e) { el = null; } }
  else if (ID) { el = doc.getElementById(ID); }
  else if (TEXT) {
    var all = Array.prototype.slice.call(doc.querySelectorAll('*'));
    el = all.filter(function(n){ return (n.textContent||'').trim() === TEXT; })[0] || null;
  }
  if (!el) { return 'no_element'; }
  var win = doc.defaultView;
  if (OP === 'scroll'){
    var target = el;
    // In standards mode `body.scrollTop` is inert; the scrolling element
    // is what actually moves.
    if (target === doc.body && doc.scrollingElement) { target = doc.scrollingElement; }
    if (target.scrollHeight <= target.clientHeight) { return 'not_scrollable'; }
    var max = target.scrollHeight - target.clientHeight;
    if (ARG > max) { return 'clamped:' + max; }
    target.scrollTo({ top: ARG, behavior: 'instant' });
    return 'at:' + target.scrollTop;
  }
  // Value-driving ops from here on.
  if (OP === 'enabled'){
    return (el.disabled === true || el.readOnly === true) ? 'not_enabled' : 'enabled';
  }
  if (el.disabled === true) { return 'disabled'; }
  if (el.readOnly === true) { return 'readonly'; }
  var proto = el.tagName === 'TEXTAREA'
    ? win.HTMLTextAreaElement.prototype : win.HTMLInputElement.prototype;
  var desc = Object.getOwnPropertyDescriptor(proto, 'value');
  var next = OP === 'clear' ? '' : String(ARG);
  if (desc && desc.set) { desc.set.call(el, next); } else { el.value = next; }
  el.dispatchEvent(new win.Event('input', { bubbles: true }));
  el.dispatchEvent(new win.Event('change', { bubbles: true }));
  // Read BACK from the element, so a control that rejected or rewrote the
  // value fails here rather than passing on the strength of the write.
  return el.value === next ? 'ok' : 'took:' + el.value;
}"#;

const FRAME_PROBE: &str = r#"function(FRAME, CSS, ID, TEXT){
  function nameOf(f){
    return f.getAttribute('title') || f.getAttribute('name') || f.getAttribute('id')
      || f.getAttribute('aria-label') || '';
  }
  var frames = Array.prototype.slice.call(document.querySelectorAll('iframe, frame'));
  var chosen = null;
  if (FRAME.indexOf('css:') === 0){
    try { chosen = document.querySelector(FRAME.slice(4)); } catch (e) { chosen = null; }
    if (chosen && chosen.tagName !== 'IFRAME' && chosen.tagName !== 'FRAME') chosen = null;
  } else {
    var exact = frames.filter(function(f){ return nameOf(f) === FRAME; });
    var loose = frames.filter(function(f){ return nameOf(f).indexOf(FRAME) !== -1; });
    chosen = exact.length ? exact[0] : (loose.length === 1 ? loose[0] : null);
  }
  if (!chosen){
    return 'no_frame:' + JSON.stringify(frames.map(nameOf).filter(function(n){ return n; }));
  }
  var doc = null;
  // A cross-origin frame throws OR yields null - both mean walled off.
  try { doc = chosen.contentDocument; } catch (e) { doc = null; }
  if (!doc) return 'cross_origin';
  var el = null;
  if (CSS){ try { el = doc.querySelector(CSS); } catch (e) { el = null; } }
  else if (ID){ try { el = doc.getElementById(ID); } catch (e) { el = null; } }
  else if (TEXT){
    var all = Array.prototype.slice.call(doc.querySelectorAll('*'));
    // Innermost element whose own text is the anchor, mirroring the
    // page-level text rung.
    var hits = all.filter(function(e){
      return (e.textContent || '').indexOf(TEXT) !== -1
        && !Array.prototype.some.call(e.children, function(c){
             return (c.textContent || '').indexOf(TEXT) !== -1; });
    });
    el = hits.length ? hits[0] : null;
    if (!el){
      // Inputs show their anchor as a value/placeholder, not as text.
      var fields = Array.prototype.slice.call(doc.querySelectorAll('input, textarea, select'));
      var f = fields.filter(function(e){
        return (e.placeholder || '') === TEXT || (e.name || '') === TEXT
          || (e.getAttribute('aria-label') || '') === TEXT;
      });
      el = f.length ? f[0] : null;
    }
  }
  if (!el) return 'ok:' + JSON.stringify({ present: false, text: '' });
  var tag = el.tagName;
  var text = (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT')
    ? el.value
    : (el.innerText !== undefined ? el.innerText : (el.textContent || ''));
  return 'ok:' + JSON.stringify({ present: true, text: text });
}"#;

/// The Date-pinning shim (GAP-P), injected before any page script. It reads
/// `at` with the REAL date parser, computes a fixed offset against the real
/// clock ONCE, and overrides `Date` so the page's "now" starts at `at` and
/// advances at real wall rate. `Date.parse`/`Date.UTC` and explicit
/// `new Date(args)` pass through unchanged - only the zero-arg "now" moves.
/// A workers-see-real-time / performance.now-unpinned limitation is
/// documented, not coded around.
fn clock_shim(at: &str) -> String {
    let at = serde_json::Value::from(at);
    format!(
        r#"(function(){{
  var RealDate = Date;
  var target = RealDate.parse({at});
  if (isNaN(target)) return;
  var delta = target - RealDate.now();
  function FakeDate() {{
    if (arguments.length === 0) return new RealDate(RealDate.now() + delta);
    return new (Function.prototype.bind.apply(
      RealDate, [null].concat(Array.prototype.slice.call(arguments))))();
  }}
  FakeDate.prototype = RealDate.prototype;
  FakeDate.now = function() {{ return RealDate.now() + delta; }};
  FakeDate.parse = RealDate.parse;
  FakeDate.UTC = RealDate.UTC;
  try {{ Object.defineProperty(window, 'Date', {{ value: FakeDate, writable: true, configurable: true }}); }}
  catch (e) {{ window.Date = FakeDate; }}
}})();"#
    )
}

/// The randomness-pinning shim, injected before any page script for the
/// same reason the clock's is: a page that has already called `Math.random`
/// cannot be un-randomised afterwards.
///
/// Same argument as the pinned clock, applied to the other source of
/// per-run drift. A page that mints a value from `Math.random` shows
/// something different on every run, so the only honest thing to write
/// against it is another read - and for a value the flow must ENTER rather
/// than compare, there is nothing to read. Pinned, the value is a constant
/// the author can write by hand, and record and replay see the same one.
///
/// mulberry32: tiny, well-distributed, and exactly reproducible from a
/// 32-bit seed. The page keeps getting plausible-looking numbers; it just
/// gets the SAME ones.
///
/// Deliberately narrow, and documented as such: `crypto.getRandomValues` is
/// untouched (it is a security primitive, not a convenience), workers get
/// their own real `Math.random`, and server-side randomness is `mock:`'s
/// job. A shim that quietly covered less than it claimed would be worse
/// than one with a stated edge.
fn random_shim(seed: u32) -> String {
    format!(
        r#"(function(){{
  var a = {seed} >>> 0;
  Math.random = function() {{
    a |= 0; a = (a + 0x6D2B79F5) | 0;
    var t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  }};
  window.__flowproofRandomPinned = true;
}})();"#
    )
}

/// Entries in `dir` that are not still downloading. Chrome writes an
/// in-progress download under a `.crdownload` suffix and renames it to its
/// final name only on completion, so that suffix is the one signal needed
/// to tell "downloading" from "done" — this fork's `Browser` handle exposes
/// no listener for the CDP download-progress events, so the filesystem is
/// the only channel available.
fn list_finished_downloads(dir: &std::path::Path) -> Result<Vec<std::path::PathBuf>, DriverError> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| web_err("reading the downloads directory", e))?;
    Ok(entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) != Some("crdownload"))
        .collect())
}

/// Two size reads, a beat apart, agreeing — the local signal that a file
/// already past the `.crdownload` rename has actually finished being
/// written to disk, not just renamed early.
fn is_size_stable(path: &std::path::Path) -> bool {
    let Ok(before) = std::fs::metadata(path).map(|m| m.len()) else {
        return false;
    };
    std::thread::sleep(Duration::from_millis(100));
    std::fs::metadata(path)
        .map(|m| m.len() == before)
        .unwrap_or(false)
}

fn web_err(context: &str, err: impl std::fmt::Display) -> DriverError {
    let message = format!("{context}: {err}");
    if is_transport_fault(&message) {
        DriverError::Transport(message)
    } else {
        DriverError::Browser(message)
    }
}

/// How long the CDP transport may sit without a BROWSER-level event before
/// headless_chrome reaps its listener thread. Its default is 30 seconds,
/// which is a live grenade for real test flows:
///
/// 1. a flow spends 30+ seconds doing page-level work (typing, polling an
///    auto-waiting assertion) without producing a single browser-level
///    event, so the listener thread times out and exits;
/// 2. the next navigation fires `TargetInfoChanged` - a browser-level
///    event - and the transport cannot deliver it to the receiver that
///    just went away;
/// 3. it treats that undeliverable event as fatal, shuts the whole message
///    loop down, and every later call fails with "Unable to make method
///    calls because underlying connection is closed", permanently.
///
/// That is the entire mechanism behind the round-3 field blocker: EVERY
/// flow that logged in recorded fine and then failed to replay, because
/// the login redirect is exactly a post-idle navigation. Silence is not
/// evidence of a dead browser - a browser that actually dies closes the
/// socket, which surfaces immediately and through a different path.
///
/// Choosing the value is a genuine trade-off, because headless_chrome
/// OVERLOADS this one knob across three jobs with opposite needs:
///
/// - `Browser`'s event-listener reap and the transport's idle reap want it
///   LONG (that is the bug above);
/// - `Transport::call_method` uses it as the bound on waiting for a call's
///   RESPONSE, which wants it SHORT: a response that never arrives blocks
///   for exactly this long. Setting it to "effectively never" turned a
///   failing CI job into one that hung for over three hours.
///
/// So: comfortably longer than any real gap between browser-level events
/// (the field flows idled 30-90 s; `Wait until` defaults to a 60 s bound),
/// and short enough that a lost response fails visibly instead of hanging.
/// A flow that deliberately waits longer than this in one step without
/// touching the browser is the case to revisit if it ever shows up.
const BROWSER_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// Show the browser window instead of running headless.
///
/// An environment variable rather than a spec field, deliberately. Watching a
/// browser drive is a property of the *run you are supervising*, not of the
/// flow: recording is the one human-in-the-loop step, and a committed
/// `headed: true` would follow the flow into CI, where there is no one to
/// watch and no display to watch on.
///
/// Presence-based, matching `FLOWPROOF_NO_SHARED_BROWSER` — `FLOWPROOF_HEADED=0`
/// still means headed, because a variable someone bothered to set is a
/// variable they meant.
///
/// **A headed recording is not pixel-identical to a headless one.** Headed
/// Chromium sizes its window from the desktop; headless uses a fixed default.
/// A flow with visual assertions should pin `browser.viewport` in its spec, or
/// its baselines will be recorded at one size and replayed at another.
fn headed_requested() -> bool {
    std::env::var_os("FLOWPROOF_HEADED").is_some() || keep_browser_open_requested()
}

/// Keep a privately-owned visible browser available for inspection after its
/// flow completes. Presence implies headed mode; a headless window cannot be
/// inspected. The CLI exposes this as `--keep-open`.
fn keep_browser_open_requested() -> bool {
    std::env::var_os("FLOWPROOF_KEEP_BROWSER_OPEN").is_some()
}

/// Headed runs are deliberately private. A shared browser isolates each flow
/// in an incognito context, which Chromium presents as a second window. When
/// the flow tab closes, the original window (including the keep-alive tab)
/// becomes visible again and looks like a leaked browser. A visible run is for
/// a person to watch, so cold-start reuse is worth less than owning one window
/// whose lifetime exactly matches the flow.
fn should_share_browser(headed: bool, no_shared: bool) -> bool {
    !headed && !no_shared
}

/// The small visible-window surface used at launch. Keeping it behind a trait
/// makes the important ordering testable without starting Chromium: maximize
/// first, then activate the flow tab before the app begins navigating.
trait HeadedTabWindow {
    fn maximize(&self) -> Result<(), DriverError>;
    fn foreground(&self) -> Result<(), DriverError>;
}

impl HeadedTabWindow for Tab {
    fn maximize(&self) -> Result<(), DriverError> {
        self.set_bounds(Bounds::Maximized)
            .map(|_| ())
            .map_err(|e| web_err("maximizing headed browser window", e))
    }

    fn foreground(&self) -> Result<(), DriverError> {
        // Target.activate selects the new flow target; Page.bringToFront asks
        // Chromium to activate the native window as well as its tab.
        self.activate()
            .map_err(|e| web_err("activating headed browser tab", e))?;
        self.bring_to_front()
            .map(|_| ())
            .map_err(|e| web_err("bringing headed browser window to the foreground", e))
    }
}

fn present_headed_window(window: &impl HeadedTabWindow, headed: bool) -> Result<(), DriverError> {
    if headed {
        window.maximize()?;
        window.foreground()?;
    }
    Ok(())
}

/// Build the launch options, split out from [`launch_browser`] so the headless
/// decision is testable without starting a browser.
fn launch_options_for(
    os_args: &[std::ffi::OsString],
    headed: bool,
) -> Result<LaunchOptions<'_>, AdapterError> {
    let mut options = LaunchOptions::default_builder();
    options.headless(!headed).sandbox(false);
    options.idle_browser_timeout(BROWSER_IDLE_TIMEOUT);
    options.args(os_args.iter().map(AsRef::as_ref).collect());
    if let Ok(path) = std::env::var("CHROME") {
        options.path(Some(path.into()));
    }
    options
        .build()
        .map_err(|e| AdapterError::Web(format!("building launch options: {e}")))
}

/// Explain a launch failure that only happens because the window was asked for.
///
/// Measured, not guessed: with `FLOWPROOF_HEADED=1` and no `DISPLAY`, Chromium
/// exits during startup, and the launcher — which is meanwhile scanning for the
/// DevTools port Chromium never opens — reports
/// `There are no available ports between 8000 and 9000 for debugging` after
/// several minutes. Nothing in that sentence mentions a display, so the obvious
/// reading is a port conflict, and the obvious fix is to go hunting for one.
///
/// This is the defect class `CHARTER.md` Milestone 1 names: a real upstream
/// failure presenting as a flowproof problem. The underlying error is kept —
/// it is still the truth — with the cause that actually explains it appended.
fn launch_failure_message(err: &str, headed: bool) -> String {
    if !headed {
        return format!("launching browser: {err}");
    }
    format!(
        "launching browser: {err}\n\
         note: Chromium was asked for a VISIBLE window by FLOWPROOF_HEADED or --keep-open. \
         A host with no desktop session (SSH, a container, a CI runner) cannot give it \
         one, and Chromium exits during startup — which surfaces as a port or timeout \
         error rather than as a missing display. Unset FLOWPROOF_HEADED to run headless."
    )
}

fn wait_until_closed(mut is_open: impl FnMut() -> bool, mut pause: impl FnMut()) {
    while is_open() {
        pause();
    }
}

/// One reading of the page's inventory, with the load state that says whether
/// it is worth believing yet.
struct SceneSample {
    ready: bool,
    entries: Vec<serde_json::Value>,
}

/// How many settling rounds before the inventory is captured regardless. A
/// page with a ticker, a carousel, or a spinner never truly stops changing,
/// so settling is best-effort with a bound: past this, the newest reading is
/// used and the step proceeds rather than hanging on a page that will never
/// go quiet.
const SCENE_SETTLE_ROUNDS: usize = 20;

/// Gap between readings. Long enough for a framework's show/hide pass to land
/// between two of them, short enough that an already-settled page pays one
/// extra round trip rather than a pause a person would notice.
const SCENE_SETTLE_INTERVAL: Duration = Duration::from_millis(100);

/// The target tokens a reading offers — what an authoring model may ground
/// against, and the only part of the scene whose churn matters here. A clock,
/// a character counter, or a field the user is typing into changes text and
/// values on every reading without changing which elements exist, and waiting
/// for those to hold still would mean waiting forever.
fn scene_shape(entries: &[serde_json::Value]) -> Vec<&str> {
    entries
        .iter()
        .filter_map(|entry| entry["target"].as_str())
        .collect()
}

/// Read the inventory only once the page has stopped rearranging it.
///
/// The scene is the grounding set an authoring model must choose from, so it
/// has to describe the page the chosen actions will actually run on. A step
/// that navigates leaves the next step's scene racing the new page: a server
/// that renders every variant of a form and hides the irrelevant ones in
/// script briefly presents *all* of them as rendered. A model handed that
/// union grounds faithfully onto a control that is about to disappear, and
/// the recorder then fails to find an element the model was entitled to
/// choose — blaming the model for the scene's mistake.
///
/// Two readings agreeing on their shape, with the document loaded, is the
/// signal that the rearranging is done.
fn settled_scene(
    mut sample: impl FnMut() -> Result<SceneSample, DriverError>,
    mut pause: impl FnMut(),
    rounds: usize,
) -> Result<Vec<serde_json::Value>, DriverError> {
    let mut previous = sample()?;
    for _ in 0..rounds {
        pause();
        let current = sample()?;
        if previous.ready
            && current.ready
            && scene_shape(&previous.entries) == scene_shape(&current.entries)
        {
            return Ok(current.entries);
        }
        previous = current;
    }
    Ok(previous.entries)
}

/// Launch a fresh Chromium (`CHROME` env var overrides the binary), optionally
/// with extra command-line flags. Headless unless `FLOWPROOF_HEADED` is set.
fn launch_browser(extra_args: &[String]) -> Result<Browser, AdapterError> {
    let headed = headed_requested();
    let os_args: Vec<std::ffi::OsString> = extra_args.iter().map(Into::into).collect();
    let options = launch_options_for(&os_args, headed)?;
    Browser::new(options)
        .map_err(|e| AdapterError::Web(launch_failure_message(&e.to_string(), headed)))
}

/// A private per-launch downloads directory when the flow didn't pin one via
/// `browser.downloads_dir`. Unique per launch (pid + a monotonic counter) so
/// two flows sharing the same shared-browser process never race over the
/// same directory.
fn fresh_downloads_dir() -> std::path::PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!("flowproof-downloads-{}-{n}", std::process::id()))
}

/// One Chromium process for the whole run, reused across flows. Each flow
/// gets an isolated incognito CONTEXT (its own cookies/cache), so reuse is
/// invisible to specs but the ~seconds-long cold start is paid ONCE per
/// suite instead of once per flow. `Browser` is a cloneable Arc handle;
/// holding one in the static keeps the process alive until the test binary
/// exits. Opt out with `FLOWPROOF_NO_SHARED_BROWSER=1`.
fn shared_browser() -> Result<Browser, AdapterError> {
    // Hold a keep-alive blank tab forever: headless Chrome exits when its
    // LAST target closes, so as flows open and close their own tabs this
    // one keeps the process — and its warm connection — alive (Playwright
    // keeps the browser independent of pages the same way).
    type SharedCell = Mutex<Option<(Browser, Arc<Tab>)>>;
    static SHARED: OnceLock<SharedCell> = OnceLock::new();
    let cell = SHARED.get_or_init(|| Mutex::new(None));
    let mut guard = cell.lock().unwrap_or_else(|e| e.into_inner());
    // Reuse only while the process is actually alive: a cheap CDP round
    // trip proves the transport. If Chrome exited (or the socket died),
    // relaunch transparently — the caller never sees a dead handle.
    if let Some((browser, _keepalive)) = guard.as_ref() {
        if browser.get_version().is_ok() {
            return Ok(browser.clone());
        }
    }
    let browser = launch_browser(&[])?;
    let keepalive = browser
        .new_tab()
        .map_err(|e| AdapterError::Web(format!("opening keep-alive tab: {e}")))?;
    *guard = Some((browser.clone(), keepalive));
    Ok(browser)
}

/// Browser-backed [`AppDriver`].
pub struct WebAppDriver {
    browser: Browser,
    /// Incognito context isolating this flow on the shared browser; `None`
    /// when the driver owns a private browser (the opt-out path), where a
    /// plain tab is already isolated.
    context_id: Option<String>,
    tab: Option<Arc<Tab>>,
    /// Session staged via [`AppDriver::stage_session`], applied by the next
    /// `launch` before the page loads.
    staged_session: Option<WebSession>,
    /// The CDP id of the localStorage seed script, held only until the
    /// first document has run it. See `drop_seed_script`.
    seed_script: Option<String>,
    /// Recent console/log lines from the page (bounded ring buffer),
    /// filled by a CDP event listener registered at launch — read
    /// retroactively when a step fails ([`AppDriver::debug_bundle`]).
    console: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<String>>>,
    /// Network mocks staged via [`AppDriver::stage_mocks`], installed by
    /// the next `launch` before navigation (CDP Fetch interception).
    staged_mocks: Vec<flowproof_driver::WebMock>,
    /// Browser config staged via [`AppDriver::stage_browser`], applied by
    /// the next `launch`: viewport/UA per-tab; extra flags swap in a
    /// private browser (flags only apply at process start).
    staged_browser: Option<flowproof_driver::WebBrowserConfig>,
    /// Native-dialog handling state, shared with the flow-wide
    /// `Page.javascriptDialogOpening` listener registered at launch. The
    /// listener runs on the tab's own event thread, so it responds to a
    /// dialog the instant it opens - a main-path CDP call would block behind
    /// the open dialog.
    dialogs: Arc<Mutex<DialogState>>,
    /// Where THIS launch's downloads land — `config.downloads_dir` if
    /// staged, otherwise a fresh directory this driver created and owns.
    /// Set once, at `launch`, from `Page.setDownloadBehavior`; read by
    /// `wait_for_download`. `None` until a `launch` with downloads enabled
    /// has run.
    downloads_dir: Option<std::path::PathBuf>,
}

/// Shared state between the driver and the flow-wide dialog listener.
#[derive(Default)]
struct DialogState {
    /// A one-shot disposition armed for the NEXT trigger, consumed by the
    /// listener when the declared dialog opens. `None` = nothing armed, so
    /// any dialog is UNDECLARED and hits the safety net.
    armed: Option<flowproof_driver::DialogArm>,
    /// What the armed handler observed and did, drained by the post-condition.
    fired: Option<flowproof_driver::FiredDialog>,
    /// An UNDECLARED dialog the safety net dismissed, drained to fail its step.
    unexpected: Option<flowproof_driver::FiredDialog>,
}

/// Cap on retained console lines — enough context for a failure, bounded
/// so a chatty app can't balloon the run bundle.
const CONSOLE_TAIL_CAP: usize = 100;

/// Standard base64 for CDP `Fetch.fulfillRequest` bodies — hand-rolled
/// (~15 lines) rather than pulling a crate into the adapter for one call.
fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

impl WebAppDriver {
    /// The mouse drag. Two things make it land where an earlier attempt
    /// managed 4 drops in 8, and neither of them is pacing:
    ///
    /// 1. Both midpoints are read in ONE layout. Scrolling the target into
    ///    view after computing the source's point moves the source out from
    ///    under the press about to be dispatched at it.
    /// 2. Every intermediate move names the held button. A mouse-family
    ///    library reads a move whose `which` is 0 as the button having come
    ///    up and abandons the drag, and CDP reports none held unless told.
    ///    Measured: dropping it takes the fixture from 10/10 to 0/10.
    ///
    /// Measured 20/20 against a live jQuery UI `sortable` with
    /// `connectToSortable`; recorded in `docs/design.md`.
    pub(crate) fn drag_mouse(
        &mut self,
        from: &UiaSelector,
        to: &UiaSelector,
    ) -> Result<(), DriverError> {
        // Enough moves for a sortable to compute intersection on the way in,
        // and enough dwell at the end for it to settle a placeholder before
        // the release.
        let (steps, dwell) = (20u32, 4u32);
        // Both midpoints, with the ONE scroll happening first: reading the
        // second after a scroll of its own would move the first out from
        // under the press that has already been dispatched at it.
        let b = self.midpoint_of(to)?;
        let a = self.midpoint_no_scroll(from)?;
        let tab = self.tab()?.clone();
        use Input::DispatchMouseEventTypeOption as K;
        let mouse = |kind: K, x: f64, y: f64, held: bool| Input::DispatchMouseEvent {
            Type: kind.clone(),
            x,
            y,
            button: held.then_some(Input::MouseButton::Left),
            // Redundant but explicit: Chrome derives `buttons` from `button`
            // above, and dropping BOTH is what takes the drop rate to zero.
            buttons: held.then_some(1),
            click_count: Some(1),
            modifiers: None,
            timestamp: None,
            force: None,
            tangential_pressure: None,
            tilt_x: None,
            tilt_y: None,
            twist: None,
            delta_x: None,
            delta_y: None,
            pointer_Type: None,
        };
        let send = |e| {
            tab.call_method(e)
                .map(|_| ())
                .map_err(|err| web_err("dragging", err))
        };
        send(mouse(K::MouseMoved, a.0, a.1, false))?;
        send(mouse(K::MousePressed, a.0, a.1, true))?;
        // Clear the library's distance threshold before aiming anywhere.
        send(mouse(K::MouseMoved, a.0 + 6.0, a.1 + 6.0, true))?;
        for i in 1..=steps {
            let t = f64::from(i) / f64::from(steps);
            send(mouse(
                K::MouseMoved,
                a.0 + (b.0 - a.0) * t,
                a.1 + (b.1 - a.1) * t,
                true,
            ))?;
        }
        // Dwell inside the target: a sortable computes intersection on move,
        // so the last position needs more than one event to settle on.
        for i in 0..dwell {
            let nudge = f64::from(i % 2);
            send(mouse(K::MouseMoved, b.0 + nudge, b.1 + nudge, true))?;
        }
        send(mouse(K::MouseReleased, b.0, b.1, true))?;
        Ok(())
    }

    /// Viewport-space midpoint WITHOUT scrolling - for the second of a pair
    /// that must be read in the same layout.
    fn midpoint_no_scroll(&mut self, selector: &UiaSelector) -> Result<(f64, f64), DriverError> {
        self.midpoint_inner(selector, false)
    }

    /// Viewport-space midpoint of an element, after scrolling it into view.
    fn midpoint_of(&mut self, selector: &UiaSelector) -> Result<(f64, f64), DriverError> {
        self.midpoint_inner(selector, true)
    }

    fn midpoint_inner(
        &mut self,
        selector: &UiaSelector,
        scroll: bool,
    ) -> Result<(f64, f64), DriverError> {
        let locator = Self::locator(selector)?;
        let got = self.with_element(&locator, &format!("locating [{selector}]"), |element| {
            if scroll {
                element.scroll_into_view()?;
            }
            let v = element.call_js_fn(
                r#"function() {
                    const r = this.getBoundingClientRect();
                    return JSON.stringify({ x: r.x + r.width / 2, y: r.y + r.height / 2 });
                }"#,
                vec![],
                false,
            )?;
            Ok(v.value.and_then(|v| v.as_str().map(str::to_string)))
        })?;
        let raw = got.ok_or_else(|| DriverError::Browser(format!("[{selector}] has no box")))?;
        let parsed: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| DriverError::Browser(format!("reading the midpoint: {e}")))?;
        Ok((
            parsed
                .get("x")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or_default(),
            parsed
                .get("y")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or_default(),
        ))
    }

    /// A driver on the shared browser (isolated context per flow), or a
    /// private browser when `FLOWPROOF_NO_SHARED_BROWSER=1` or the browser is
    /// headed. Headed ownership prevents the keep-alive window from surviving
    /// after the visible flow window closes.
    pub fn new() -> Result<Self, AdapterError> {
        if !should_share_browser(
            headed_requested(),
            std::env::var_os("FLOWPROOF_NO_SHARED_BROWSER").is_some(),
        ) {
            return Ok(Self {
                browser: launch_browser(&[])?,
                context_id: None,
                tab: None,
                staged_session: None,
                seed_script: None,
                console: Default::default(),
                staged_mocks: Vec::new(),
                staged_browser: None,
                dialogs: Default::default(),
                downloads_dir: None,
            });
        }
        let browser = shared_browser()?;
        let context = browser
            .new_context()
            .map_err(|e| AdapterError::Web(format!("creating browser context: {e}")))?;
        let context_id = context.get_id().to_string();
        Ok(Self {
            browser,
            context_id: Some(context_id),
            tab: None,
            staged_session: None,
            seed_script: None,
            console: Default::default(),
            staged_mocks: Vec::new(),
            staged_browser: None,
            dialogs: Default::default(),
            downloads_dir: None,
        })
    }

    /// Apply staged session state to a fresh tab BEFORE navigation: cookies
    /// via CDP, localStorage via an on-new-document script (Playwright's
    /// addInitScript pattern — it runs before any page script on every
    /// navigation, so the app boots already seeded).
    ///
    /// The script re-runs on EVERY document in the tab (CDP semantics), but
    /// seeding is a one-time fixture, not an invariant: a flow that seeds a
    /// cart, mutates it through the UI, then navigates must KEEP the
    /// mutation. So the registration id comes back to the caller, which
    /// drops the script once the first document has it (see
    /// `drop_seed_script`).
    ///
    /// It used to guard itself with a `sessionStorage` sentinel instead.
    /// That made seeding depend on the PAGE's storage semantics, which do
    /// not hold where it matters: sessionStorage is per ORIGIN, so any
    /// navigation that crosses one (a login host to an app host, or two
    /// `file://` documents, which modern Chrome treats as separate origins)
    /// could not see the sentinel, re-seeded, and silently overwrote the
    /// user's mutation. Removing the script instead is decided entirely on
    /// our side of the boundary, so no page or origin can defeat it.
    fn apply_session(
        tab: &Arc<Tab>,
        session: &WebSession,
        url: &str,
    ) -> Result<Option<String>, DriverError> {
        let mut seed_script = None;
        if !session.local_storage.is_empty() {
            let mut source = String::from("try{");
            for (key, value) in &session.local_storage {
                let key = serde_json::to_string(key).unwrap_or_default();
                let value = serde_json::to_string(value).unwrap_or_default();
                source.push_str(&format!("localStorage.setItem({key},{value});"));
            }
            source.push_str("}catch(e){}");
            let added = tab
                .call_method(Page::AddScriptToEvaluateOnNewDocument {
                    source,
                    world_name: None,
                    include_command_line_api: None,
                    run_immediately: None,
                })
                .map_err(|e| web_err("seeding localStorage", e))?;
            seed_script = Some(added.identifier);
        }
        if !session.cookies.is_empty() {
            let cookies = session
                .cookies
                .iter()
                .map(|(name, value, domain)| Network::CookieParam {
                    name: name.clone(),
                    value: value.clone(),
                    // Without an explicit domain the cookie binds to the
                    // launch URL's host.
                    url: domain.is_none().then(|| url.to_string()),
                    domain: domain.clone(),
                    path: None,
                    secure: None,
                    http_only: None,
                    same_site: None,
                    expires: None,
                    priority: None,
                    same_party: None,
                    source_scheme: None,
                    source_port: None,
                    partition_key: None,
                })
                .collect();
            tab.set_cookies(cookies)
                .map_err(|e| web_err("setting session cookies", e))?;
        }
        Ok(seed_script)
    }

    /// Drop the seed script once the first document has run it, so no later
    /// navigation re-seeds over what the flow has since changed. Best
    /// effort: if the removal fails the run is still correct for the common
    /// same-origin case, and failing the flow over a cleanup call would be
    /// worse than the stale registration.
    fn drop_seed_script(&mut self) {
        let Some(identifier) = self.seed_script.take() else {
            return;
        };
        if let Ok(tab) = self.tab() {
            let _ = tab.call_method(Page::RemoveScriptToEvaluateOnNewDocument { identifier });
        }
    }

    fn tab(&self) -> Result<&Arc<Tab>, DriverError> {
        self.tab
            .as_ref()
            .ok_or_else(|| DriverError::Browser("no page open: call launch first".into()))
    }

    fn locator_of(selector: &UiaSelector) -> Option<WebLocator> {
        let nth = selector.nth;
        if let Some(cell) = &selector.cell {
            return Some(WebLocator {
                css: None,
                text: None,
                nth: None,
                cell: Some(cell.clone()),
                scope: None,
            });
        }
        if let Some(scope) = &selector.scope {
            return Some(WebLocator {
                css: None,
                text: None,
                nth: None,
                cell: None,
                scope: Some(scope.clone()),
            });
        }
        if let Some(css) = selector.css_selector() {
            return Some(WebLocator {
                css: Some(css),
                text: None,
                nth,
                cell: None,
                scope: None,
            });
        }
        // Text anchor: find by visible text / accessible label / placeholder
        // — how humans (and Playwright's getByText/getByPlaceholder/getByRole
        // name matching) address elements on pages without ids.
        selector.name.as_ref().map(|text| WebLocator {
            css: None,
            text: Some(text.clone()),
            nth,
            cell: None,
            scope: None,
        })
    }

    fn locator(selector: &UiaSelector) -> Result<WebLocator, DriverError> {
        Self::locator_of(selector).ok_or_else(|| {
            DriverError::Browser(format!(
                "selector [{selector}] has no css, automation_id, or text"
            ))
        })
    }

    /// Resolve a table cell by IDENTITY (#58): run a JS pass that finds the
    /// column by header text and the row by anchor, TAGS the cell with a
    /// unique attribute, and reports status; then find the tagged element
    /// through the normal CSS path. Ambiguity is a hard error (Fable), a
    /// clean miss returns `Ok(None)` like any element miss.
    /// The resolver JS call for a cell, with its params JSON-injected.
    fn cell_resolver_js(cell: &flowproof_driver::CellQuery) -> String {
        let opt = |v: &Option<String>| {
            v.as_deref()
                .map(|x| serde_json::Value::from(x).to_string())
                .unwrap_or_else(|| "null".into())
        };
        format!(
            "({CELL_RESOLVER})({col},{anchor},{field},{rowid},{also})",
            col = serde_json::Value::from(cell.column.as_str()),
            anchor = serde_json::Value::from(cell.anchor.as_str()),
            field = opt(&cell.column_field),
            rowid = opt(&cell.row_id),
            also = serde_json::Value::from(cell.also.clone()),
        )
    }

    /// Run the resolver, which TAGS the winning cell, and return the status.
    /// Shared by resolve_cell (which then finds the tag) and cell_hints
    /// (which reads attributes off the tag).
    fn tag_cell(&self, cell: &flowproof_driver::CellQuery) -> Result<String, DriverError> {
        Ok(self
            .tab()?
            .evaluate(&Self::cell_resolver_js(cell), false)
            .map_err(|e| web_err("resolving a table cell", e))?
            .value
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default())
    }

    fn resolve_cell(
        &self,
        cell: &flowproof_driver::CellQuery,
    ) -> Result<Option<headless_chrome::Element<'_>>, DriverError> {
        let status = self.tag_cell(cell)?;
        match status.as_str() {
            "ok" => {
                // Tagged; resolve it through the ordinary path.
                self.try_find(&WebLocator {
                    css: Some("[data-flowproof-cell]".into()),
                    text: None,
                    nth: None,
                    cell: None,
                    scope: None,
                })
            }
            // A miss is a miss - the auto-wait loop will retry, and the
            // final error names the cell.
            "no_match" | "no_row" | "no_column" | "no_header" => Ok(None),
            other => {
                // Ambiguity and duplicate headers are hard errors: the
                // spec named an identity that does not uniquely resolve.
                let detail = if let Some(n) = other.strip_prefix("ambiguous_row:") {
                    format!(
                        "row anchor '{}' matches {n} rows - use a more specific anchor",
                        cell.anchor
                    )
                } else if other == "dup_header" {
                    format!(
                        "column header '{}' is not unique - use a `css:` selector for this table",
                        cell.column
                    )
                } else {
                    format!("could not resolve the cell ({other})")
                };
                Err(DriverError::Browser(detail))
            }
        }
    }

    /// The container-resolver JS call, with its params JSON-injected.
    fn scope_resolver_js(scope: &flowproof_driver::ScopeQuery) -> String {
        let opt = |v: &Option<String>| {
            v.as_deref()
                .map(|x| serde_json::Value::from(x).to_string())
                .unwrap_or_else(|| "null".into())
        };
        format!(
            "({SCOPE_RESOLVER})({container},{anchor},{id},{items},{also})",
            container = serde_json::Value::from(scope.container.as_str()),
            anchor = serde_json::Value::from(scope.anchor.as_str()),
            id = opt(&scope.container_id),
            items = serde_json::Value::from(ITEM_CONTAINERS),
            also = serde_json::Value::from(scope.also.clone()),
        )
    }

    /// Run the container resolver, which TAGS the winning container, and
    /// return its status. Shared by resolve_scope and scope_hints.
    fn tag_scope(&self, scope: &flowproof_driver::ScopeQuery) -> Result<String, DriverError> {
        Ok(self
            .tab()?
            .evaluate(&Self::scope_resolver_js(scope), false)
            .map_err(|e| web_err("resolving a scoped container", e))?
            .value
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default())
    }

    /// Resolve a scoped-container target: find the container by identity,
    /// TAG it, then run the ORDINARY resolution ladder rooted at the tag -
    /// the same tag-then-find trick `resolve_cell` uses. Ambiguity is a
    /// hard error; a clean miss returns `Ok(None)` like any element miss.
    fn resolve_scope(
        &self,
        scope: &flowproof_driver::ScopeQuery,
    ) -> Result<Option<headless_chrome::Element<'_>>, DriverError> {
        let status = self.tag_scope(scope)?;
        match status.as_str() {
            "ok" => {
                if let Some(css) = scope.inner_css_selector() {
                    return self.try_find(&WebLocator {
                        css: Some(format!("[data-flowproof-scope] {css}")),
                        text: None,
                        nth: None,
                        cell: None,
                        scope: None,
                    });
                }
                let Some(text) = &scope.inner_text else {
                    return Err(DriverError::Browser(
                        "a scoped target needs an inner css, id, or text".into(),
                    ));
                };
                let tab = self.tab()?;
                for xpath in text_xpaths(text) {
                    if let Ok(element) = tab.find_element_by_xpath(&rooted_xpath(&xpath)) {
                        return Ok(Some(element));
                    }
                }
                Ok(None)
            }
            // Both are misses: the auto-wait loop retries, and the failure
            // message names the container. `anchor_without_container` is
            // still a miss - it only changes what the TIMEOUT says.
            "no_match" | "anchor_without_container" => Ok(None),
            other => {
                let detail = if let Some(n) = other.strip_prefix("ambiguous:") {
                    format!(
                        "container anchor '{}' matches {n} items - use a more specific anchor \
                         or a css: container",
                        scope.anchor
                    )
                } else if other == "bad_container" {
                    format!(
                        "'{}' is not a usable container - use the word `item` or a \
                         \"css:<selector>\"",
                        scope.container
                    )
                } else {
                    format!("could not resolve the container ({other})")
                };
                Err(DriverError::Browser(detail))
            }
        }
    }

    /// One resolution attempt, in preference order: css, then exact text
    /// anchor, then prefix text anchor (Playwright's name matching accepts
    /// a leading match when the accessible name carries trailing detail —
    /// catalog cards, chips like `ID: …`).
    fn try_find(
        &self,
        locator: &WebLocator,
    ) -> Result<Option<headless_chrome::Element<'_>>, DriverError> {
        let tab = self.tab()?;
        if let Some(cell) = &locator.cell {
            return self.resolve_cell(cell);
        }
        if let Some(scope) = &locator.scope {
            return self.resolve_scope(scope);
        }
        if let Some(css) = &locator.css {
            return Ok(match locator.nth {
                None => tab.find_element(css).ok(),
                Some(n) => tab
                    .find_elements(css)
                    .ok()
                    .and_then(|found| found.into_iter().nth(n.saturating_sub(1) as usize)),
            });
        }
        if let Some(text) = &locator.text {
            for xpath in text_xpaths(text) {
                let xpath = match locator.nth {
                    Some(n) => format!("({xpath})[{n}]"),
                    None => xpath,
                };
                if let Ok(element) = tab.find_element_by_xpath(&xpath) {
                    return Ok(Some(element));
                }
            }
        }
        Ok(None)
    }

    fn find(&self, locator: &WebLocator) -> Result<headless_chrome::Element<'_>, DriverError> {
        let deadline = std::time::Instant::now() + FIND_TIMEOUT;
        loop {
            if let Some(element) = self.try_find(locator)? {
                return Ok(element);
            }
            if std::time::Instant::now() >= deadline {
                return Err(DriverError::Browser(format!("no element for {locator}")));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn exists(&self, locator: &WebLocator) -> Result<bool, DriverError> {
        Ok(self.try_find(locator)?.is_some())
    }

    /// The in-page mirror of `try_find` for css and text-anchor locators:
    /// one JS expression that resolves to the element or `null`. `None`
    /// for the locator shapes it does not cover (cell, scope) — those stay
    /// on the element-handle path.
    ///
    /// This exists because the CDP transport pays a fixed latency per
    /// round trip, and `find_element` alone is four of them. Probes that
    /// only need an answer — exists, the actionability gate — ask the page
    /// once instead.
    fn js_resolver(locator: &WebLocator) -> Option<String> {
        if locator.cell.is_some() || locator.scope.is_some() {
            return None;
        }
        let nth = locator
            .nth
            .map_or_else(|| "null".to_string(), |n| n.to_string());
        if let Some(css) = &locator.css {
            let css = serde_json::Value::from(css.as_str()).to_string();
            return Some(format!(
                "((css, nth) => nth ? (document.querySelectorAll(css)[nth - 1] || null) \
                 : document.querySelector(css))({css}, {nth})"
            ));
        }
        let text = locator.text.as_deref()?;
        // The SAME xpath ladder try_find walks, evaluated in order in one
        // pass: the first rung with a match wins.
        let xpaths = serde_json::Value::from(text_xpaths(text)).to_string();
        Some(format!(
            "((xpaths, nth) => {{
                for (const xp of xpaths) {{
                    const q = nth ? '(' + xp + ')[' + nth + ']' : xp;
                    const found = document.evaluate(
                        q, document, null, XPathResult.FIRST_ORDERED_NODE_TYPE, null);
                    if (found.singleNodeValue) {{ return found.singleNodeValue; }}
                }}
                return null;
            }})({xpaths}, {nth})"
        ))
    }

    /// Run an element operation with ONE retry on a CDP transport fault:
    /// re-resolve the element (its object id may be gone with the dead
    /// connection) and try again before failing the step.
    fn with_element<T>(
        &self,
        locator: &WebLocator,
        context: &str,
        op: impl Fn(&headless_chrome::Element<'_>) -> Result<T, anyhow::Error>,
    ) -> Result<T, DriverError> {
        let mut retried = false;
        loop {
            let element = self.find(locator)?;
            match op(&element) {
                Ok(value) => return Ok(value),
                Err(e) if !retried && is_transport_fault(&e.to_string()) => {
                    retried = true;
                    std::thread::sleep(Duration::from_millis(300));
                }
                Err(e) => return Err(web_err(context, e)),
            }
        }
    }
}

/// "bottom" or "top", for a scroll failure message.
fn edge_word(to_bottom: bool) -> &'static str {
    if to_bottom {
        "bottom"
    } else {
        "top"
    }
}

/// Faults of the CDP transport itself (dead websocket, dropped event) —
/// distinct from "element not found": worth one retry with a fresh handle.
fn is_transport_fault(message: &str) -> bool {
    let m = message.to_lowercase();
    m.contains("connection is closed")
        || m.contains("the event waited for never came")
        || m.contains("unable to make method calls")
}

/// How a [`UiaSelector`] resolves on a page: a CSS selector or a text
/// anchor, optionally narrowed to the nth match (1-based).
struct WebLocator {
    css: Option<String>,
    text: Option<String>,
    nth: Option<u32>,
    cell: Option<flowproof_driver::CellQuery>,
    scope: Option<flowproof_driver::ScopeQuery>,
}

/// Root every union branch of a text-anchor XPath under the tagged
/// container, so the ordinary ladder searches INSIDE the scope instead of
/// page-wide. Branches are joined with ` | ` and each starts at the
/// document root; splitting on ` | //` (not ` | `) keeps a literal that
/// happens to contain a pipe from being mistaken for a branch break.
fn rooted_xpath(xpath: &str) -> String {
    const ROOT: &str = "//*[@data-flowproof-scope]";
    xpath
        .split(" | //")
        .enumerate()
        .map(|(i, branch)| {
            if i == 0 {
                format!("{ROOT}{branch}")
            } else {
                format!("{ROOT}//{branch}")
            }
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

impl std::fmt::Display for WebLocator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(cell) = &self.cell {
            return write!(
                f,
                "the \"{}\" column of the row containing \"{}\"",
                cell.column, cell.anchor
            );
        }
        if let Some(scope) = &self.scope {
            let inner = scope
                .inner_css_selector()
                .or_else(|| scope.inner_text.clone())
                .unwrap_or_default();
            return write!(
                f,
                "the \"{inner}\" in the {} containing \"{}\"",
                scope.container, scope.anchor
            );
        }
        match (&self.css, &self.text) {
            (Some(css), _) => write!(f, "css '{css}'")?,
            (None, Some(text)) => write!(f, "text '{text}'")?,
            (None, None) => write!(f, "empty locator")?,
        }
        if let Some(n) = self.nth {
            write!(f, " (match #{n})")?;
        }
        Ok(())
    }
}

/// XPaths matching an interactable element by its visible text, accessible
/// label, or placeholder — Playwright's text/placeholder addressing —
/// tried in order: (1) exact match on the element's DIRECT text nodes (its
/// own text, so a sibling avatar's initials can never fuse with a label
/// into "ETE2E Test Runner's Team"); (2) exact match on the concatenated
/// subtree text (covers labels wrapped in spans); (3) `<label>` association
/// by the label's exact text — both the wrapping form
/// `<label>Name: <input/></label>` and `<label for>`/`id` pairing; (4) and
/// (5) the own-text/subtree rungs as prefix matches (a leading match is
/// accepted when the accessible name carries trailing detail — catalog
/// cards, chips like `ID: …`); (6) label association as a prefix match
/// (so `Name` finds the field labelled `Name:`); (7) and (8) ASCII
/// case-insensitive fallbacks of the exact and prefix rungs — role names
/// are case-insensitive in Playwright, and real pages disagree with specs
/// about capitalization ("Close Account" vs "Close account"). A
/// case-sensitive match always wins over a case-insensitive one.
///
/// Button-type inputs (`type=submit|button|reset`) are void elements whose
/// accessible name comes from the `value` attribute (HTML-AAM), so every
/// build rung also matches them by `@value` with the rung's own comparison
/// (exact, prefix, or case-insensitive). Only those three types: text-like
/// inputs hold user data in `value`, not a name.
fn text_xpaths(text: &str) -> Vec<String> {
    const UPPER: &str = "'ABCDEFGHIJKLMNOPQRSTUVWXYZ'";
    const LOWER: &str = "'abcdefghijklmnopqrstuvwxyz'";
    let lit = xpath_literal(text);
    let lower_lit = xpath_literal(&text.to_ascii_lowercase());
    let ci = |expr: &str| format!("translate({expr}, {UPPER}, {LOWER})={lower_lit}");
    let ci_prefix =
        |expr: &str| format!("starts-with(translate({expr}, {UPPER}, {LOWER}), {lower_lit})");
    let build = |by_text: String, by_label: String, by_placeholder: String, by_value: String| {
        format!(
            "//*[self::button or self::a or self::summary or @role='button' or \
             @role='tab' or @role='option' or @type='submit']\
             [{by_text} or {by_label}] | \
             //input[(@type='submit' or @type='button' or @type='reset') and {by_value}] | \
             //input[{by_placeholder} or {by_label}] | \
             //textarea[{by_placeholder} or {by_label}]"
        )
    };
    // Fields addressed by their <label>: the wrapping form associates by
    // containment, the `for` form by id. XPath 1.0 node-set comparison
    // makes `@id = //label[…]/@for` "any label whose for equals this id".
    let by_label_assoc = |label_text: String| {
        ["input", "textarea", "select"]
            .map(|tag| {
                format!(
                    "//label[{label_text}]//{tag} | \
                     //{tag}[@id = //label[{label_text}]/@for]"
                )
            })
            .join(" | ")
    };
    vec![
        build(
            format!("text()[normalize-space(.)={lit}]"),
            format!("@aria-label={lit}"),
            format!("@placeholder={lit}"),
            format!("@value={lit}"),
        ),
        build(
            format!("normalize-space()={lit}"),
            format!("@aria-label={lit}"),
            format!("@placeholder={lit}"),
            format!("@value={lit}"),
        ),
        by_label_assoc(format!("normalize-space()={lit}")),
        build(
            format!("text()[starts-with(normalize-space(.), {lit})]"),
            format!("starts-with(@aria-label, {lit})"),
            format!("starts-with(@placeholder, {lit})"),
            format!("starts-with(@value, {lit})"),
        ),
        build(
            format!("starts-with(normalize-space(), {lit})"),
            format!("starts-with(@aria-label, {lit})"),
            format!("starts-with(@placeholder, {lit})"),
            format!("starts-with(@value, {lit})"),
        ),
        by_label_assoc(format!("starts-with(normalize-space(), {lit})")),
        format!(
            "{} | {}",
            build(
                ci("normalize-space()"),
                ci("@aria-label"),
                ci("@placeholder"),
                ci("@value"),
            ),
            by_label_assoc(ci("normalize-space()")),
        ),
        format!(
            "{} | {}",
            build(
                ci_prefix("normalize-space()"),
                ci_prefix("@aria-label"),
                ci_prefix("@placeholder"),
                ci_prefix("@value"),
            ),
            by_label_assoc(ci_prefix("normalize-space()")),
        ),
    ]
}

/// Quote `text` as an XPath string literal, handling embedded quotes.
fn xpath_literal(text: &str) -> String {
    if !text.contains('\'') {
        format!("'{text}'")
    } else if !text.contains('"') {
        format!("\"{text}\"")
    } else {
        let parts: Vec<String> = text.split('\'').map(|p| format!("'{p}'")).collect();
        format!("concat({})", parts.join(", \"'\", "))
    }
}

impl Drop for WebAppDriver {
    fn drop(&mut self) {
        // An inspected browser remains owned by Flowproof: wait until the user
        // closes its flow tab, then perform the ordinary Browser drop so the
        // child process is reaped and its temporary profile is removed. This
        // is intentionally a wait rather than a leaked/detached Chrome.
        if self.context_id.is_none() && keep_browser_open_requested() {
            if let Some(flow_tab) = self.tab.as_ref() {
                eprintln!(
                    "flow finished; keeping Chromium open (--keep-open). Close its window to exit."
                );
                wait_until_closed(
                    || {
                        self.browser
                            .get_tabs()
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .iter()
                            .any(|tab| Arc::ptr_eq(tab, flow_tab))
                    },
                    || std::thread::sleep(Duration::from_millis(200)),
                );
            }
        }
        // The shared browser outlives this driver: close the flow tab so pages
        // don't accumulate across a suite. A private browser (opt-out or
        // headed) is torn down with its process.
        if self.context_id.is_some() {
            if let Some(tab) = self.tab.take() {
                let _ = tab.close(false);
            }
        }
    }
}

impl WebAppDriver {
    /// Run one framed operation and turn its status into a result.
    ///
    /// `op` is `type`, `clear` or `scroll`; `arg` is the text or the pixel
    /// offset. Every failure is named, because "it did not work" inside a
    /// frame is the hardest thing to diagnose from outside one.
    fn frame_act(
        &mut self,
        query: &flowproof_driver::FrameQuery,
        op: &str,
        arg: serde_json::Value,
    ) -> Result<String, DriverError> {
        let js = |v: &Option<String>| {
            v.as_deref()
                .map(|x| serde_json::Value::from(x).to_string())
                .unwrap_or_else(|| "null".into())
        };
        let call = format!(
            "({FRAME_ACT})({frame},{css},{id},{text},{op},{arg})",
            frame = serde_json::Value::from(query.frame.as_str()),
            css = js(&query.inner_css),
            id = js(&query.inner_id),
            text = js(&query.inner_text),
            op = serde_json::Value::from(op),
            arg = arg,
        );
        let status = self
            .tab()?
            .evaluate(&call, false)
            .map_err(|e| web_err("acting inside an iframe", e))?
            .value
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        let frame = &query.frame;
        match status.as_str() {
            "cross_origin" => Err(cross_origin(frame)),
            "frame_hidden" => Err(DriverError::Browser(format!(
                "iframe '{frame}' is not rendered, so driving it would act on a surface \
                 nobody can see"
            ))),
            "no_element" => Err(DriverError::Browser(format!(
                "the target was not found inside iframe '{frame}'"
            ))),
            "disabled" => Err(DriverError::Browser(format!(
                "the target inside iframe '{frame}' is disabled - real typing would be \
                 ignored, so setting its value would be a lie"
            ))),
            "readonly" => Err(DriverError::Browser(format!(
                "the target inside iframe '{frame}' is read-only"
            ))),
            "not_scrollable" => Err(DriverError::Browser(format!(
                "the target inside iframe '{frame}' is not a scroll container (its content \
                 fits), so scrolling it would pass without moving anything"
            ))),
            s if s.starts_with("no_frame:") => {
                let available: Vec<String> =
                    serde_json::from_str(&s["no_frame:".len()..]).unwrap_or_default();
                // The same wording an assertion gives, so a missing frame
                // reads identically whichever half of the grammar met it.
                Err(DriverError::Browser(
                    flowproof_driver::frame_miss(
                        frame,
                        &flowproof_driver::FrameProbe::NoFrame { available },
                    )
                    .to_string(),
                ))
            }
            s if s.starts_with("clamped:") => Err(DriverError::Browser(format!(
                "the target inside iframe '{frame}' stops at {}px",
                &s["clamped:".len()..]
            ))),
            s if s.starts_with("took:") => Err(DriverError::Browser(format!(
                "the value did not take inside iframe '{frame}': it now reads '{}'",
                &s["took:".len()..]
            ))),
            _ => Ok(status),
        }
    }
}

/// Would a click at `el`'s centre reach `el`?
///
/// Shared verbatim by the two callers that ask it, because they are supposed
/// to agree and did not: recording asks through `element_receives_events`,
/// replay through the single-round-trip `actionability_gate`, and the gate
/// kept an older copy of the rule. A styled radio was therefore recordable
/// and unreplayable — the record accepted the click, the replay refused it,
/// and the trace was correct all along.
///
/// `el` is the element; `hit` is what `elementFromPoint` returned for its
/// centre. Written as a JS expression body over those two names.
const FORWARDS_CLICK_JS: &str = r#"
    (() => {
        if (!hit) { return false; }
        if (hit === el || el.contains(hit) || hit.contains(el)) { return true; }
        // A custom-styled checkbox or radio: the real input is visually
        // replaced by a sibling inside its own label, so the hit is neither
        // ancestor nor descendant. The browser forwards a click anywhere in
        // the label to the input - which is how a person ticks the box.
        //
        // On the BROWSER's terms, not on the mere presence of a label. A
        // label labels ONE control, so a label wrapping several cannot lend
        // its area to the others; and interactive content inside a label
        // keeps the activation for itself, so a hit on a link or a button
        // there leaves this control untouched.
        const label = hit.closest('label');
        const labels = el.labels ? Array.from(el.labels) : [];
        if (!label || !labels.includes(label)) { return false; }
        const INTERACTIVE = 'a[href], area[href], button, details, embed, iframe, \
            select, textarea, audio[controls], video[controls], img[usemap], \
            input:not([type=hidden])';
        for (let node = hit; node && node !== label; node = node.parentElement) {
            if (node.matches(INTERACTIVE)) { return false; }
        }
        return true;
    })()
"#;

impl AppDriver for WebAppDriver {
    fn cell_hints(
        &mut self,
        selector: &UiaSelector,
    ) -> Result<Option<flowproof_driver::CellHints>, DriverError> {
        let Some(cell) = &selector.cell else {
            return Ok(None);
        };
        // Tag the winning cell, then read the field and row id off the tag,
        // harvested at record time so replay can fall back to them when the
        // header text or the anchor has since changed. No element handle
        // needed, so this skips resolve_cell's find step.
        if self.tag_cell(cell)? != "ok" {
            return Ok(None);
        }
        let raw = self
            .tab()?
            .evaluate(&format!("({CELL_HINTS})()"), false)
            .map_err(|e| web_err("reading cell hints", e))?
            .value
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        let parsed: serde_json::Value =
            serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
        let field = parsed
            .get("field")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let id = parsed
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        Ok(Some(flowproof_driver::CellHints {
            column_field: field,
            row_id: id,
        }))
    }

    fn scope_hints(
        &mut self,
        selector: &UiaSelector,
    ) -> Result<Option<flowproof_driver::ScopeHints>, DriverError> {
        let Some(scope) = &selector.scope else {
            return Ok(None);
        };
        // Tag the winning container, then read its id off the tag. The same
        // pass answers the failure question: `anchor_without_container`
        // means the anchor IS on the page but no container holds it, which
        // is the one miss whose message should name the fix.
        let status = self.tag_scope(scope)?;
        if status == "anchor_without_container" {
            return Ok(Some(flowproof_driver::ScopeHints {
                container_id: None,
                anchor_without_container: true,
            }));
        }
        if status != "ok" {
            return Ok(None);
        }
        let raw = self
            .tab()?
            .evaluate(&format!("({SCOPE_HINTS})()"), false)
            .map_err(|e| web_err("reading container hints", e))?
            .value
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        let parsed: serde_json::Value =
            serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
        Ok(Some(flowproof_driver::ScopeHints {
            container_id: parsed
                .get("id")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            anchor_without_container: false,
        }))
    }

    /// `command` is the URL to open; `window_name` is unused for web.
    fn launch(
        &mut self,
        command: &str,
        _window_name: &str,
        _timeout: Duration,
    ) -> Result<(), DriverError> {
        let staged_browser = self.staged_browser.take();
        // Extra Chrome flags only apply at process start: swap in a
        // PRIVATE browser for this flow (a plain tab on it is already
        // isolated), paying its cold start instead of sharing.
        if let Some(config) = &staged_browser {
            if !config.args.is_empty() {
                self.browser = launch_browser(&config.args)
                    .map_err(|e| DriverError::Browser(e.to_string()))?;
                self.context_id = None;
            }
        }
        // On the shared browser, open the tab INSIDE this flow's isolated
        // context so its cookies/storage never touch another flow's.
        let tab = match &self.context_id {
            Some(id) => self.browser.new_tab_with_options(CreateTarget {
                url: "about:blank".to_string(),
                browser_context_id: Some(id.clone()),
                left: None,
                top: None,
                width: None,
                height: None,
                window_state: None,
                enable_begin_frame_control: None,
                new_window: None,
                background: None,
                for_tab: None,
                hidden: None,
            }),
            None => self.browser.new_tab(),
        }
        .map_err(|e| web_err("opening tab", e))?;
        // A visible flow is the only Chrome window the user should have to
        // manage. Chromium's default launch bounds can be a narrow utility
        // window (and an incognito context used to create another one), so
        // present the privately-owned headed window at the desktop's normal
        // maximized size. Device emulation below still pins the page viewport
        // when the spec requests one.
        present_headed_window(tab.as_ref(), headed_requested())?;
        // Viewport/UA emulation BEFORE navigation, so the app boots into
        // the emulated device (responsive breakpoints, UA sniffing). NOT
        // best-effort: a flow recorded mobile must never run desktop.
        if let Some(config) = &staged_browser {
            if let Some(vp) = &config.viewport {
                tab.call_method(Emulation::SetDeviceMetricsOverride {
                    width: vp.width,
                    height: vp.height,
                    device_scale_factor: vp.device_scale_factor,
                    mobile: vp.mobile,
                    scale: None,
                    screen_width: None,
                    screen_height: None,
                    position_x: None,
                    position_y: None,
                    dont_set_visible_size: None,
                    screen_orientation: None,
                    viewport: None,
                    display_feature: None,
                    device_posture: None,
                })
                .map_err(|e| web_err("emulating viewport", e))?;
                if vp.touch {
                    tab.call_method(Emulation::SetTouchEmulationEnabled {
                        enabled: true,
                        max_touch_points: Some(1),
                    })
                    .map_err(|e| web_err("emulating touch", e))?;
                }
            }
            if let Some(ua) = &config.user_agent {
                tab.set_user_agent(ua, None, None)
                    .map_err(|e| web_err("overriding user agent", e))?;
            }
            // A pinned clock (GAP-P): a Date shim injected before any page
            // script runs, plus a CDP timezone override. NOT best-effort - a
            // date-dependent flow that silently ran at wall time would test
            // the wrong thing, so an install failure aborts the flow.
            if let Some(clock) = &config.clock {
                let shim = clock_shim(&clock.at);
                tab.call_method(Page::AddScriptToEvaluateOnNewDocument {
                    source: shim,
                    world_name: None,
                    include_command_line_api: None,
                    run_immediately: None,
                })
                .map_err(|e| web_err("pinning the clock", e))?;
                if let Some(tz) = &clock.timezone {
                    tab.call_method(Emulation::SetTimezoneOverride {
                        timezone_id: tz.clone(),
                    })
                    .map_err(|e| web_err("pinning the timezone", e))?;
                }
            }
            // Pinned randomness, on the clock's terms: injected before any
            // page script, and NOT best-effort. A flow that silently ran
            // against real randomness would enter a value the page never
            // generated and fail somewhere else entirely.
            if let Some(random) = &config.random {
                tab.call_method(Page::AddScriptToEvaluateOnNewDocument {
                    source: random_shim(random.seed),
                    world_name: None,
                    include_command_line_api: None,
                    run_immediately: None,
                })
                .map_err(|e| web_err("pinning randomness", e))?;
            }
        }
        if let Some(session) = self.staged_session.take() {
            self.seed_script = Self::apply_session(&tab, &session, command)?;
        }
        // Network mocks: install interception BEFORE navigation so even the
        // first document's subresources are answerable. Unlike the console
        // listener this is NOT best-effort — a mock that silently failed to
        // install would change what the flow tests.
        if !self.staged_mocks.is_empty() {
            let mocks = std::mem::take(&mut self.staged_mocks);
            tab.enable_fetch(None, None)
                .map_err(|e| web_err("enabling network interception", e))?;
            tab.enable_request_interception(Arc::new(
                move |_transport: Arc<headless_chrome::browser::transport::Transport>,
                      _session: headless_chrome::browser::transport::SessionId,
                      event: headless_chrome::protocol::cdp::Fetch::events::RequestPausedEvent| {
                    use headless_chrome::browser::tab::RequestPausedDecision;
                    use headless_chrome::protocol::cdp::Fetch;
                    let url = &event.params.request.url;
                    let method = event.params.request.method.to_ascii_uppercase();
                    // Mocked responses must carry permissive CORS headers:
                    // the page's origin differs from the mocked host, and a
                    // fulfilled response is still subject to CORS — without
                    // them the fetch rejects and the mock looks dead.
                    let cors = |ct: Option<&str>| {
                        let mut headers = vec![
                            Fetch::HeaderEntry {
                                name: "access-control-allow-origin".into(),
                                value: "*".into(),
                            },
                            Fetch::HeaderEntry {
                                name: "access-control-allow-methods".into(),
                                value: "*".into(),
                            },
                            Fetch::HeaderEntry {
                                name: "access-control-allow-headers".into(),
                                value: "*".into(),
                            },
                        ];
                        if let Some(ct) = ct {
                            headers.push(Fetch::HeaderEntry {
                                name: "content-type".into(),
                                value: ct.to_string(),
                            });
                        }
                        headers
                    };
                    let any_match = mocks.iter().any(|m| url.contains(&m.url_contains));
                    // CORS preflight for a mocked URL: answer it ourselves —
                    // the real host may not even exist.
                    if method == "OPTIONS" && any_match {
                        return RequestPausedDecision::Fulfill(Fetch::FulfillRequest {
                            request_id: event.params.request_id.clone(),
                            response_code: 204,
                            response_headers: Some(cors(None)),
                            binary_response_headers: None,
                            body: None,
                            response_phrase: None,
                        });
                    }
                    let rule = mocks.iter().find(|m| {
                        url.contains(&m.url_contains)
                            && m.method.as_ref().is_none_or(|want| *want == method)
                    });
                    match rule {
                        Some(m) => RequestPausedDecision::Fulfill(Fetch::FulfillRequest {
                            request_id: event.params.request_id.clone(),
                            response_code: u32::from(m.status),
                            response_headers: Some(cors(Some(&m.content_type))),
                            binary_response_headers: None,
                            body: Some(base64_encode(&m.body)),
                            response_phrase: None,
                        }),
                        None => RequestPausedDecision::Continue(None),
                    }
                },
            ))
            .map_err(|e| web_err("installing network mocks", e))?;
        }
        // Console tail: subscribe BEFORE navigation so boot-time errors are
        // captured too. Best-effort — a page without console history still
        // yields a DOM snapshot on failure.
        if tab.enable_log().and_then(|t| t.enable_runtime()).is_ok() {
            let buffer = self.console.clone();
            let listener = move |event: &headless_chrome::protocol::cdp::types::Event| {
                use headless_chrome::protocol::cdp::types::Event;
                let line = match event {
                    Event::LogEntryAdded(e) => Some(format!(
                        "[{:?}] {}",
                        e.params.entry.level, e.params.entry.text
                    )),
                    Event::RuntimeExceptionThrown(e) => {
                        Some(format!("[exception] {}", e.params.exception_details.text))
                    }
                    _ => None,
                };
                if let Some(line) = line {
                    let mut buf = buffer.lock().unwrap_or_else(|e| e.into_inner());
                    if buf.len() >= CONSOLE_TAIL_CAP {
                        buf.pop_front();
                    }
                    buf.push_back(line);
                }
            };
            tab.add_event_listener(Arc::new(listener)).ok();
        }
        // Flow-wide native-dialog handler. `Page.enable` (already called at
        // tab creation) makes `javascriptDialogOpening` fire. The listener
        // runs on the tab's own event thread, so it answers the dialog the
        // instant it opens - a JS dialog blocks JS synchronously, and the
        // main path is meanwhile blocked inside the click that opened it. An
        // ARMED disposition wins for the declared step; everything else is
        // dismissed and recorded as UNEXPECTED so its step fails rather than
        // hangs. Both directions are deterministic.
        {
            // Fresh state per launch: a prior flow's arming must not leak.
            *self.dialogs.lock().unwrap_or_else(|e| e.into_inner()) = DialogState::default();
            let dialogs = self.dialogs.clone();
            let dialog_tab = tab.clone();
            let listener = move |event: &headless_chrome::protocol::cdp::types::Event| {
                use headless_chrome::protocol::cdp::types::Event;
                let Event::PageJavascriptDialogOpening(opening) = event else {
                    return;
                };
                let dialog_type = match opening.params.Type {
                    Page::DialogType::Alert => "alert",
                    Page::DialogType::Confirm => "confirm",
                    Page::DialogType::Prompt => "prompt",
                    Page::DialogType::Beforeunload => "beforeunload",
                };
                let message = opening.params.message.clone();
                // Decide under the lock, then release it BEFORE the CDP call
                // so nothing holds the state mutex across transport I/O.
                let armed = {
                    let mut state = dialogs.lock().unwrap_or_else(|e| e.into_inner());
                    state.armed.take()
                };
                let handle = dialog_tab.get_dialog();
                // Record the outcome BEFORE answering the dialog. Answering
                // unblocks the renderer, which lets the click command that
                // opened the dialog return on the main thread; recording
                // first guarantees the driver sees this outcome by the time
                // that click returns, with no race between the two threads.
                match armed {
                    Some(arm) => {
                        let accept =
                            matches!(arm.disposition, flowproof_driver::DialogDisposition::Accept);
                        let reply = if accept { arm.reply.clone() } else { None };
                        {
                            let mut state = dialogs.lock().unwrap_or_else(|e| e.into_inner());
                            state.fired = Some(flowproof_driver::FiredDialog {
                                dialog_type: dialog_type.to_string(),
                                message,
                                accepted: accept,
                                reply: reply.clone(),
                            });
                        }
                        let _ = if accept {
                            handle.accept(reply)
                        } else {
                            handle.dismiss()
                        };
                    }
                    None => {
                        // Safety net: record the undeclared dialog, then
                        // dismiss it so the page unblocks and its step fails.
                        {
                            let mut state = dialogs.lock().unwrap_or_else(|e| e.into_inner());
                            state.unexpected = Some(flowproof_driver::FiredDialog {
                                dialog_type: dialog_type.to_string(),
                                message,
                                accepted: false,
                                reply: None,
                            });
                        }
                        let _ = handle.dismiss();
                    }
                }
            };
            tab.add_event_listener(Arc::new(listener)).ok();
        }
        // Downloads: enabled on every launch, regardless of whether
        // `browser:` was declared — `Wait until the download completes as
        // <name>` is a step grammar, not gated behind browser setup.
        // `config.downloads_dir` if staged, else a fresh per-launch temp
        // directory this driver owns; either way `Page.setDownloadBehavior`
        // is what makes headless Chrome write the file at all instead of
        // silently discarding it.
        let downloads_dir = staged_browser
            .as_ref()
            .and_then(|config| config.downloads_dir.clone())
            .unwrap_or_else(fresh_downloads_dir);
        std::fs::create_dir_all(&downloads_dir)
            .map_err(|e| web_err("creating the downloads directory", e))?;
        tab.call_method(Page::SetDownloadBehavior {
            behavior: Page::SetDownloadBehaviorBehaviorOption::Allow,
            download_path: Some(downloads_dir.display().to_string()),
        })
        .map_err(|e| web_err("enabling downloads", e))?;
        self.downloads_dir = Some(downloads_dir);
        tab.navigate_to(command)
            .map_err(|e| web_err(&format!("navigating to {command}"), e))?;
        tab.wait_until_navigated()
            .map_err(|e| web_err("waiting for page load", e))?;
        self.tab = Some(tab);
        // The first document has now run the seed. Drop the script so no
        // later navigation re-seeds over what the flow has since changed.
        self.drop_seed_script();
        Ok(())
    }

    fn debug_bundle(&mut self) -> Result<Option<flowproof_driver::DebugBundle>, DriverError> {
        // Best-effort by contract: a half-captured bundle still beats none.
        let dom_html = self
            .tab()
            .ok()
            .and_then(|tab| {
                tab.evaluate("document.documentElement.outerHTML", false)
                    .ok()
            })
            .and_then(|v| v.value)
            .and_then(|v| v.as_str().map(str::to_string));
        let console = self
            .console
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect();
        Ok(Some(flowproof_driver::DebugBundle { dom_html, console }))
    }

    fn element_checked(&mut self, selector: &UiaSelector) -> Result<Option<bool>, DriverError> {
        let locator = Self::locator(selector)?;
        let value = self.with_element(
            &locator,
            &format!("reading checked state of [{selector}]"),
            |element| element.call_js_fn(CHECKED_STATE_JS, vec![], false),
        )?;
        // The helper returns null for "not a checkbox-like control", which
        // is a different answer from false and must survive as one.
        Ok(value.value.and_then(|v| v.as_bool()))
    }

    fn set_checked(&mut self, selector: &UiaSelector, checked: bool) -> Result<(), DriverError> {
        let locator = Self::locator(selector)?;
        let current = self.element_checked(selector)?.ok_or_else(|| {
            DriverError::Browser(format!(
                "[{selector}] is not a checkbox, radio, or switch, so it cannot be checked"
            ))
        })?;
        if current != checked {
            // Click the element the USER would click. On the common MUI
            // shape the input is visually hidden inside a styled wrapper,
            // and clicking a zero-sized input does nothing - so the helper
            // hands back whichever ancestor is actually clickable.
            self.with_element(&locator, &format!("clicking [{selector}]"), |element| {
                element.click().map(|_| ())
            })?;
        }
        // Verify it took: a click that lands on a disabled or intercepted
        // control must fail the step, not pass silently.
        let now = self.element_checked(selector)?;
        if now != Some(checked) {
            return Err(DriverError::Browser(format!(
                "[{selector}] did not become {}: it reads {}",
                if checked { "checked" } else { "unchecked" },
                match now {
                    Some(true) => "checked",
                    Some(false) => "unchecked",
                    None => "not a checkbox",
                }
            )));
        }
        Ok(())
    }

    fn current_url(&mut self) -> Result<String, DriverError> {
        let value = self
            .tab()?
            .evaluate("window.location.href", false)
            .map_err(|e| web_err("reading page url", e))?;
        Ok(value
            .value
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default())
    }
    fn probe_cookie(&mut self, name: &str) -> Result<flowproof_driver::CookieProbe, DriverError> {
        use flowproof_driver::{CookieFacts, CookieProbe};
        let cookies = self
            .tab()?
            .get_cookies()
            .map_err(|e| web_err("reading cookies", e))?;
        match cookies.iter().find(|c| c.name == name) {
            Some(cookie) => Ok(CookieProbe::Found(CookieFacts {
                http_only: cookie.http_only,
                secure: cookie.secure,
                // Session cookies report no expiry; anything with one
                // outlives the browser session.
                persistent: !cookie.session,
            })),
            // Names only. A cookie jar is full of credentials, and the
            // point of naming what IS there is to fix a typo, not to dump
            // the jar.
            None => Ok(CookieProbe::Absent {
                present: cookies.iter().map(|c| c.name.clone()).collect(),
            }),
        }
    }

    fn page_title(&mut self) -> Result<String, DriverError> {
        let value = self
            .tab()?
            .evaluate("document.title", false)
            .map_err(|e| web_err("reading page title", e))?;
        Ok(value
            .value
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default())
    }

    fn surface_text(&mut self) -> Result<String, DriverError> {
        // Visible text PLUS the accessible names of visible elements:
        // icon-only buttons (a command palette, an account menu) exist on
        // the page only as aria-labels, and `page shows` must see them.
        let value = self
            .tab()?
            .evaluate(
                r#"(() => {
                    const text = document.body ? document.body.innerText : '';
                    const names = [];
                    for (const el of document.querySelectorAll('[aria-label]')) {
                        const r = el.getBoundingClientRect();
                        if (r.width > 0 && r.height > 0) {
                            names.push(el.getAttribute('aria-label'));
                        }
                    }
                    return names.length ? text + '\n' + names.join('\n') : text;
                })()"#,
                false,
            )
            .map_err(|e| web_err("reading page text", e))?;
        Ok(value
            .value
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default())
    }

    fn stage_session(&mut self, session: WebSession) -> Result<(), DriverError> {
        self.staged_session = Some(session);
        Ok(())
    }

    fn stage_mocks(&mut self, rules: Vec<flowproof_driver::WebMock>) -> Result<(), DriverError> {
        self.staged_mocks = rules;
        Ok(())
    }

    fn stage_browser(
        &mut self,
        config: flowproof_driver::WebBrowserConfig,
    ) -> Result<(), DriverError> {
        self.staged_browser = Some(config);
        Ok(())
    }

    fn navigate(&mut self, url: &str) -> Result<(), DriverError> {
        let tab = self.tab()?;
        tab.navigate_to(url)
            .map_err(|e| web_err(&format!("navigating to {url}"), e))?;
        tab.wait_until_navigated()
            .map_err(|e| web_err("waiting for page load", e))?;
        Ok(())
    }

    fn reload(&mut self) -> Result<(), DriverError> {
        let tab = self.tab()?;
        tab.reload(false, None)
            .map_err(|e| web_err("reloading the page", e))?;
        tab.wait_until_navigated()
            .map_err(|e| web_err("waiting for reload", e))?;
        Ok(())
    }

    fn element_exists(&mut self, selector: &UiaSelector) -> Result<bool, DriverError> {
        // A framed target lives in another document, so the ordinary
        // locator cannot see it. Answering `false` here would be a lie
        // that reads as "absent" - the probe answers from inside the
        // frame, and a cross-origin frame is an error, not a `false`.
        if let Some(query) = &selector.frame {
            return match self.probe_frame(query)? {
                flowproof_driver::FrameProbe::Ready { present, .. } => Ok(present),
                flowproof_driver::FrameProbe::NoFrame { .. } => Ok(false),
                flowproof_driver::FrameProbe::CrossOrigin => Err(cross_origin(&query.frame)),
            };
        }
        let Some(locator) = Self::locator_of(selector) else {
            return Ok(false); // non-web ladder rungs simply don't match
        };
        // One round trip where the locator allows it; the CDP transport's
        // per-call latency makes the four-call element-handle path the
        // expensive way to learn a boolean.
        if let Some(resolver) = Self::js_resolver(&locator) {
            let value = self
                .tab()?
                .evaluate(&format!("!!({resolver})"), false)
                .map_err(|e| web_err(&format!("probing for [{selector}]"), e))?;
            return Ok(value.value.and_then(|v| v.as_bool()).unwrap_or(false));
        }
        self.exists(&locator)
    }

    fn actionability_gate(&mut self, target: &UiaSelector) -> Result<Option<String>, DriverError> {
        // The enabled → stable → receives-events pass in ONE round trip,
        // including the stability interval (it elapses inside the page).
        // Same questions, same order, same answers as the composed default
        // — which remains the path for the locator shapes the in-page
        // resolver does not cover, and for framed targets.
        let resolver = (target.frame.is_none())
            .then(|| {
                Self::locator_of(target)
                    .as_ref()
                    .and_then(Self::js_resolver)
            })
            .flatten();
        let Some(resolver) = resolver else {
            return flowproof_driver::composed_actionability_gate(self, target);
        };
        let gate_js = format!(
            "(async el => {{
                if (!el) {{ return 'missing'; }}
                if (el.disabled === true || el.getAttribute('aria-disabled') === 'true'
                    || el.closest('fieldset[disabled]')) {{ return 'disabled'; }}
                const a = el.getBoundingClientRect();
                await new Promise(tick => setTimeout(tick, {interval}));
                const b = el.getBoundingClientRect();
                if (a.x !== b.x || a.y !== b.y || a.width !== b.width
                    || a.height !== b.height) {{ return 'unstable'; }}
                // Scroll first, exactly as the click itself will —
                // elementFromPoint outside the viewport returns null, and an
                // element below the fold must not read as obscured.
                if (el.scrollIntoViewIfNeeded) {{ el.scrollIntoViewIfNeeded(); }}
                else {{ el.scrollIntoView({{ block: 'center', inline: 'center' }}); }}
                const r = el.getBoundingClientRect();
                if (r.width === 0 || r.height === 0) {{ return 'obscured'; }}
                const hit = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2);
                return ({forwards}) ? 'ok' : 'obscured';
            }})({resolver})",
            interval = flowproof_driver::STABILITY_INTERVAL.as_millis(),
            forwards = FORWARDS_CLICK_JS,
        );
        let value = self
            .tab()?
            .evaluate(&gate_js, true)
            .map_err(|e| web_err(&format!("actionability of [{target}]"), e))?;
        let verdict = value.value.and_then(|v| v.as_str().map(str::to_string));
        Ok(match verdict.as_deref() {
            Some("ok") => None,
            Some("disabled") => Some("disabled".into()),
            Some("unstable") => Some("unstable (still moving/animating)".into()),
            Some("obscured") => Some("obscured (another element would receive the click)".into()),
            // The target resolved a moment ago and is gone now: keep the
            // gate polling — a re-render usually brings it back.
            Some("missing") => Some("no longer present (was removed from the DOM)".into()),
            other => {
                return Err(DriverError::Browser(format!(
                    "actionability of [{target}]: unexpected gate answer {other:?}"
                )))
            }
        })
    }

    fn element_enabled(&mut self, selector: &UiaSelector) -> Result<bool, DriverError> {
        // A framed target is not reachable through the ordinary locator.
        // The actionable gate is answered inside the frame instead - and
        // it is the SAME question `FRAME_ACT` refuses on, so a disabled
        // framed control is caught here rather than written into.
        if let Some(query) = &selector.frame {
            let query = query.clone();
            return match self.frame_act(&query, "enabled", serde_json::Value::Null) {
                Ok(status) => Ok(status == "enabled"),
                // `disabled`/`readonly` are answers, not faults.
                Err(DriverError::Browser(m)) if m.contains("is disabled") => Ok(false),
                Err(DriverError::Browser(m)) if m.contains("read-only") => Ok(false),
                Err(e) => Err(e),
            };
        }
        let locator = Self::locator(selector)?;
        let value = self.with_element(
            &locator,
            &format!("reading enabled state of [{selector}]"),
            |element| {
                element.call_js_fn(
                    r#"function() {
                        if (this.disabled === true) { return false; }
                        if (this.getAttribute('aria-disabled') === 'true') { return false; }
                        return !this.closest('fieldset[disabled]');
                    }"#,
                    vec![],
                    false,
                )
            },
        )?;
        Ok(value.value.and_then(|v| v.as_bool()).unwrap_or(true))
    }

    fn element_visible(&mut self, selector: &UiaSelector) -> Result<Option<bool>, DriverError> {
        let locator = Self::locator(selector)?;
        let value = self.with_element(
            &locator,
            &format!("reading visibility of [{selector}]"),
            |element| {
                element.call_js_fn(
                    // `checkVisibility` is the browser's own answer and
                    // covers the cases a hand-rolled check keeps missing:
                    // `display:none` on an ancestor, `visibility:hidden`,
                    // `content-visibility`, and the `hidden` attribute. The
                    // box test behind it catches the other family - an
                    // element that is "visible" by style yet occupies no
                    // space at all, which no user can see or click.
                    r#"function() {
                        if (typeof this.checkVisibility === 'function'
                            && !this.checkVisibility({
                                contentVisibilityAuto: true,
                                opacityProperty: true,
                                visibilityProperty: true
                            })) { return false; }
                        var r = this.getClientRects();
                        return r.length > 0;
                    }"#,
                    vec![],
                    false,
                )
            },
        )?;
        Ok(Some(value.value.and_then(|v| v.as_bool()).unwrap_or(true)))
    }

    fn element_attribute(
        &mut self,
        selector: &UiaSelector,
        name: &str,
    ) -> Result<Option<String>, DriverError> {
        let locator = Self::locator(selector)?;
        // `getAttribute` is ASCII case-insensitive for HTML elements and
        // returns null for a missing attribute (-> None) but "" for a present
        // empty one (`download=""` -> Some("")); that distinction is the whole
        // point of `has attribute` vs a value comparison.
        let value = self.with_element(
            &locator,
            &format!("reading attribute '{name}' of [{selector}]"),
            |element| {
                element.call_js_fn(
                    "function(n) { return this.getAttribute(n); }",
                    vec![serde_json::json!(name)],
                    false,
                )
            },
        )?;
        Ok(value.value.and_then(|v| v.as_str().map(str::to_string)))
    }

    fn element_computed_style(
        &mut self,
        selector: &UiaSelector,
        prop: &str,
    ) -> Result<String, DriverError> {
        let locator = Self::locator(selector)?;
        let value = self.with_element(
            &locator,
            &format!("reading computed style '{prop}' of [{selector}]"),
            |element| {
                element.call_js_fn(
                    "function(p) { return getComputedStyle(this).getPropertyValue(p).trim(); }",
                    vec![serde_json::json!(prop)],
                    false,
                )
            },
        )?;
        Ok(value
            .value
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default())
    }

    fn scroll(&mut self, selector: Option<&UiaSelector>, to: ScrollTo) -> Result<(), DriverError> {
        // A framed scroll goes through the frame's own document, and picks
        // the scrolling element rather than `body` - in standards mode
        // `body.scrollTop` is inert, so the same spelling would silently do
        // nothing one doctype away.
        if let (Some(sel), ScrollTo::Offset(px)) = (selector, to) {
            if let Some(query) = &sel.frame {
                let query = query.clone();
                let status = self.frame_act(&query, "scroll", serde_json::json!(px))?;
                let at: f64 = status
                    .strip_prefix("at:")
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| {
                        DriverError::Browser(format!(
                            "scrolling inside iframe '{}' returned no answer ({status:?})",
                            query.frame
                        ))
                    })?;
                if (at - f64::from(px)).abs() > 1.0 {
                    return Err(DriverError::Browser(format!(
                        "the target inside iframe '{}' did not scroll to {px}px - it is at \
                         {at}px",
                        query.frame
                    )));
                }
                return Ok(());
            }
        }
        // Instant scroll, no settle-wait: the next assertion auto-waits. Every
        // form verifies the scroll took (position reached the edge, or the
        // rect is within the viewport), so a scroll that did nothing fails.
        match selector {
            // The page itself: `Scroll to the top|bottom`.
            None => {
                let to_bottom = matches!(to, ScrollTo::Bottom);
                let ok = self
                    .tab()?
                    .evaluate(
                        &format!(
                            r#"(() => {{
                                const el = document.scrollingElement || document.documentElement;
                                el.scrollTop = {target};
                                const atBottom = el.scrollTop + el.clientHeight >= el.scrollHeight - 2;
                                const atTop = el.scrollTop <= 2;
                                return {which};
                            }})()"#,
                            target = if to_bottom { "el.scrollHeight" } else { "0" },
                            which = if to_bottom { "atBottom" } else { "atTop" },
                        ),
                        false,
                    )
                    .map_err(|e| web_err("scrolling the page", e))?;
                if ok.value.and_then(|v| v.as_bool()) != Some(true) {
                    return Err(DriverError::Browser(
                        "the page did not reach the requested edge".into(),
                    ));
                }
                Ok(())
            }
            Some(sel) => {
                let locator = Self::locator(sel)?;
                match to {
                    // Bring an in-DOM element into the viewport.
                    ScrollTo::IntoView => {
                        let value = self.with_element(
                            &locator,
                            &format!("scrolling [{sel}] into view"),
                            |element| {
                                element.scroll_into_view()?;
                                element.call_js_fn(
                                    r#"function() {
                                        const r = this.getBoundingClientRect();
                                        const vw = window.innerWidth
                                            || document.documentElement.clientWidth;
                                        const vh = window.innerHeight
                                            || document.documentElement.clientHeight;
                                        return r.bottom > 0 && r.right > 0
                                            && r.top < vh && r.left < vw;
                                    }"#,
                                    vec![],
                                    false,
                                )
                            },
                        )?;
                        if value.value.and_then(|v| v.as_bool()) != Some(true) {
                            return Err(DriverError::Browser(format!(
                                "[{sel}] is not in the viewport after scrolling"
                            )));
                        }
                        Ok(())
                    }
                    // Scroll the container to an EXACT offset.
                    ScrollTo::Offset(px) => {
                        let status = self.with_element(
                            &locator,
                            &format!("scrolling [{sel}] to {px}px"),
                            |element| {
                                element.call_js_fn(
                                    // A STATUS STRING, because a JS throw
                                    // does not reach Rust as an Err and a
                                    // bare bool cannot say WHY.
                                    r#"function(px) {
                                        // Address the scrolling element, not
                                        // `body`: in standards mode
                                        // `body.scrollTop` is inert, so the
                                        // same spelling would silently do
                                        // nothing on a page one doctype away.
                                        var el = this;
                                        if (el === (el.ownerDocument.body)
                                            && el.ownerDocument.scrollingElement) {
                                            el = el.ownerDocument.scrollingElement;
                                        }
                                        if (el.scrollHeight <= el.clientHeight) {
                                            return 'not_scrollable';
                                        }
                                        var max = el.scrollHeight - el.clientHeight;
                                        if (px > max) { return 'clamped:' + max; }
                                        // `instant`: a container with
                                        // `scroll-behavior: smooth` animates a
                                        // bare assignment, and the readback
                                        // below would catch it mid-flight.
                                        el.scrollTo({ top: px, behavior: 'instant' });
                                        return 'at:' + el.scrollTop;
                                    }"#,
                                    vec![serde_json::json!(px)],
                                    false,
                                )
                            },
                        )?;
                        let status = status
                            .value
                            .as_ref()
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        if status == "not_scrollable" {
                            return Err(DriverError::Browser(format!(
                                "[{sel}] is not a scroll container (its content fits), so \
                                 scrolling it to {px}px would pass without moving anything"
                            )));
                        }
                        if let Some(max) = status.strip_prefix("clamped:") {
                            return Err(DriverError::Browser(format!(
                                "[{sel}] cannot scroll to {px}px - it stops at {max}px"
                            )));
                        }
                        let at: f64 = status
                            .strip_prefix("at:")
                            .and_then(|v| v.parse().ok())
                            .ok_or_else(|| {
                                DriverError::Browser(format!(
                                    "scrolling [{sel}] returned no answer ({status:?})"
                                ))
                            })?;
                        // A tolerance, not equality: `scrollTop` is
                        // fractional under a non-integer device pixel ratio,
                        // so 147 legitimately reads back as 146.99…
                        if (at - f64::from(px)).abs() > 1.0 {
                            return Err(DriverError::Browser(format!(
                                "[{sel}] did not scroll to {px}px - it is at {at}px"
                            )));
                        }
                        Ok(())
                    }
                    // Scroll the element AS A CONTAINER to an edge.
                    _ => {
                        let to_bottom = matches!(to, ScrollTo::Bottom);
                        let value = self.with_element(
                            &locator,
                            &format!("scrolling [{sel}] to the {}", edge_word(to_bottom)),
                            |element| {
                                element.call_js_fn(
                                    r#"function(toBottom) {
                                        this.scrollTop = toBottom ? this.scrollHeight : 0;
                                        const atBottom = this.scrollTop + this.clientHeight
                                            >= this.scrollHeight - 2;
                                        const atTop = this.scrollTop <= 2;
                                        return toBottom ? atBottom : atTop;
                                    }"#,
                                    vec![serde_json::json!(to_bottom)],
                                    false,
                                )
                            },
                        )?;
                        if value.value.and_then(|v| v.as_bool()) != Some(true) {
                            return Err(DriverError::Browser(format!(
                                "[{sel}] did not reach the {}",
                                edge_word(to_bottom)
                            )));
                        }
                        Ok(())
                    }
                }
            }
        }
    }

    fn element_receives_events(
        &mut self,
        selector: &UiaSelector,
    ) -> Result<Option<bool>, DriverError> {
        // A framed target is in another document, and a framed action never
        // dispatches at a point - this gate exists to protect coordinate
        // clicks. `None` is the honest answer: the driver cannot tell from
        // here, so the gate is satisfied. (The gate that DOES matter for a
        // framed write - disabled/read-only - is answered inside the frame
        // by `element_enabled`.)
        if selector.frame.is_some() {
            return Ok(None);
        }
        let locator = Self::locator(selector)?;
        let value =
            self.with_element(&locator, &format!("hit-testing [{selector}]"), |element| {
                // Scroll first, exactly as the click itself will: headless
                // chrome's `Element::click` begins with `scroll_into_view`,
                // so hit-testing before scrolling asks about a position the
                // click will never use. An element below the fold then reads
                // as "obscured" - `elementFromPoint` outside the viewport
                // returns null - and the gate blocks a click that would have
                // worked. A whole settings form under the fold was
                // untestable this way (field report, round 3).
                element.scroll_into_view()?;
                // Playwright's obscured check: does elementFromPoint at the
                // element's center resolve to it (or a relative)? A toast or
                // modal backdrop on top makes the click land elsewhere.
                element.call_js_fn(
                    r#"function() {
                        const r = this.getBoundingClientRect();
                        if (r.width === 0 || r.height === 0) { return false; }
                        const t = document.elementFromPoint(
                            r.x + r.width / 2, r.y + r.height / 2);
                        if (!t) { return false; }
                        if (t === this || this.contains(t) || t.contains(this)) { return true; }
                        // A custom-styled checkbox or radio: the real input is
                        // visually replaced by a sibling inside its own label,
                        // so the hit is neither ancestor nor descendant. The
                        // browser forwards a click anywhere in the label to the
                        // input, which is exactly how a person ticks the box -
                        // and how the hand-written spec for this same form does
                        // it, by targeting the label instead. Refusing here
                        // makes every styled control unrecordable.
                        //
                        // Forwarded on the BROWSER's terms, not on the mere
                        // presence of a label, because a click the label does
                        // not forward is a click that does nothing - and
                        // recording it as a success is the false green this
                        // gate exists to prevent. Two shapes look identical
                        // from the element and behave nothing alike:
                        //
                        //  - a label labels ONE control (the `for` target,
                        //    else its first labelable descendant), so a label
                        //    wrapping several cannot lend its area to the
                        //    others. `labels` is that relation read from this
                        //    end, which is why it is asked instead of walking
                        //    up to the nearest `<label>`;
                        //  - interactive content inside a label keeps the
                        //    activation for itself. A hit on a link, a button
                        //    or the OTHER input in there follows its own
                        //    behaviour and leaves this control untouched.
                        const label = t.closest('label');
                        const labels = this.labels ? Array.from(this.labels) : [];
                        if (!label || !labels.includes(label)) { return false; }
                        // `area[href]` is interactive content too - it extends
                        // HTMLAnchorElement - and `img[usemap]` never catches it:
                        // elementFromPoint returns the AREA, whose ancestors run
                        // map -> label, so the image is a sibling never visited.
                        const INTERACTIVE = 'a[href], area[href], button, details, \
                            embed, iframe, select, textarea, audio[controls], \
                            video[controls], img[usemap], input:not([type=hidden])';
                        for (let node = t; node && node !== label;
                             node = node.parentElement) {
                            if (node.matches(INTERACTIVE)) { return false; }
                        }
                        return true;
                    }"#,
                    vec![],
                    false,
                )
            })?;
        Ok(value.value.and_then(|v| v.as_bool()))
    }

    fn today(&mut self) -> Result<Option<String>, DriverError> {
        let value = self
            .tab()?
            // Built from local parts rather than `toISOString`: a pinned or
            // faked clock is free to leave that unimplemented, and it would
            // answer in UTC for a page whose own day has already turned.
            .evaluate(
                r#"(() => {
                    const d = new Date();
                    const p = n => String(n).padStart(2, '0');
                    return d.getFullYear() + '-' + p(d.getMonth() + 1) + '-' + p(d.getDate());
                })()"#,
                false,
            )
            .map_err(|e| web_err("reading the page's current date", e))?;
        Ok(value.value.and_then(|v| v.as_str().map(str::to_string)))
    }

    fn occluding_element(&mut self, selector: &UiaSelector) -> Result<Option<String>, DriverError> {
        if selector.frame.is_some() {
            return Ok(None);
        }
        let locator = Self::locator(selector)?;
        let value = self.with_element(
            &locator,
            &format!("naming what covers [{selector}]"),
            |element| {
                element.call_js_fn(
                    r#"function() {
                        const r = this.getBoundingClientRect();
                        const t = document.elementFromPoint(
                            r.x + r.width / 2, r.y + r.height / 2);
                        if (!t || t === this) { return null; }
                        const tag = t.tagName.toLowerCase();
                        if (t.id) { return tag + '#' + t.id; }
                        const cls = Array.from(t.classList).slice(0, 2).join('.');
                        const text = (t.textContent || '').trim().slice(0, 40);
                        return cls ? tag + '.' + cls
                            : (text ? tag + ' reading "' + text + '"' : tag);
                    }"#,
                    vec![],
                    false,
                )
            },
        )?;
        Ok(value.value.and_then(|v| v.as_str().map(str::to_string)))
    }

    fn invoke(&mut self, selector: &UiaSelector) -> Result<(), DriverError> {
        let locator = Self::locator(selector)?;
        self.with_element(&locator, &format!("clicking [{selector}]"), |element| {
            element.click().map(|_| ())
        })
    }

    fn set_files(&mut self, selector: &UiaSelector, paths: &[String]) -> Result<(), DriverError> {
        // Absolute paths: Chrome resolves DOM.setFileInputFiles against ITS
        // working directory, not ours — canonicalize (which also fails
        // loudly on a missing file, before the step "passes" emptily).
        let mut absolute = Vec::with_capacity(paths.len());
        for path in paths {
            let canonical = std::fs::canonicalize(path)
                .map_err(|e| web_err(&format!("upload file '{path}'"), e))?;
            absolute.push(canonical.to_string_lossy().into_owned());
        }
        let locator = Self::locator(selector)?;
        self.with_element(
            &locator,
            &format!("setting files on [{selector}]"),
            |element| {
                let refs: Vec<&str> = absolute.iter().map(String::as_str).collect();
                element.set_input_files(&refs).map(|_| ())
            },
        )
    }

    /// Wait for exactly one NEW file to land in this launch's downloads
    /// directory and finish writing. A snapshot taken at call time is the
    /// baseline — a file already sitting in the directory (a prior
    /// download, in a pinned `downloads_dir` reused across launches) is not
    /// this call's answer, so a flow with two downloads in sequence gets
    /// the second one on the second call rather than the first one twice.
    fn wait_for_download(&mut self, timeout: Duration) -> Result<std::path::PathBuf, DriverError> {
        let dir = self
            .downloads_dir
            .clone()
            .ok_or_else(|| DriverError::Browser("no page open: call launch first".into()))?;
        let baseline = list_finished_downloads(&dir)?;
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let fresh: Vec<_> = list_finished_downloads(&dir)?
                .into_iter()
                .filter(|p| !baseline.contains(p))
                .collect();
            match fresh.as_slice() {
                [one] if is_size_stable(one) => return Ok(one.clone()),
                [_, _, ..] => {
                    return Err(DriverError::Browser(format!(
                        "wait for download: {} files landed at once - only one download was expected",
                        fresh.len()
                    )));
                }
                _ => {}
            }
            if std::time::Instant::now() >= deadline {
                return Err(DriverError::Browser(format!(
                    "wait for download: no download landed in {} within {timeout:?}",
                    dir.display()
                )));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn context_click(&mut self, selector: &UiaSelector) -> Result<(), DriverError> {
        let locator = Self::locator(selector)?;
        let tab = self.tab()?.clone();
        self.with_element(
            &locator,
            &format!("right-clicking [{selector}]"),
            |element| {
                element.scroll_into_view()?;
                let point = element.get_midpoint()?;
                for kind in [
                    Input::DispatchMouseEventTypeOption::MousePressed,
                    Input::DispatchMouseEventTypeOption::MouseReleased,
                ] {
                    tab.call_method(Input::DispatchMouseEvent {
                        Type: kind,
                        x: point.x,
                        y: point.y,
                        button: Some(Input::MouseButton::Right),
                        click_count: Some(1),
                        modifiers: None,
                        timestamp: None,
                        buttons: None,
                        force: None,
                        tangential_pressure: None,
                        tilt_x: None,
                        tilt_y: None,
                        twist: None,
                        delta_x: None,
                        delta_y: None,
                        pointer_Type: None,
                    })?;
                }
                Ok(())
            },
        )
    }

    fn double_click(&mut self, selector: &UiaSelector) -> Result<(), DriverError> {
        let locator = Self::locator(selector)?;
        let tab = self.tab()?.clone();
        self.with_element(
            &locator,
            &format!("double-clicking [{selector}]"),
            |element| {
                element.scroll_into_view()?;
                let point = element.get_midpoint()?;
                // A real `dblclick` is two full press/release pairs at the
                // same point; Chromium raises the DOM `dblclick` on the
                // SECOND release when its click_count reaches 2. Mirrors the
                // context_click CDP shape, left button, click counts 1,1,2,2.
                for (kind, click_count) in [
                    (Input::DispatchMouseEventTypeOption::MousePressed, 1),
                    (Input::DispatchMouseEventTypeOption::MouseReleased, 1),
                    (Input::DispatchMouseEventTypeOption::MousePressed, 2),
                    (Input::DispatchMouseEventTypeOption::MouseReleased, 2),
                ] {
                    tab.call_method(Input::DispatchMouseEvent {
                        Type: kind,
                        x: point.x,
                        y: point.y,
                        button: Some(Input::MouseButton::Left),
                        click_count: Some(click_count),
                        modifiers: None,
                        timestamp: None,
                        buttons: None,
                        force: None,
                        tangential_pressure: None,
                        tilt_x: None,
                        tilt_y: None,
                        twist: None,
                        delta_x: None,
                        delta_y: None,
                        pointer_Type: None,
                    })?;
                }
                Ok(())
            },
        )
    }

    fn hover(&mut self, selector: &UiaSelector) -> Result<(), DriverError> {
        let locator = Self::locator(selector)?;
        let tab = self.tab()?.clone();
        // The closure returns the `:hover` self-check so the caller can turn
        // a false into a clear DriverError, exactly as `scroll` turns a
        // failed edge-check into one. A bare "dispatch succeeded" would pass
        // even when the move landed on an occluder, so hover VERIFIES.
        let hovered =
            self.with_element(&locator, &format!("hovering [{selector}]"), |element| {
                element.scroll_into_view()?;
                // NOT-OBSCURED gate: hovering an obscured element is
                // meaningless because the occluder receives the `mouseover`,
                // not our target. Same Playwright-style `elementFromPoint`
                // check the click path uses.
                let visible = element.call_js_fn(
                    r#"function() {
                    const r = this.getBoundingClientRect();
                    if (r.width === 0 || r.height === 0) { return false; }
                    const t = document.elementFromPoint(
                        r.x + r.width / 2, r.y + r.height / 2);
                    return !!(t && (t === this || this.contains(t) || t.contains(this)));
                }"#,
                    vec![],
                    false,
                )?;
                if visible.value.and_then(|v| v.as_bool()) != Some(true) {
                    anyhow::bail!("the element is obscured; a hover would land on the occluder");
                }
                let point = element.get_midpoint()?;
                // A single `mouseMoved` at the element's center: no
                // press/release, and no synthesized intermediate moves. The
                // browser derives `mouseover`/`mouseenter` itself, and the
                // hover state PERSISTS until the author's next explicit
                // pointer action, because nothing else moves the pointer.
                tab.call_method(Input::DispatchMouseEvent {
                    Type: Input::DispatchMouseEventTypeOption::MouseMoved,
                    x: point.x,
                    y: point.y,
                    button: None,
                    click_count: None,
                    modifiers: None,
                    timestamp: None,
                    buttons: None,
                    force: None,
                    tangential_pressure: None,
                    tilt_x: None,
                    tilt_y: None,
                    twist: None,
                    delta_x: None,
                    delta_y: None,
                    pointer_Type: None,
                })?;
                // THE VERIFY: `:hover` is true only if the hit test at the pointer
                // landed on this element or a descendant. Mirrors how `scroll`
                // proves its effect took, instead of trusting the dispatch.
                element.call_js_fn(
                    "function() { return this.matches(':hover'); }",
                    vec![],
                    false,
                )
            })?;
        if hovered.value.and_then(|v| v.as_bool()) != Some(true) {
            return Err(DriverError::Browser(format!(
                "[{selector}] is not hovered after the pointer move (the hit test landed elsewhere)"
            )));
        }
        Ok(())
    }

    fn arm_dialog(&mut self, arm: flowproof_driver::DialogArm) -> Result<(), DriverError> {
        // The flow-wide listener is installed at launch; arming just hands it
        // the one-shot disposition for the next trigger. A stale record from
        // an earlier step is cleared so this step's verify reads only its own.
        let mut state = self.dialogs.lock().unwrap_or_else(|e| e.into_inner());
        state.fired = None;
        state.armed = Some(arm);
        Ok(())
    }

    fn take_fired_dialog(&mut self) -> Option<flowproof_driver::FiredDialog> {
        let mut state = self.dialogs.lock().unwrap_or_else(|e| e.into_inner());
        // Disarm too: a declared dialog that never fired must not linger and
        // swallow a later, undeclared one that the safety net should catch.
        state.armed = None;
        state.fired.take()
    }

    fn take_unexpected_dialog(&mut self) -> Option<flowproof_driver::FiredDialog> {
        let mut state = self.dialogs.lock().unwrap_or_else(|e| e.into_inner());
        state.unexpected.take()
    }

    /// Probe a same-origin iframe: resolve the frame, then look the inner
    /// target up INSIDE its document. The three states come back distinct so
    /// the caller can auto-wait on a frame that has not rendered yet, stop
    /// hard on a cross-origin one, and never mistake either for "absent".
    fn probe_frame(
        &mut self,
        query: &flowproof_driver::FrameQuery,
    ) -> Result<flowproof_driver::FrameProbe, DriverError> {
        use flowproof_driver::FrameProbe;
        let js = |v: &Option<String>| {
            v.as_deref()
                .map(|x| serde_json::Value::from(x).to_string())
                .unwrap_or_else(|| "null".into())
        };
        let call = format!(
            "({FRAME_PROBE})({frame},{css},{id},{text})",
            frame = serde_json::Value::from(query.frame.as_str()),
            css = js(&query.inner_css),
            id = js(&query.inner_id),
            text = js(&query.inner_text),
        );
        let status = self
            .tab()?
            .evaluate(&call, false)
            .map_err(|e| web_err("probing an iframe", e))?
            .value
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        if status == "cross_origin" {
            return Ok(FrameProbe::CrossOrigin);
        }
        if let Some(names) = status.strip_prefix("no_frame:") {
            let available: Vec<String> = serde_json::from_str(names).unwrap_or_default();
            return Ok(FrameProbe::NoFrame { available });
        }
        let Some(payload) = status.strip_prefix("ok:") else {
            return Err(DriverError::Browser(format!(
                "could not probe iframe '{}' ({status})",
                query.frame
            )));
        };
        let parsed: serde_json::Value = serde_json::from_str(payload)
            .map_err(|e| DriverError::Browser(format!("malformed iframe probe: {e}")))?;
        Ok(FrameProbe::Ready {
            present: parsed
                .get("present")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            text: parsed
                .get("text")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
        })
    }

    fn read_text(&mut self, selector: &UiaSelector) -> Result<String, DriverError> {
        if let Some(query) = &selector.frame {
            return match self.probe_frame(query)? {
                flowproof_driver::FrameProbe::Ready {
                    present: true,
                    text,
                } => Ok(text),
                flowproof_driver::FrameProbe::Ready { present: false, .. } => {
                    Err(DriverError::Browser(format!(
                        "the target was not found inside iframe '{}'",
                        query.frame
                    )))
                }
                probe @ flowproof_driver::FrameProbe::NoFrame { .. } => Err(DriverError::Browser(
                    flowproof_driver::frame_miss(&query.frame, &probe),
                )),
                flowproof_driver::FrameProbe::CrossOrigin => Err(cross_origin(&query.frame)),
            };
        }
        let locator = Self::locator(selector)?;
        // Inner text covers most elements; inputs expose their VALUE — the
        // text a user sees in the box (Playwright's toHaveValue reading).
        let value = self.with_element(
            &locator,
            &format!("reading text of [{selector}]"),
            |element| {
                element.call_js_fn(
                    r#"function() {
                        const tag = this.tagName;
                        if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') {
                            return this.value;
                        }
                        return this.innerText !== undefined ? this.innerText : (this.textContent || '');
                    }"#,
                    vec![],
                    false,
                )
            },
        )?;
        Ok(value
            .value
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default())
    }

    fn type_text(&mut self, selector: &UiaSelector, text: &str) -> Result<(), DriverError> {
        // A framed target lives in another document. Driven through the
        // frame's own DOM, with the guards `FRAME_ACT` documents - NOT the
        // trusted-keystroke path the main document uses, which is a real
        // difference and is stated in docs/authoring.md.
        if let Some(query) = &selector.frame {
            let query = query.clone();
            self.frame_act(&query, "type", serde_json::Value::from(text))?;
            return Ok(());
        }
        let locator = Self::locator(selector)?;
        // A native <select> cannot be committed by clicks or keystrokes in
        // headless Chromium (and a coordinate click never fires React's
        // onChange). Committing a value IS a property set + events: match
        // an option by value, then visible text, set through the native
        // setter, and fire input+change like a user's selection would.
        // A STATUS STRING, not a throw and not a bare boolean. A JS
        // exception does not reach Rust as an `Err` here, so throwing on a
        // missing option looked identical to "this is not a <select>" - and
        // fell through to typing the option's name into the dropdown, which
        // keyboard-selects by prefix and lands on whatever starts with the
        // same letters. A wrong option, selected quietly.
        //
        // The two cases are now different answers: `not_select` is the
        // genuine fall-through to typing, `no_option` is a failure.
        const SELECT_COMMIT_JS: &str = r#"function(wanted) {
            if (this.tagName !== 'SELECT') { return 'not_select'; }
            const w = String(wanted).trim();
            const options = Array.from(this.options);
            const match = options.find(o => o.value === w)
                || options.find(o => o.textContent.trim() === w)
                || options.find(o => o.textContent.trim().startsWith(w));
            if (!match) { return 'no_option:' + w; }
            const desc = Object.getOwnPropertyDescriptor(
                HTMLSelectElement.prototype, 'value');
            if (desc && desc.set) { desc.set.call(this, match.value); }
            else { this.value = match.value; }
            this.dispatchEvent(new Event('input', { bubbles: true }));
            this.dispatchEvent(new Event('change', { bubbles: true }));
            return 'ok';
        }"#;
        // One round trip where the locator allows it — the resolved element
        // becomes `this` exactly as on the element-handle path. A vanished
        // element makes the call throw, which reads as `not_select` and
        // falls through to the typing path's own find-and-retry.
        let status = if let Some(resolver) = Self::js_resolver(&locator) {
            let wanted = serde_json::Value::from(text).to_string();
            self.tab()?
                .evaluate(
                    &format!("({SELECT_COMMIT_JS}).call(({resolver}), {wanted})"),
                    false,
                )
                .map_err(|e| web_err(&format!("selecting in [{selector}]"), e))?
                .value
        } else {
            self.with_element(&locator, &format!("selecting in [{selector}]"), |element| {
                element.call_js_fn(SELECT_COMMIT_JS, vec![serde_json::json!(text)], false)
            })?
            .value
        };
        let status = status
            .as_ref()
            .and_then(|v| v.as_str())
            .unwrap_or("not_select");
        if status == "ok" {
            return Ok(());
        }
        if let Some(wanted) = status.strip_prefix("no_option:") {
            return Err(DriverError::Browser(format!(
                "no option matching '{wanted}' in [{selector}] - the options are matched by \
                 value, then by visible text; nothing was selected"
            )));
        }
        self.with_element(&locator, &format!("typing into [{selector}]"), |element| {
            // Select what the field holds before the keystrokes land: typing
            // over a selection replaces, so the field ends up reading `text`
            // exactly - the trait's fill contract - while the keys stay REAL.
            // An app filtering on keydown sees the same trusted events it
            // always did; setting `.value` directly would not fire them.
            //
            // `select()` exists on input and textarea and throws on nothing
            // else here; contenteditable and other exotics fall through and
            // keep keystroke-append, which is the old behaviour, not a new
            // wrong one.
            // click -> select -> keystrokes, in that order and WITHOUT
            // `type_into`, which clicks again and would collapse the
            // selection back to a caret - the keystrokes would append, which
            // is the accident this contract removes.
            element.click()?;
            element
                .call_js_fn(
                    r#"function() {
                        if (typeof this.select === 'function') { this.select(); }
                    }"#,
                    vec![],
                    false,
                )
                .map(|_| ())?;
            element.parent.type_str(text).map(|_| ())
        })
    }

    fn click_at(
        &mut self,
        selector: &UiaSelector,
        x_pct: f64,
        y_pct: f64,
    ) -> Result<(), DriverError> {
        let locator = Self::locator(selector)?;
        let tab = self.tab()?.clone();
        // Resolve the point IN THE PAGE, and verify the hit test lands on
        // this element before dispatching. A click at an offset can leave
        // the element entirely (a rounded corner, an overlapping sibling),
        // and a click that lands on the occluder while reporting success is
        // the false green `Hover` already guards against the same way.
        let point = self.with_element(
            &locator,
            &format!("locating the click point in [{selector}]"),
            |element| {
                element.scroll_into_view()?;
                let got = element.call_js_fn(
                    r#"function(xp, yp) {
                        const r = this.getBoundingClientRect();
                        if (r.width === 0 || r.height === 0) { return null; }
                        const x = r.x + r.width * xp / 100;
                        const y = r.y + r.height * yp / 100;
                        const hit = document.elementFromPoint(x, y);
                        const mine = !!(hit && (hit === this || this.contains(hit)));
                        return JSON.stringify({ x: x, y: y, mine: mine });
                    }"#,
                    vec![serde_json::json!(x_pct), serde_json::json!(y_pct)],
                    false,
                )?;
                Ok(got.value.and_then(|v| v.as_str().map(str::to_string)))
            },
        )?;
        let point = point.ok_or_else(|| {
            DriverError::Browser(format!("[{selector}] has no box to click inside"))
        })?;
        let parsed: serde_json::Value = serde_json::from_str(&point)
            .map_err(|e| DriverError::Browser(format!("reading the click point: {e}")))?;
        if parsed.get("mine").and_then(|v| v.as_bool()) != Some(true) {
            return Err(DriverError::Browser(format!(
                "{x_pct}%,{y_pct}% of [{selector}] is not on the element - the point lands \
                 on something else, so the click would go to the wrong target"
            )));
        }
        let (x, y) = (
            parsed.get("x").and_then(|v| v.as_f64()).unwrap_or_default(),
            parsed.get("y").and_then(|v| v.as_f64()).unwrap_or_default(),
        );
        let mouse = |kind, button| Input::DispatchMouseEvent {
            Type: kind,
            x,
            y,
            button,
            click_count: Some(1),
            modifiers: None,
            timestamp: None,
            buttons: None,
            force: None,
            tangential_pressure: None,
            tilt_x: None,
            tilt_y: None,
            twist: None,
            delta_x: None,
            delta_y: None,
            pointer_Type: None,
        };
        tab.call_method(mouse(Input::DispatchMouseEventTypeOption::MouseMoved, None))
            .map_err(|e| web_err("moving onto the click point", e))?;
        tab.call_method(mouse(
            Input::DispatchMouseEventTypeOption::MousePressed,
            Some(Input::MouseButton::Left),
        ))
        .map_err(|e| web_err("pressing at the click point", e))?;
        tab.call_method(mouse(
            Input::DispatchMouseEventTypeOption::MouseReleased,
            Some(Input::MouseButton::Left),
        ))
        .map_err(|e| web_err("releasing at the click point", e))?;
        Ok(())
    }

    fn drag(&mut self, from: &UiaSelector, to: &UiaSelector) -> Result<(), DriverError> {
        self.drag_mouse(from, to)
    }

    fn select_options(
        &mut self,
        selector: &UiaSelector,
        values: &[String],
    ) -> Result<(), DriverError> {
        let locator = Self::locator(selector)?;
        let outcome = self.with_element(
            &locator,
            &format!("selecting options in [{selector}]"),
            |element| {
                element.call_js_fn(
                    // The whole selection is set in ONE pass and committed
                    // with ONE input+change pair, because that is what the
                    // app's own handler expects to see: a user finishing a
                    // selection, not four of them.
                    //
                    // Every name is resolved BEFORE anything is selected,
                    // so a typo in the third option leaves the control
                    // untouched rather than half-applied. A step that
                    // failed partway would be worse than one that failed.
                    // Returns a STATUS STRING rather than throwing. A JS
                    // exception does not reach Rust as an `Err` here - the
                    // single-option path quietly falls back to typing when
                    // that happens - so an outcome that must be checked has
                    // to be a value that comes back.
                    r#"function(wanted) {
                        if (this.tagName !== 'SELECT') { return 'not_a_select'; }
                        if (!this.multiple) { return 'not_multiple'; }
                        const options = Array.from(this.options);
                        const pick = (w) => options.find(o => o.value === w)
                            || options.find(o => o.textContent.trim() === w)
                            || options.find(o => o.textContent.trim().startsWith(w));
                        const chosen = [];
                        for (const raw of wanted) {
                            const w = String(raw).trim();
                            const match = pick(w);
                            // Every name is resolved BEFORE anything is
                            // selected, so a typo in the third option
                            // leaves the control untouched rather than
                            // half-applied.
                            if (!match) { return 'no_option:' + w; }
                            chosen.push(match);
                        }
                        // Set-a-state, like Check: what is named becomes
                        // selected and what is not named does not, however
                        // the control arrived.
                        for (const o of options) {
                            o.selected = chosen.indexOf(o) !== -1;
                        }
                        this.dispatchEvent(new Event('input', { bubbles: true }));
                        this.dispatchEvent(new Event('change', { bubbles: true }));
                        // The post-condition, read back from the control
                        // itself: what IS selected now.
                        return 'ok:' + Array.from(this.selectedOptions)
                            .map(o => o.textContent.trim()).join('\u001f');
                    }"#,
                    vec![serde_json::json!(values)],
                    false,
                )
            },
        )?;
        let status = outcome
            .value
            .as_ref()
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let selected = match status.as_str() {
            "not_a_select" => {
                return Err(DriverError::Browser(format!(
                    "[{selector}] is not a <select>, so it has no options to select"
                )))
            }
            "not_multiple" => {
                return Err(DriverError::Browser(format!(
                    "[{selector}] does not allow multiple options - select one option instead"
                )))
            }
            s if s.starts_with("no_option:") => {
                return Err(DriverError::Browser(format!(
                    "no option matching '{}' in [{selector}] - the selection was left untouched",
                    &s["no_option:".len()..]
                )))
            }
            s if s.starts_with("ok:") => s["ok:".len()..].to_string(),
            other => {
                return Err(DriverError::Browser(format!(
                    "selecting options in [{selector}] returned no answer ({other:?})"
                )))
            }
        };
        // Verify the state took, exactly as `Check` does. A control that
        // accepted the assignment and then re-derived its own selection
        // (a framework re-render) must fail here rather than pass on the
        // strength of having been asked.
        let got: Vec<&str> = if selected.is_empty() {
            Vec::new()
        } else {
            selected.split('\u{1f}').collect()
        };
        if got.len() != values.len() {
            return Err(DriverError::Browser(format!(
                "selecting in [{selector}] did not take: asked for {} option(s), \
                 the control now has {} ({})",
                values.len(),
                got.len(),
                got.join(", ")
            )));
        }
        Ok(())
    }

    fn clear_text(&mut self, selector: &UiaSelector) -> Result<(), DriverError> {
        if let Some(query) = &selector.frame {
            let query = query.clone();
            self.frame_act(&query, "clear", serde_json::Value::Null)?;
            return Ok(());
        }
        let locator = Self::locator(selector)?;
        // Go through the native value setter so framework-controlled inputs
        // (React et al.) see the change, then fire the events they listen to.
        self.with_element(&locator, &format!("clearing [{selector}]"), |element| {
            element
                .call_js_fn(
                    r#"function() {
                        this.focus();
                        if ('value' in this) {
                            const proto = this.tagName === 'TEXTAREA'
                                ? HTMLTextAreaElement.prototype
                                : HTMLInputElement.prototype;
                            const setter = Object.getOwnPropertyDescriptor(proto, 'value');
                            if (setter && setter.set) { setter.set.call(this, ''); }
                            else { this.value = ''; }
                        } else {
                            this.textContent = '';
                        }
                        this.dispatchEvent(new Event('input', { bubbles: true }));
                        this.dispatchEvent(new Event('change', { bubbles: true }));
                    }"#,
                    vec![],
                    false,
                )
                .map(|_| ())
        })
    }

    fn type_focused(&mut self, text: &str) -> Result<(), DriverError> {
        self.tab()?
            .type_str(text)
            .map_err(|e| web_err("typing into the focused element", e))?;
        Ok(())
    }

    fn press_key(&mut self, key: &str, modifiers: &[KeyMod]) -> Result<(), DriverError> {
        let mods: Vec<ModifierKey> = modifiers
            .iter()
            .map(|m| match m {
                KeyMod::Ctrl => ModifierKey::Ctrl,
                KeyMod::Alt => ModifierKey::Alt,
                KeyMod::Shift => ModifierKey::Shift,
                KeyMod::Meta => ModifierKey::Meta,
            })
            .collect();
        self.tab()?
            .press_key_with_modifiers(key, (!mods.is_empty()).then_some(mods.as_slice()))
            .map_err(|e| web_err(&format!("pressing key '{key}'"), e))?;
        Ok(())
    }

    fn screen_size(&mut self) -> Result<(u32, u32), DriverError> {
        // Headless default viewport; good enough for trace metadata.
        Ok((1280, 720))
    }

    fn capture(&mut self) -> Result<Option<image::RgbaImage>, DriverError> {
        let png = self
            .tab()?
            .capture_screenshot(Page::CaptureScreenshotFormatOption::Png, None, None, true)
            .map_err(|e| web_err("capturing screenshot", e))?;
        let frame = image::load_from_memory(&png)
            .map_err(|e| web_err("decoding screenshot", e))?
            .to_rgba8();
        Ok(Some(frame))
    }

    fn element_rect(&mut self, selector: &UiaSelector) -> Result<Option<PixelRect>, DriverError> {
        let Some(locator) = Self::locator_of(selector) else {
            return Ok(None);
        };
        let Some(element) = self.try_find(&locator)? else {
            return Ok(None);
        };
        let quad = element
            .get_box_model()
            .map_err(|e| web_err(&format!("box model of [{selector}]"), e))?
            .content;
        Ok(Some((
            quad.most_left().floor() as i32,
            quad.most_top().floor() as i32,
            quad.width().ceil() as u32,
            quad.height().ceil() as u32,
        )))
    }

    fn scene(&mut self) -> Result<Option<String>, DriverError> {
        // Enumerate visible interactable elements plus readable leaf text —
        // the grounding set an authoring model must choose targets from.
        // Static text matters for natural `Remember the order number` steps,
        // not only for assertions.
        // `target` is the provenance-neutral token the model echoes; the
        // bare `css` key is kept one release for older agents.
        const SCENE_JS: &str = r#"
            (() => {
              function semanticCss(el) {
                if (el.id) return '#' + CSS.escape(el.id);
                for (const attr of ['data-testid', 'data-test', 'data-qa', 'aria-label', 'name']) {
                  const value = el.getAttribute(attr);
                  if (value) {
                    const candidate = el.tagName.toLowerCase() + '[' + attr + '="' +
                      CSS.escape(value) + '"]';
                    if (document.querySelectorAll(candidate).length === 1) return candidate;
                  }
                }
                for (const cls of Array.from(el.classList)) {
                  const candidate = el.tagName.toLowerCase() + '.' + CSS.escape(cls);
                  if (document.querySelectorAll(candidate).length === 1) return candidate;
                }
                return null;
              }
              function cssPath(el) {
                const semantic = semanticCss(el);
                if (semantic) return semantic;
                const parts = [];
                while (el && el.nodeType === 1 && el !== document.body) {
                  const tag = el.tagName.toLowerCase();
                  const siblings = Array.from(el.parentElement.children)
                    .filter(s => s.tagName === el.tagName);
                  parts.unshift(tag + ':nth-of-type(' + (siblings.indexOf(el) + 1) + ')');
                  el = el.parentElement;
                }
                return 'body > ' + parts.join(' > ');
              }
              function compoundCss(el) {
                const classes = Array.from(el.classList);
                if (!classes.length) return null;
                return el.tagName.toLowerCase() + classes.map(cls => '.' + CSS.escape(cls)).join('');
              }
              function relativeCss(el, container) {
                const semantic = semanticCss(el);
                if (semantic && container.querySelectorAll(semantic).length === 1) return semantic;
                const compound = compoundCss(el);
                if (!compound) return null;
                if (container.querySelectorAll(compound).length === 1) return compound;

                // A value cell often shares its base classes with the label
                // beside it, while the label has one extra presentation class
                // (`bg-info`, for example). Excluding that distinguishing
                // class gives the scoped target a stable, non-positional inner
                // selector without smuggling visible text into CSS.
                const extras = new Set();
                for (const other of container.querySelectorAll(compound)) {
                  if (other === el) continue;
                  for (const cls of other.classList) {
                    if (!el.classList.contains(cls)) extras.add(cls);
                  }
                }
                for (const extra of extras) {
                  const candidate = compound + ':not(.' + CSS.escape(extra) + ')';
                  if (container.querySelectorAll(candidate).length === 1) return candidate;
                }
                return null;
              }
              function scopedReadable(el) {
                const container = el.parentElement;
                if (!container) return null;
                const containerCss = semanticCss(container) || compoundCss(container);
                const innerCss = relativeCss(el, container);
                if (!containerCss || !innerCss) return null;

                // Use a neighbouring leaf as the row/card identity. Require
                // that the anchor identifies one matching container on the
                // current screen; runtime scoped resolution will enforce the
                // same relationship again on every replay.
                const siblings = Array.from(container.children);
                const before = siblings.slice(0, siblings.indexOf(el)).reverse();
                const after = siblings.slice(siblings.indexOf(el) + 1);
                for (const anchorEl of before.concat(after)) {
                  const anchor = (anchorEl.textContent || '').trim();
                  if (!anchor || anchor.length > 120) continue;
                  const matching = Array.from(document.querySelectorAll(containerCss)).filter(candidate =>
                    Array.from(candidate.children).some(child =>
                      child !== el && (child.textContent || '').trim() === anchor
                    )
                  );
                  if (matching.length !== 1 || matching[0] !== container) continue;
                  const containerTarget = 'css:' + containerCss;
                  const innerTarget = 'css:' + innerCss;
                  return {
                    token: 'scoped:' + containerTarget + ' containing ' + JSON.stringify(anchor) +
                      ' > ' + innerTarget,
                    container: containerTarget,
                    anchor,
                    inner: innerTarget,
                  };
                }
                return null;
              }
              const all = Array.from(document.querySelectorAll('body *'));
              const styledLeaf = el => el.children.length === 0 &&
                ['DIV', 'SPAN'].includes(el.tagName) &&
                /(?:background|color)\s*:/i.test(el.getAttribute('style') || '');
              // What the PAGE says about a field, in its own words.
              //
              // Validation frameworks mark the WRAPPER, not the control - the
              // control's own class stays empty - so looking only at the
              // element finds nothing and reports a rejected form as a clean
              // one. They also publish the rule they enforced ("Must be a
              // number between 1 and 2000"), which is worth more than the
              // boolean: it lets a value be chosen correctly the first time
              // rather than guessed at after a refusal.
              const MARKED_BAD = /(^|\s)(invalid|error|has-error|is-invalid)(\s|$)/;
              const invalidity = el => {
                let host = el;
                for (let depth = 0; host && depth < 4; depth++) {
                  if (MARKED_BAD.test(host.className || '')) { return true; }
                  host = host.parentElement;
                }
                return false;
              };
              const fieldRule = el => {
                const wrap = el.closest('.field') || el.parentElement;
                if (!wrap) { return null; }
                const msg = wrap.querySelector('.error, .hint, .help, .invalid-feedback');
                if (!msg) { return null; }
                const text = (msg.textContent || '').trim();
                return text ? text.slice(0, 100) : null;
              };
              const isRendered = el => {
                const s = getComputedStyle(el);
                const r = el.getBoundingClientRect();
                return s.display !== 'none' && s.visibility !== 'hidden' &&
                  Number(s.opacity) > 0 && r.width > 0 && r.height > 0;
              };
              // A control the page has hidden and replaced with styling -
              // a custom checkbox, radio, or a price option drawn as a table
              // cell. The control itself is zero-sized, so it never reaches
              // this inventory, and neither does its label when the label is
              // only a wrapper around the same styling. The option then
              // cannot be chosen at all: a model asked to "pick one of the
              // price options" is offered nothing that is one, and reaches
              // for whatever else carries that name.
              //
              // What a person clicks is the nearest thing that IS rendered -
              // the label, else the cell or wrapper around it - and the
              // browser forwards the click to the control. That element
              // stands in for the one that cannot be seen.
              // Interactive content keeps a click for itself, so a host
              // containing any of it cannot speak for the control behind it.
              const CONSUMES_CLICK = 'a[href], area[href], button, details, embed, \
                  iframe, audio[controls], video[controls], img[usemap]';
              const standInHosts = new Set();
              for (const control of document.querySelectorAll('input, select, textarea')) {
                if (control.type === 'hidden' || isRendered(control)) { continue; }
                // A disabled control has NO activation behaviour, so forwarding
                // is a no-op: the click lands, nothing changes, and the step is
                // written down as a success. An out-of-stock option in a styled
                // radio group is exactly this shape.
                if (control.disabled || control.closest('fieldset[disabled]')) { continue; }
                // Only a LABEL forwards a click to its control. A cell or a
                // wrapper div forwards nothing unless the page happens to have
                // its own handler, and recording a click that does nothing is
                // the failure this inventory exists to avoid.
                const host = control.closest('label');
                if (!host || !isRendered(host)) { continue; }
                const labels = control.labels ? Array.from(control.labels) : [];
                if (!labels.includes(host)) { continue; }
                // More than one control under the same label, and activation
                // goes to the first - not necessarily this one.
                if (host.querySelectorAll('input, select, textarea').length !== 1) { continue; }
                if (host.querySelector(CONSUMES_CLICK)) { continue; }
                standInHosts.add(host);
              }
              const interactive = el => el.matches(
                'input, button, a, select, textarea, [role=button], [role=checkbox], [role=radio], [role=menuitem], [draggable], [ondrop], .draggable-row, .droparea'
              ) || (!!el.id && el.children.length === 0 && ['DIV', 'SPAN'].includes(el.tagName)) ||
                styledLeaf(el) || standInHosts.has(el);
              const readableLeaf = el => {
                if (['SCRIPT', 'STYLE', 'NOSCRIPT'].includes(el.tagName)) return false;
                const text = (el.textContent || '').trim();
                return text && !Array.from(el.children).some(child =>
                  (child.textContent || '').trim()
                ) && (semanticCss(el) || scopedReadable(el));
              };
              const ordered = all.filter(interactive).concat(
                all.filter(el => !interactive(el) && readableLeaf(el))
              );
              const seen = new Set();
              const chosen = ordered.filter(el => {
                const r = el.getBoundingClientRect();
                const style = getComputedStyle(el);
                // Authoring inventories the rendered PAGE, not only the
                // current viewport. A user naturally says "enter the date"
                // even when the field starts below the fold, and every web
                // action already scrolls its target into view before acting.
                const rendered = style.display !== 'none' && style.visibility !== 'hidden' &&
                  Number(style.opacity) > 0 && r.width > 0 && r.height > 0;
                if (!rendered || seen.has(el)) return false;
                seen.add(el);
                return true;
              }).slice(0, 100);
              // A step meaning "fill in this form" needs the state of the
              // form, not only its shape: which fields are empty, which the
              // page demands, which boxes are ticked, and — for a dropdown —
              // the exact option strings. Without the options an authoring
              // model guesses a label, and a <select> refuses a name it does
              // not have. A password's value is never reported: the scene
              // travels to a model.
              const fieldValue = el => {
                const tag = el.tagName;
                if (!['INPUT', 'SELECT', 'TEXTAREA'].includes(tag)) return undefined;
                if (tag === 'INPUT' && ['checkbox', 'radio', 'password'].includes(el.type)) {
                  return undefined;
                }
                return (el.value || '').trim().slice(0, 80) || undefined;
              };
              const entries = chosen.map(el => {
                const css = cssPath(el);
                const scoped = !interactive(el) && !semanticCss(el) ? scopedReadable(el) : null;
                const label = el.labels && el.labels[0] ? el.labels[0].textContent.trim()
                    : (el.getAttribute('aria-label') || el.getAttribute('placeholder') || '');
                const ticks = el.tagName === 'INPUT' && ['checkbox', 'radio'].includes(el.type);
                const isField = ['INPUT', 'SELECT', 'TEXTAREA'].includes(el.tagName);
                const entry = {
                    target: scoped ? scoped.token : 'css:' + css,
                    css,
                    tag: el.tagName.toLowerCase(),
                    actionable: interactive(el),
                    type: el.getAttribute('type') || undefined,
                    text: (el.textContent || '').trim().slice(0, 80) || undefined,
                    label: label || undefined,
                    value: fieldValue(el),
                    checked: ticks ? el.checked : undefined,
                    required: el.required || undefined,
                    // Only a CONTROL can be rejected, and only a control can
                    // be corrected. The marker sits on a wrapper, so asking
                    // this of every element also flags the label and the error
                    // message inside it - naming one bad field three times,
                    // two of them things nothing can type into. A model handed
                    // that list spends its one correction on a <span>.
                    invalid: (isField ? invalidity(el) : false) || undefined,
                    rule: (isField ? fieldRule(el) : null) || undefined,
                    options: el.tagName === 'SELECT'
                      ? Array.from(el.options)
                          .map(option => (option.textContent || '').trim())
                          .filter(Boolean)
                          .slice(0, 60)
                      : undefined,
                    background_color: styledLeaf(el) ? getComputedStyle(el).backgroundColor : undefined,
                };
                if (scoped) entry.scope = {
                  container: scoped.container,
                  anchor: scoped.anchor,
                  inner: scoped.inner,
                };
                return entry;
              });

              // A human can ask for the number of displayed rows or the
              // final cell without naming one current row. Represent those
              // collection identities directly so the model can ground the
              // intent without inventing a selector from today's DOM.
              for (const table of document.querySelectorAll('table')) {
                const tableCss = semanticCss(table);
                if (!tableCss) continue;
                const rows = Array.from(table.querySelectorAll('tr'));
                if (rows.length) {
                  const css = tableCss + ' tr';
                  entries.push({
                    target: 'css:' + css,
                    css,
                    tag: 'collection',
                    actionable: false,
                    count: rows.length,
                    label: 'rows in ' + tableCss,
                  });
                }
                const lastCell = table.querySelector('tr:last-child td:last-child');
                if (lastCell) {
                  const css = tableCss + ' tr:last-child td:last-child';
                  entries.push({
                    target: 'css:' + css,
                    css,
                    tag: 'td',
                    actionable: false,
                    text: (lastCell.textContent || '').trim().slice(0, 80) || undefined,
                    label: 'last cell in the final row of ' + tableCss,
                  });
                }
              }

              // Same-origin iframe values are a normal part of the live
              // scene. Their synthetic token is translated to Target::Framed
              // before tracing, just like a scoped row token; no synthetic
              // authoring syntax is persisted or shown to the human.
              for (const frameEl of document.querySelectorAll('iframe, frame')) {
                const frameRect = frameEl.getBoundingClientRect();
                const frameStyle = getComputedStyle(frameEl);
                if (frameStyle.display === 'none' || frameStyle.visibility === 'hidden' ||
                    Number(frameStyle.opacity) <= 0 || frameRect.width <= 0 || frameRect.height <= 0) {
                  continue;
                }
                const frame = frameEl.getAttribute('title') || frameEl.getAttribute('name') ||
                  frameEl.id || frameEl.getAttribute('aria-label');
                if (!frame) continue;
                let frameDoc;
                try { frameDoc = frameEl.contentDocument; } catch (_) { continue; }
                if (!frameDoc || !frameDoc.body) continue;
                const inner = [frameDoc.body].concat(Array.from(frameDoc.querySelectorAll(
                  'input, button, select, textarea, a, [role=button], [role=checkbox], [role=radio], [role=menuitem], [id]'
                )));
                for (const el of inner) {
                  const style = frameDoc.defaultView.getComputedStyle(el);
                  const rect = el.getBoundingClientRect();
                  if (style.display === 'none' || style.visibility === 'hidden' ||
                      Number(style.opacity) <= 0 || rect.width <= 0 || rect.height <= 0) {
                    continue;
                  }
                  let css = el === frameDoc.body ? 'body' : null;
                  if (!css && el.id) css = '#' + CSS.escape(el.id);
                  if (!css) {
                    for (const attr of ['data-testid', 'data-test', 'data-qa', 'aria-label', 'name']) {
                      const value = el.getAttribute(attr);
                      if (!value) continue;
                      const candidate = el.tagName.toLowerCase() + '[' + attr + '="' +
                        CSS.escape(value) + '"]';
                      if (frameDoc.querySelectorAll(candidate).length === 1) {
                        css = candidate;
                        break;
                      }
                    }
                  }
                  if (!css) continue;
                  const innerTarget = 'css:' + css;
                  const token = 'framed:' + JSON.stringify(frame) + ' > ' + innerTarget;
                  const actionable = el.matches(
                    'input, button, select, textarea, a, [role=button], [role=checkbox], [role=radio], [role=menuitem]'
                  );
                  const label = el.labels && el.labels[0] ? el.labels[0].textContent.trim()
                    : (el.getAttribute('aria-label') || el.getAttribute('placeholder') || '');
                  entries.push({
                    target: token,
                    css,
                    tag: el.tagName.toLowerCase(),
                    actionable,
                    text: el === frameDoc.body ? undefined
                      : (el.value || el.textContent || '').trim().slice(0, 80) || undefined,
                    label: label || (el === frameDoc.body ? 'scroll surface' : undefined),
                    scope: { frame, inner: innerTarget },
                  });
                }
              }
              return JSON.stringify({
                ready: document.readyState === 'complete',
                entries,
              });
            })()
        "#;
        let sample = || {
            let value = self
                .tab()?
                .evaluate(SCENE_JS, false)
                .map_err(|e| web_err("evaluating scene script", e))?;
            let raw = value
                .value
                .and_then(|v| v.as_str().map(str::to_string))
                .ok_or_else(|| DriverError::Browser("scene script returned no value".into()))?;
            let parsed: serde_json::Value = serde_json::from_str(&raw)
                .map_err(|e| DriverError::Browser(format!("scene script returned no JSON: {e}")))?;
            Ok(SceneSample {
                ready: parsed["ready"].as_bool().unwrap_or(true),
                entries: parsed["entries"].as_array().cloned().unwrap_or_default(),
            })
        };
        let entries = settled_scene(
            sample,
            || std::thread::sleep(SCENE_SETTLE_INTERVAL),
            SCENE_SETTLE_ROUNDS,
        )?;
        let json = serde_json::to_string(&entries)
            .map_err(|e| DriverError::Browser(format!("re-serialising scene: {e}")))?;
        Ok(Some(json))
    }

    fn password_rects(&mut self) -> Result<Vec<PixelRect>, DriverError> {
        let tab = self.tab()?;
        let fields = tab
            .find_elements("input[type=password]")
            .unwrap_or_default();
        let mut rects = Vec::new();
        for field in fields {
            let quad = field
                .get_box_model()
                .map_err(|e| web_err("box model of password field", e))?
                .content;
            rects.push((
                quad.most_left().floor() as i32,
                quad.most_top().floor() as i32,
                quad.width().ceil() as u32,
                quad.height().ceil() as u32,
            ));
        }
        Ok(rects)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use flowproof_driver::DriverError;

    struct VisibleWindowProbe {
        calls: RefCell<Vec<&'static str>>,
    }

    impl super::HeadedTabWindow for VisibleWindowProbe {
        fn maximize(&self) -> Result<(), DriverError> {
            self.calls.borrow_mut().push("maximize");
            Ok(())
        }

        fn foreground(&self) -> Result<(), DriverError> {
            self.calls.borrow_mut().push("foreground");
            Ok(())
        }
    }

    fn sample(ready: bool, targets: &[&str]) -> super::SceneSample {
        super::SceneSample {
            ready,
            entries: targets
                .iter()
                .map(|target| serde_json::json!({ "target": target, "actionable": true }))
                .collect(),
        }
    }

    fn targets_of(entries: &[serde_json::Value]) -> Vec<String> {
        super::scene_shape(entries)
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    /// The Tricentis failure this exists to prevent. `#get_truck` navigates to
    /// a page that renders every vehicle form and hides the irrelevant ones in
    /// script. Read too early, the inventory is the union — a motorcycle
    /// `#model` beside the truck's `#payload` — and a model that grounds on
    /// `#model` is then blamed for choosing a control that was genuinely
    /// listed. The scene must be the settled form, not the union.
    #[test]
    fn union_of_forms_mid_transition_is_never_returned() {
        let readings = RefCell::new(vec![
            sample(false, &["css:#make", "css:#model", "css:#payload"]),
            sample(true, &["css:#make", "css:#model", "css:#payload"]),
            sample(true, &["css:#make", "css:#payload"]),
            sample(true, &["css:#make", "css:#payload"]),
        ]);
        let mut pauses = 0;
        let entries =
            super::settled_scene(|| Ok(readings.borrow_mut().remove(0)), || pauses += 1, 20)
                .expect("settles");
        assert_eq!(targets_of(&entries), vec!["css:#make", "css:#payload"]);
    }

    /// Two readings that agree while the document is still loading prove
    /// nothing — that is exactly the window in which the pre-script union
    /// looks stable.
    #[test]
    fn agreement_before_load_completes_does_not_count_as_settled() {
        let readings = RefCell::new(vec![
            sample(false, &["css:#model"]),
            sample(false, &["css:#model"]),
            sample(true, &["css:#payload"]),
            sample(true, &["css:#payload"]),
        ]);
        let entries = super::settled_scene(|| Ok(readings.borrow_mut().remove(0)), || {}, 20)
            .expect("settles");
        assert_eq!(targets_of(&entries), vec!["css:#payload"]);
    }

    /// A page that never goes quiet must still yield a scene. Recording it is
    /// better than hanging on a carousel that will rotate forever.
    #[test]
    fn a_page_that_never_settles_returns_its_newest_reading_within_the_bound() {
        let round = std::cell::Cell::new(0usize);
        let mut pauses = 0;
        let entries = super::settled_scene(
            || {
                let n = round.get();
                round.set(n + 1);
                // A carousel: one slide's target replaced on every reading, so
                // no two consecutive shapes ever agree.
                Ok(super::SceneSample {
                    ready: true,
                    entries: vec![
                        serde_json::json!({"target": "css:#make"}),
                        serde_json::json!({"target": format!("css:#slide{n}")}),
                    ],
                })
            },
            || pauses += 1,
            3,
        )
        .expect("gives up cleanly");
        assert_eq!(pauses, 3, "bounded by the round count, not by the page");
        assert_eq!(targets_of(&entries), vec!["css:#make", "css:#slide3"]);
    }

    /// Text and values churn on a live page without changing which elements
    /// exist. Waiting for those to hold still would wait forever, so the
    /// shape deliberately ignores them.
    #[test]
    fn churning_text_does_not_prevent_settling() {
        let readings = RefCell::new(vec![
            super::SceneSample {
                ready: true,
                entries: vec![serde_json::json!({"target": "css:#clock", "text": "12:00:01"})],
            },
            super::SceneSample {
                ready: true,
                entries: vec![serde_json::json!({"target": "css:#clock", "text": "12:00:02"})],
            },
        ]);
        let mut pauses = 0;
        let entries =
            super::settled_scene(|| Ok(readings.borrow_mut().remove(0)), || pauses += 1, 20)
                .expect("settles");
        assert_eq!(pauses, 1, "settles on the first comparison");
        assert_eq!(entries[0]["text"], "12:00:02");
    }

    /// The single-round-trip probes exist because the CDP transport pays a
    /// fixed latency per call; this pins WHICH locator shapes they cover.
    /// A cell or scoped locator must return `None` — their resolution
    /// (tag-then-find, ambiguity as a hard error) lives on the
    /// element-handle path, and a JS mirror that silently diverged would
    /// resolve a different element than the action then operates on.
    #[test]
    fn js_resolver_covers_css_and_text_and_declines_cell_and_scope() {
        let css = super::WebLocator {
            css: Some("#order".into()),
            text: None,
            nth: Some(2),
            cell: None,
            scope: None,
        };
        let resolver = super::WebAppDriver::js_resolver(&css).expect("css is fast-path");
        assert!(resolver.contains("\"#order\""));
        assert!(resolver.contains("querySelectorAll"), "nth needs the list");

        let text = super::WebLocator {
            css: None,
            text: Some("Greet".into()),
            nth: None,
            cell: None,
            scope: None,
        };
        let resolver = super::WebAppDriver::js_resolver(&text).expect("text is fast-path");
        // The SAME xpath ladder the element-handle path walks.
        for xpath in super::text_xpaths("Greet") {
            assert!(
                resolver.contains(&serde_json::Value::from(xpath.as_str()).to_string()),
                "every rung travels into the page"
            );
        }

        let cell = super::WebLocator {
            css: None,
            text: None,
            nth: None,
            cell: Some(flowproof_driver::CellQuery {
                column: "Total".into(),
                anchor: "Order 7".into(),
                also: Vec::new(),
                column_field: None,
                row_id: None,
            }),
            scope: None,
        };
        assert!(
            super::WebAppDriver::js_resolver(&cell).is_none(),
            "cells stay on the element-handle path"
        );
    }

    #[test]
    fn visible_flow_window_is_foregrounded_at_launch() {
        let headed = VisibleWindowProbe {
            calls: RefCell::new(Vec::new()),
        };
        super::present_headed_window(&headed, true).expect("present visible window");
        assert_eq!(*headed.calls.borrow(), ["maximize", "foreground"]);

        let headless = VisibleWindowProbe {
            calls: RefCell::new(Vec::new()),
        };
        super::present_headed_window(&headless, false).expect("headless is a no-op");
        assert!(headless.calls.borrow().is_empty());
    }

    /// The wiring, without starting a browser: `headed` must reach
    /// `LaunchOptions.headless` inverted. Asserting on the built options rather
    /// than on the boolean that produced them is the point — the previous code
    /// passed a literal `true`, and a test of the literal would have proven
    /// nothing.
    #[test]
    fn headed_flips_the_headless_launch_option() {
        let no_args: Vec<std::ffi::OsString> = Vec::new();

        let headless = super::launch_options_for(&no_args, false).expect("options build");
        assert!(headless.headless, "default must stay headless");

        let headed = super::launch_options_for(&no_args, true).expect("options build");
        assert!(!headed.headless, "FLOWPROOF_HEADED must show the window");
    }

    /// A visible shared browser leaks its keep-alive window into the user's
    /// desktop after the flow tab closes. Headless suites retain reuse; either
    /// explicit opt-out or headed mode must select one privately-owned process.
    #[test]
    fn headed_runs_never_use_the_shared_keep_alive_browser() {
        assert!(super::should_share_browser(false, false));
        assert!(!super::should_share_browser(true, false));
        assert!(!super::should_share_browser(false, true));
        assert!(!super::should_share_browser(true, true));
    }

    /// The headed launch failure must name the display, and must not invent one
    /// when headless fails for an unrelated reason.
    ///
    /// The message quoted here is the one actually observed on a display-less
    /// host, not a plausible-looking stand-in.
    #[test]
    fn a_headed_launch_failure_explains_the_missing_display() {
        let real = "There are no available ports between 8000 and 9000 for debugging";

        let headed = super::launch_failure_message(real, true);
        assert!(
            headed.contains(real),
            "the underlying error is still the truth"
        );
        assert!(
            headed.contains("FLOWPROOF_HEADED") && headed.contains("--keep-open"),
            "must name both ways a visible browser can be requested: {headed}"
        );
        assert!(
            headed.contains("desktop session"),
            "must name the actual cause, not just the symptom: {headed}"
        );

        // Headless failures are a different problem and must not be explained
        // away as a display that was never asked for.
        let headless = super::launch_failure_message(real, false);
        assert!(headless.contains(real));
        assert!(
            !headless.contains("FLOWPROOF_HEADED"),
            "headless failures must not blame a variable nobody set: {headless}"
        );
    }

    /// The variable's NAME, which the option test above cannot cover: a typo in
    /// `headed_requested` would leave the feature silently unreachable while
    /// every other test still passed.
    ///
    /// Safe to mutate the environment here because no other test in this
    /// workspace reads `FLOWPROOF_HEADED`; the value is restored either way.
    #[test]
    fn visibility_and_keep_open_env_vars_are_wired() {
        let restore = std::env::var_os("FLOWPROOF_HEADED");
        let restore_keep = std::env::var_os("FLOWPROOF_KEEP_BROWSER_OPEN");
        std::env::remove_var("FLOWPROOF_HEADED");
        std::env::remove_var("FLOWPROOF_KEEP_BROWSER_OPEN");
        assert!(!super::headed_requested(), "unset must mean headless");

        // Deliberately "0": a variable someone bothered to set is one they
        // meant, and presence-based matches FLOWPROOF_NO_SHARED_BROWSER.
        std::env::set_var("FLOWPROOF_HEADED", "0");
        assert!(super::headed_requested(), "any value must mean headed");

        std::env::remove_var("FLOWPROOF_HEADED");
        std::env::set_var("FLOWPROOF_KEEP_BROWSER_OPEN", "0");
        assert!(
            super::headed_requested(),
            "keeping a browser open must also make it visible"
        );
        assert!(super::keep_browser_open_requested());

        match restore {
            Some(v) => std::env::set_var("FLOWPROOF_HEADED", v),
            None => std::env::remove_var("FLOWPROOF_HEADED"),
        }
        match restore_keep {
            Some(v) => std::env::set_var("FLOWPROOF_KEEP_BROWSER_OPEN", v),
            None => std::env::remove_var("FLOWPROOF_KEEP_BROWSER_OPEN"),
        }
    }

    #[test]
    fn keep_open_waits_until_the_flow_window_closes() {
        let mut observations = vec![false, true, true];
        let mut pauses = 0;
        super::wait_until_closed(
            || observations.pop().expect("one observation per check"),
            || pauses += 1,
        );
        assert_eq!(pauses, 2, "one pause for each still-open observation");
        assert!(
            observations.is_empty(),
            "the closed observation was consumed"
        );
    }

    #[test]
    fn text_xpath_ladder_orders_exact_label_prefix_then_case_insensitive() {
        let rungs = super::text_xpaths("Close Account");
        assert_eq!(rungs.len(), 8);
        // Rung 1: exact own-text — unchanged from the original ladder.
        assert!(rungs[0].contains("text()[normalize-space(.)='Close Account']"));
        // Rung 3: label association — wrapping form and for/id pairing.
        assert!(rungs[2].contains("//label[normalize-space()='Close Account']//input"));
        assert!(rungs[2].contains("//input[@id = //label[normalize-space()='Close Account']/@for]"));
        assert!(rungs[2].contains("//select"));
        // Rung 6: label prefix — `Name` finds the field labelled `Name:`.
        assert!(rungs[5].contains("starts-with(normalize-space(), 'Close Account')"));
        // Rungs 7-8: case-insensitive fallbacks compare lowercased text.
        assert!(rungs[6].contains("translate(normalize-space(), 'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz')='close account'"));
        assert!(rungs[6].contains("translate(@aria-label"));
        assert!(rungs[7].contains("starts-with(translate(normalize-space()"));
        // No case-sensitive rung mentions translate: exact always wins.
        for rung in &rungs[..6] {
            assert!(
                !rung.contains("translate("),
                "case-sensitive rung uses translate: {rung}"
            );
        }
    }

    #[test]
    fn text_xpath_ladder_matches_button_type_inputs_by_value() {
        let rungs = super::text_xpaths("Login");
        const TYPES: &str = "(@type='submit' or @type='button' or @type='reset')";
        // Exact rungs (1-2): @value equality, gated to button-type inputs.
        for rung in [&rungs[0], &rungs[1]] {
            assert!(
                rung.contains(&format!("//input[{TYPES} and @value='Login']")),
                "exact rung missing value branch: {rung}"
            );
        }
        // Prefix rungs (4-5): starts-with on @value.
        for rung in [&rungs[3], &rungs[4]] {
            assert!(
                rung.contains(&format!(
                    "//input[{TYPES} and starts-with(@value, 'Login')]"
                )),
                "prefix rung missing value branch: {rung}"
            );
        }
        // CI rungs (7-8): translate()-lowered @value comparison.
        assert!(rungs[6].contains(&format!(
            "//input[{TYPES} and translate(@value, 'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz')='login']"
        )));
        assert!(rungs[7].contains(&format!(
            "//input[{TYPES} and starts-with(translate(@value, 'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz'), 'login')]"
        )));
        // Label-association rungs (3, 6) never consult @value.
        assert!(!rungs[2].contains("@value"));
        assert!(!rungs[5].contains("@value"));
        // The gate is exactly the three button-ish types: no text inputs by
        // value, no input[type=image]/@alt.
        for rung in &rungs {
            assert!(
                !rung.contains("@type='image'"),
                "image input leaked: {rung}"
            );
            assert!(!rung.contains("@alt"), "alt matching leaked: {rung}");
        }
    }

    #[test]
    fn base64_matches_the_standard_alphabet_and_padding() {
        // RFC 4648 vectors.
        for (input, want) in [
            (&b""[..], ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg=="),
            (b"fooba", "Zm9vYmE="),
            (b"foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(super::base64_encode(input), want);
        }
        // Binary-safe (high bytes map into +/ territory).
        assert_eq!(super::base64_encode(&[0xfb, 0xff, 0xfe]), "+//+");
    }
}
