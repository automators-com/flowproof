import fs from "node:fs/promises";
import path from "node:path";

const VERSION = "0.12.2";
const DESIGN_VERSION = "1.0.1";
const W = 1280;
const H = 720;

const C = {
  ink: "#1c1733",
  purple: "#482b7c",
  violet: "#6f57cc",
  purpleSurface: "#433163",
  purpleSoft: "#faf8fe",
  mint: "#08ffb3",
  mintDeep: "#028759",
  mintContent: "#06301f",
  body: "#4b5563",
  muted: "#6b7280",
  faint: "#6f7683",
  paper: "#ffffff",
  panel: "#fafbfc",
  field: "#f5f5f9",
  danger: "#b93831",
  warning: "#a96608",
  info: "#315fbd",
};

const SANS = "Noto Sans";
const MONO = "JetBrains Mono";
const NONE = { style: "solid", fill: "none", width: 0 };
const FLOWPROOF_DOC = `https://github.com/automators-com/flowproof/blob/v${VERSION}/docs`;
const DESIGN_DOC = `https://github.com/automators-com/design-system/blob/v${DESIGN_VERSION}/DESIGN.md`;

function textBox(slide, text, position, options = {}) {
  const shape = slide.shapes.add({
    geometry: "textbox",
    name: options.name,
    position,
    fill: options.fill ?? "none",
    line: NONE,
    borderRadius: options.radius,
  });
  shape.text = text;
  shape.text.style = {
    typeface: options.typeface ?? SANS,
    fontSize: options.size ?? 20,
    bold: options.bold ?? false,
    color: options.color ?? C.ink,
    alignment: options.align ?? "left",
    verticalAlignment: options.valign ?? "top",
    autoFit: options.autoFit ?? "shrinkText",
    wrap: "square",
    lineSpacing: options.lineSpacing ?? 1.12,
    insets: options.insets ?? { top: 0, right: 0, bottom: 0, left: 0 },
  };
  return shape;
}

function surface(slide, position, options = {}) {
  return slide.shapes.add({
    geometry: "roundRect",
    name: options.name,
    position,
    fill: options.fill ?? C.paper,
    line: NONE,
    borderRadius: options.radius ?? 16,
    shadow: options.shadow ?? "shadow-sm",
  });
}

function addFooter(slide, current, total, deck) {
  textBox(slide, `${deck}  /  Flowproof ${VERSION}`, { left: 64, top: 676, width: 430, height: 20 }, {
    size: 13, typeface: MONO, color: C.faint, name: "footer-label",
  });
  textBox(slide, `${String(current).padStart(2, "0")} / ${String(total).padStart(2, "0")}`, { left: 1110, top: 676, width: 106, height: 20 }, {
    size: 13, typeface: MONO, color: C.ink, align: "right", bold: true, name: "page-number",
  });
}

function addTitle(slide, title, subtitle, options = {}) {
  textBox(slide, title, { left: 72, top: options.top ?? 56, width: options.width ?? 1136, height: options.height ?? 92 }, {
    size: options.size ?? 40, bold: true, color: options.color ?? C.ink, lineSpacing: 1.04, name: "slide-title",
  });
  if (subtitle) {
    textBox(slide, subtitle, { left: 72, top: options.subtitleTop ?? 152, width: options.subtitleWidth ?? 940, height: options.subtitleHeight ?? 62 }, {
      size: options.subtitleSize ?? 20, color: C.body, lineSpacing: 1.28, name: "slide-subtitle",
    });
  }
}

function addSignal(slide, label, color = C.mintDeep, y = 604) {
  slide.shapes.add({ geometry: "ellipse", position: { left: 74, top: y + 4, width: 12, height: 12 }, fill: color, line: NONE });
  textBox(slide, label, { left: 98, top: y, width: 440, height: 26 }, { size: 16, bold: true, color });
}

function addCode(slide, code, position, options = {}) {
  surface(slide, position, { fill: C.paper, radius: 16, shadow: "shadow-md", name: options.name ?? "code-card" });
  const lines = code.split("\n").length;
  const size = options.size ?? (lines > 20 ? 13 : lines > 16 ? 14 : lines > 12 ? 15.5 : 17);
  textBox(slide, code, {
    left: position.left + 30,
    top: position.top + 26,
    width: position.width - 60,
    height: position.height - 50,
  }, {
    size,
    typeface: MONO,
    color: C.ink,
    lineSpacing: 1.08,
    autoFit: "shrinkText",
    name: `${options.name ?? "code-card"}-text`,
  });
}

function addNotes(slide, sources, presenter = "") {
  const notes = [presenter, "[Sources]", ...sources.map((source) => `- ${source}`)].filter(Boolean).join("\n");
  slide.speakerNotes.textFrame.setText(notes);
}

function addTwoColumnRows(slide, rows, y = 230) {
  rows.forEach(([left, right], index) => {
    const top = y + index * 58;
    textBox(slide, left, { left: 92, top, width: 460, height: 38 }, { size: 19, bold: true, color: C.violet });
    textBox(slide, right, { left: 620, top, width: 540, height: 42 }, { size: 18, color: C.body });
  });
}

const obstacleItems = [
  ["12952", "Twins", "No completion path", false],
  ["14090", "Dropdown table", "Select several controls", true],
  ["16384", "Test data in a service", "Deadline shorter than the flow", false],
  ["19875", "Tomorrow", "Pin the clock", true],
  ["21269", "Future Christmas", "Pin the clock", true],
  ["22505", "Ids are not everything", "No completion path", false],
  ["23292", "Todo list", "Drag and prove the drop", true],
  ["24499", "And counting", "Pin randomness", true],
  ["30034", "Red stripe", "Ground a small visual target", true],
  ["32403", "Math", "Read, then answer", true],
  ["33678", "Wait a moment", "Auto-wait an assertion", true],
  ["41032", "Lots of rows", "Remember a count", true],
  ["41036", "Table search", "Pin randomness", true],
  ["41037", "Meeting scheduler", "Read a table relationship", true],
  ["41038", "Halfway", "Click a meaningful point", true],
  ["41040", "Click me if you can", "Pointer cannot land", false],
  ["41041", "Escape", "Literal text", true],
  ["45618", "Tough cookie", "Focus and key commit", true],
  ["51130", "Popup windows", "Vacuous green refused", false],
  ["57683", "Confusing dates", "Pin randomness", true],
  ["60469", "Flying element", "No completion path", false],
  ["64161", "Not a table", "Scoped live value", true],
  ["66666", "Hidden element", "Not human-groundable", false],
  ["66667", "Empty", "Ground maze checkpoints", true],
  ["70310", "The last row", "Remember the final cell", true],
  ["70924", "Errors occur", "Repeat and recover", true],
  ["72946", "Get the number", "Read a fixed value", true],
  ["72954", "Two times", "Stable visible text", true],
  ["73588", "The obvious", "Select a generated value", true],
  ["73589", "Bubble sort", "Repeat and compare", true],
  ["73590", "Find and fill", "No completion path", false],
  ["73591", "Find the changed cell", "No completion path", false],
  ["78264", "Addition", "Pin randomness", true],
  ["81012", "Extracting text", "Read, then answer", true],
  ["81121", "Again and again", "Repeat to visible state", true],
  ["82018", "Reaction game", "Frame-rate timing", false],
  ["87912", "Be fast", "Deadline shorter than the flow", false],
  ["92248", "Fun with tables", "No completion path", false],
  ["94441", "Testing methods", "Choose several options", true],
  ["99999", "Scroll into view", "Work inside a frame", true],
];

const humanActions = [
  "Generate the dropdown challenge", "Choose Obstacle Course in the first dropdown", "Choose WebDriver in the second dropdown",
  "Choose Cloud in the third dropdown", "Choose XScan in the fourth dropdown", "Choose Mobile in the fifth dropdown", "Submit the selected answers",
  "Enter 16.01.2026 in the date field", "Enter Monday as the weekday for Christmas",
  ...Array.from({ length: 6 }, (_, index) => `Drag task ${index + 1} into the todo drop area`),
  "Enter 2 as the number of C characters", "Generate the red-stripe challenge", "Click the red stripe",
  "Enter 1274 as the calculation result", "Start the calculation", "Send the result",
  "Remember the number of displayed table rows as the row count", "Enter the remembered row count in the row-count field", "Submit the row count",
  "Enter True as the table-search result", "Enter Closed as the meeting-scheduler result", "Click the right half of the button",
  "Enter {Click} in the result field", "Click the generated-number box", "Enter 10104576 in the first number field",
  "Enter 929262 in the second number field", "Enter 47633101 in the third number field", "Move focus to the next field",
  "Generate a date", "Enter 2030-02-01 in the date-solution field", "Submit the date", "Generate an order ID",
  "Remember the value beside order id as the order ID", "Enter the remembered order ID in the offer ID field", "Generate the maze",
  ...Array.from({ length: 10 }, (_, index) => `Click checkpoint ${index}`),
  "Remember the last cell in the final order row as the order value", "Enter the remembered order value in the order-value field",
  "Clear the fault", "Press the counter button", "Enter Sue's number, 00618971341641, in the number field",
  "Click Click me", "Click Click me", "Generate the answer options", "Choose npOXTMQ57J from the answer dropdown", "Submit the answer",
  "Swap the two numbers", "Move to the next pair", "Enter 152 as the addition result", "Enter 1981.05 as the extracted total amount",
  "Press the button again", "Press the button once more", "Select Functional, End2End, GUI, and Exploratory testing together",
  "Scroll the embedded challenge to 147 pixels", "Enter Tosca in the text field inside the embedded challenge", "Submit the answer",
];

function validateContent() {
  const solved = obstacleItems.filter((item) => item[3]);
  if (solved.length !== 28 || obstacleItems.length !== 40) throw new Error("Obstacle totals drifted");
  for (const action of humanActions) {
    if (/\b(?:css:|id:|rules:|xpath:)\b/i.test(action)) throw new Error(`Selector leaked into human action: ${action}`);
  }
}

function trainingDeck(Presentation) {
  const deck = Presentation.create({ slideSize: { width: W, height: H } });
  const total = 22;
  const sources = [`${FLOWPROOF_DOC}/authoring.md`, `${FLOWPROOF_DOC}/obstacle-course.md`, DESIGN_DOC];
  const add = (builder, notes = sources) => {
    const slide = deck.slides.add();
    slide.background.fill = C.panel;
    builder(slide, deck.slides.items.length);
    addFooter(slide, deck.slides.items.length, total, "Training");
    addNotes(slide, notes);
  };

  add((s) => {
    textBox(s, "Automating the obstacle course", { left: 72, top: 116, width: 900, height: 86 }, { size: 56, bold: true, lineSpacing: 1.02 });
    textBox(s, `with Flowproof ${VERSION}`, { left: 72, top: 214, width: 760, height: 72 }, { size: 46, bold: true, color: C.violet });
    textBox(s, "Human intent at record time. Deterministic proof at replay.", { left: 72, top: 332, width: 810, height: 44 }, { size: 23, color: C.body });
    textBox(s, "40", { left: 72, top: 474, width: 120, height: 54 }, { size: 40, bold: true });
    textBox(s, "28", { left: 230, top: 474, width: 120, height: 54 }, { size: 40, bold: true, color: C.mintDeep });
    textBox(s, "12", { left: 388, top: 474, width: 120, height: 54 }, { size: 40, bold: true, color: C.danger });
    textBox(s, "pages", { left: 72, top: 528, width: 120, height: 28 }, { size: 16, color: C.body });
    textBox(s, "passing", { left: 230, top: 528, width: 120, height: 28 }, { size: 16, color: C.body });
    textBox(s, "refused", { left: 388, top: 528, width: 120, height: 28 }, { size: 16, color: C.body });
    addSignal(s, "Verified against the 0.12.2 authoring contract", C.mintDeep, 594);
  });

  add((s) => {
    addTitle(s, "The first half teaches the mechanism.", "The second half lets every obstacle name the technique it needs.");
    addTwoColumnRows(s, [
      ["Human intent", "Write the action in ordinary words."],
      ["Grounded recording", "The model chooses only from the live inventory."],
      ["Deterministic replay", "Selectors live in the trace, not in the author's prose."],
      ["Honest refusal", "A false green is worse than a loud limitation."],
      ["Forty exercises", "Twenty-eight pass; twelve document a real boundary."],
    ], 254);
  });

  add((s) => {
    addTitle(s, "Record once. Replay from then on.", "The model participates only while the human intent is grounded.");
    textBox(s, "Record", { left: 78, top: 260, width: 230, height: 50 }, { size: 32, bold: true, color: C.violet });
    textBox(s, "Live app + human steps\nproduces a validated trace", { left: 78, top: 318, width: 330, height: 86 }, { size: 21, color: C.body });
    textBox(s, "Replay", { left: 78, top: 458, width: 230, height: 50 }, { size: 32, bold: true, color: C.mintDeep });
    textBox(s, "Trace only\nzero authoring-model calls", { left: 78, top: 516, width: 330, height: 72 }, { size: 21, color: C.body });
    addCode(s, `flowproof record order.flow.yaml\nflowproof run    order.flow.yaml\n\nname: Order status\napp: web\nurl: https://example.com/orders\nsteps:\n  - Enter ABC in the order ID field\n  - Search for the order\n  - assert: page shows Shipped`, { left: 520, top: 220, width: 670, height: 390 });
  });

  add((s) => {
    addTitle(s, "Describe the thing you mean.", "Different wording can express the same intent because the live scene supplies the possible targets.");
    addTwoColumnRows(s, [
      ["Save the form", "the visible Save control"],
      ["Could you save this?", "the same intent, conversationally"],
      ["Use the second Delete button", "one of several look-alikes"],
      ["Choose Grace Hopper's row", "a target identified by nearby text"],
      ["Put Tosca in the embedded form", "a field inside a same-origin frame"],
    ], 246);
  });

  add((s) => {
    addTitle(s, "Relationships make selectors unnecessary.", "A human names the value by what surrounds it; Flowproof records the deterministic scoped target.");
    addCode(s, `steps:\n  - Remember the value beside "order id" as the order ID\n  - Enter the remembered order ID in the offer ID field\n  - assert: page shows You solved this automation problem`, { left: 142, top: 264, width: 996, height: 236 });
    addSignal(s, "The author names the relationship, not the DOM structure", C.mintDeep, 548);
  });

  add((s) => {
    addTitle(s, "Say what the person does.", "Plain language covers direct, relational, positional, multi-control, frame, and keyboard actions.");
    addTwoColumnRows(s, [
      ["Save the form", "press a control"],
      ["Click the right half of the button", "click a meaningful point"],
      ["Choose A, B and C together", "commit several options"],
      ["Bring the footer into view", "scroll to an off-screen target"],
      ["Drag Row 1 into Done", "press, move while held, release"],
      ["Move focus to the next field", "press Tab and commit the field"],
    ], 230);
  });

  add((s) => {
    addTitle(s, "A drag must be followed by proof.", "The drop effect belongs to the application, so the next step must assert what changed.");
    addCode(s, `steps:\n  - Drag task 1 into the todo drop area\n  - assert: the "css:tbody.droparea" shows Write recipe`, { left: 160, top: 276, width: 960, height: 190 });
    addSignal(s, "Actions stay human; this assertion is explicit because the drop area has no usable name", C.warning, 538);
  });

  add((s) => {
    addTitle(s, "Assertions wait for outcomes, not time.", "A fixed sleep guesses. An assertion waits until its condition is true or its bound expires.");
    addCode(s, `- assert: page shows Saved\n- assert: the "SEND" is enabled within 40s\n- assert: the "Total" shows 42\n- assert: the "Basket" is not visible\n- assert: the "css:.row" appears 5 times`, { left: 170, top: 250, width: 940, height: 282 });
  });

  add((s) => {
    addTitle(s, "When the app invents a value, remember it.", "The value is read fresh during record and replay; only the human name appears in the specification.");
    addCode(s, `steps:\n  - Remember the offer ID as the offer\n  - Enter the remembered offer in Reference\n  - Remember the number of displayed rows as the row count\n  - Enter the remembered row count in Count`, { left: 150, top: 258, width: 980, height: 250 });
    addSignal(s, "No generated value is frozen into the trace", C.mintDeep, 550);
  });

  add((s) => {
    addTitle(s, "Pin what drifts, then read the page.", "A seed, clock, and viewport make the environment reproducible; they do not replace observation.");
    addCode(s, `browser:\n  viewport: { width: 1280, height: 900 }\n  random:\n    seed: 1\n  clock:\n    at: "2026-01-15T12:00:00Z"\n    timezone: "Europe/Berlin"`, { left: 110, top: 250, width: 520, height: 292 });
    textBox(s, "1  Pin the environment.\n\n2  Run with an intentionally wrong assertion.\n\n3  Read the real value and update the flow.", { left: 730, top: 264, width: 430, height: 270 }, { size: 22, color: C.body, lineSpacing: 1.2 });
  });

  add((s) => {
    addTitle(s, "Read values off the page. Do not predict them.", "Other scripts may consume random draws first. The seed fixes the sequence, not the position.");
    textBox(s, "The stable test is not “our PRNG simulation agrees.”\n\nThe stable test is “the page displayed this value, the flow used it, and replay proved the same relationship again.”", { left: 120, top: 270, width: 1040, height: 220 }, { size: 28, bold: true, color: C.purple, align: "center", valign: "middle", lineSpacing: 1.18 });
    addSignal(s, "A pinned viewport also prevents controls from drifting under occluders", C.info, 554);
  });

  add((s) => {
    addTitle(s, "Repeat and when remain explicit.", "Control structure is deterministic; the actions nested inside it remain ordinary human intent.");
    addCode(s, `- repeat:\n    until: page shows Enough\n    max: 15\n    steps:\n      - Press the counter button again\n\n- when: the recovery button is not visible\n  steps:\n    - Enable recovery mode`, { left: 300, top: 220, width: 680, height: 376 });
  });

  add((s) => {
    addTitle(s, "Conditions read state and never wait.", "Keep them narrow enough to distinguish the intended state from similar words elsewhere on the page.");
    addTwoColumnRows(s, [
      ["page shows X", "the visible surface contains X"],
      ["the target shows X", "one resolved element contains X"],
      ["the target is visible", "it resolves and is rendered"],
      ["A is greater than B", "both values parse as numbers"],
    ], 270);
    addSignal(s, "A broad page condition can match an unrelated heading", C.warning, 552);
  });

  add((s) => {
    addTitle(s, "The frame is part of the target.", "People describe the embedded surface; the recording stores an ordinary deterministic framed target.");
    addCode(s, `steps:\n  - Scroll the embedded challenge to 147 pixels\n  - Enter Tosca in the text field inside the embedded challenge\n  - Submit the answer`, { left: 150, top: 278, width: 980, height: 220 });
  });

  add((s) => {
    addTitle(s, "Flowproof refuses shortcuts a person could not take.", "The framework treats a trustworthy failure as more valuable than an unearned green.");
    addTwoColumnRows(s, [
      ["No invented selectors", "The model must choose a listed live target."],
      ["No click through an occluder", "The real hit target must receive the click."],
      ["No hidden human target", "A rendered inventory cannot name an invisible control."],
      ["No synthetic success", "An app-defined effect must be asserted."],
    ], 260);
  });

  add((s) => {
    addTitle(s, "A false green is worse than a refusal.", "Obstacle 51130 reports success even though its popup never opens. Recording that pass would prove nothing.");
    textBox(s, "REFUSE", { left: 140, top: 278, width: 1000, height: 112 }, { size: 76, bold: true, color: C.danger, align: "center" });
    textBox(s, "A passing report must correspond to working software.", { left: 210, top: 430, width: 860, height: 48 }, { size: 27, color: C.body, align: "center" });
  });

  add((s) => {
    addTitle(s, "Human intent is default. Structure stays explicit.", "The source remains readable without making replay probabilistic.");
    surface(s, { left: 74, top: 236, width: 530, height: 318 }, { fill: C.purpleSoft, shadow: "shadow-none" });
    textBox(s, "Plain scalar actions", { left: 108, top: 272, width: 450, height: 42 }, { size: 27, bold: true, color: C.purple });
    textBox(s, "Any natural wording\nGrounded against the live scene\nValidated target inventory\nDeterministic trace output", { left: 108, top: 336, width: 420, height: 174 }, { size: 21, color: C.body, lineSpacing: 1.24 });
    surface(s, { left: 676, top: 236, width: 530, height: 318 }, { fill: C.paper, shadow: "shadow-sm" });
    textBox(s, "Structured proof", { left: 710, top: 272, width: 450, height: 42 }, { size: 27, bold: true, color: C.purple });
    textBox(s, "assert: expected outcome\nrepeat: condition and bound\nwhen: deterministic branch\nExplicit failure semantics", { left: 710, top: 336, width: 420, height: 174 }, { size: 21, color: C.body, lineSpacing: 1.24 });
  });

  for (let page = 0; page < 4; page += 1) {
    add((s) => {
      addTitle(s, page === 0 ? "Every exercise names its technique." : `Obstacle map, continued.`, "Green entries have a passing record and replay. Red entries document a real boundary.");
      obstacleItems.slice(page * 10, page * 10 + 10).forEach(([id, name, technique, solved], index) => {
        const col = index % 2;
        const row = Math.floor(index / 2);
        const left = 78 + col * 598;
        const top = 218 + row * 88;
        textBox(s, id, { left, top, width: 86, height: 28 }, { size: 17, bold: true, typeface: MONO, color: solved ? C.mintDeep : C.danger });
        textBox(s, name, { left: left + 98, top: top - 2, width: 300, height: 30 }, { size: 20, bold: true });
        textBox(s, technique, { left: left + 98, top: top + 32, width: 420, height: 28 }, { size: 16, color: C.body });
      });
    });
  }

  add((s) => {
    textBox(s, "Twenty-eight passing solutions.", { left: 84, top: 160, width: 1080, height: 82 }, { size: 52, bold: true, color: C.ink, align: "center" });
    textBox(s, "Twelve honest refusals.", { left: 84, top: 268, width: 1080, height: 82 }, { size: 52, bold: true, color: C.violet, align: "center" });
    textBox(s, "Zero sketches.", { left: 84, top: 376, width: 1080, height: 82 }, { size: 52, bold: true, color: C.mintDeep, align: "center" });
    textBox(s, "Every green corresponds to a passing record and a passing replay.", { left: 210, top: 522, width: 860, height: 42 }, { size: 22, color: C.body, align: "center" });
  });

  if (deck.slides.items.length !== total) throw new Error(`Training slide count is ${deck.slides.items.length}`);
  return deck;
}

function yaml(id, title, browserLines, stepLines) {
  const context = browserLines.length ? `\nbrowser:\n${browserLines.map((line) => `  ${line}`).join("\n")}` : "";
  return `name: obstacle ${id} - ${title}\napp: web\nurl: https://obstaclecourse.tricentis.com/Obstacles/${id}${context}\nsteps:\n${stepLines.map((line) => `  ${line}`).join("\n")}`;
}

const solvedFlows = [
  ["14090", "Dropdown table", "Generate the challenge, choose five answers, then submit.", ["random:", "  seed: 1"], ["- Generate the dropdown challenge", "- Choose Obstacle Course in the first dropdown", "- Choose WebDriver in the second dropdown", "- Choose Cloud in the third dropdown", "- Choose XScan in the fourth dropdown", "- Choose Mobile in the fifth dropdown", "- Submit the selected answers"]],
  ["19875", "Tomorrow", "Pin the clock and enter the next calendar day.", ["clock:", "  at: \"2026-01-15T12:00:00Z\"", "  timezone: Europe/Berlin"], ["- Enter 16.01.2026 in the date field"]],
  ["21269", "Future Christmas", "Pin the clock and answer with the weekday.", ["clock:", "  at: \"2026-01-15T12:00:00Z\"", "  timezone: Europe/Berlin"], ["- Enter Monday as the weekday for Christmas"]],
  ["23292", "Todo list", "Drag every task and prove each app-defined drop.", ["viewport: { width: 1280, height: 2400 }"], ["- Drag task 1 into the todo drop area", "- assert: the \"css:tbody.droparea\" shows Write recipe", "- Drag task 2 into the todo drop area", "- assert: the \"css:tbody.droparea\" shows Buy ingredients", "- Drag task 3 into the todo drop area", "- assert: the \"css:tbody.droparea\" shows Bake cake", "- Drag task 4 into the todo drop area", "- assert: the \"css:tbody.droparea\" shows Eat cake", "- Drag task 5 into the todo drop area", "- assert: the \"css:tbody.droparea\" shows Cleanup", "- Drag task 6 into the todo drop area", "- assert: the \"css:tbody.droparea\" shows Update recipe"]],
  ["24499", "And counting", "Read the prompt under a pinned seed and enter its count.", ["random:", "  seed: 1"], ["- Enter 2 as the number of C characters"]],
  ["30034", "Red stripe", "Generate the challenge and click the visible red stripe.", ["random:", "  seed: 1234"], ["- Generate the red-stripe challenge", "- Click the red stripe"]],
  ["32403", "Math", "Read the generated operation once and enter its result.", ["random:", "  seed: 1"], ["- Enter 1274 as the calculation result"]],
  ["33678", "Wait a moment", "Start, wait on the real state, then submit.", ["random:", "  seed: 1"], ["- Start the calculation", "- assert: the \"SEND\" is enabled within 40s", "- Send the result"]],
  ["41032", "Lots of rows", "Remember the live row count rather than freezing it.", [], ["- Remember the number of displayed table rows as the row count", "- Enter the remembered row count in the row-count field", "- Submit the row count"]],
  ["41036", "Table search", "Read the pinned table and enter the answer.", ["random:", "  seed: 1"], ["- Enter True as the table-search result"]],
  ["41037", "Meeting scheduler", "Read the requested table relationship and answer.", ["random:", "  seed: 1"], ["- Enter Closed as the meeting-scheduler result"]],
  ["41038", "Halfway", "Click a meaningful point inside the control.", [], ["- Click the right half of the button"]],
  ["41041", "Escape", "Enter literal text without command-language escaping.", [], ["- Enter {Click} in the result field"]],
  ["45618", "Tough cookie", "Read the focused box and commit the final field with Tab.", ["random:", "  seed: 1"], ["- Click the generated-number box", "- Enter 10104576 in the first number field", "- Enter 929262 in the second number field", "- Enter 47633101 in the third number field", "- Move focus to the next field"]],
  ["57683", "Confusing dates", "Generate, translate the displayed date, and submit.", ["random:", "  seed: 1"], ["- Generate a date", "- Enter 2030-02-01 in the date-solution field", "- Submit the date"]],
  ["64161", "Not a table", "Name the value by its neighbouring label.", [], ["- Generate an order ID", "- Remember the value beside \"order id\" as the order ID", "- Enter the remembered order ID in the offer ID field"]],
  ["66667", "Empty", "Generate the maze and visit every visible checkpoint.", ["random:", "  seed: 1234"], ["- Generate the maze", ...Array.from({ length: 10 }, (_, index) => `- Click checkpoint ${index}`)]],
  ["70310", "The last row", "Read the final cell from the final order row.", [], ["- Remember the last cell in the final order row as the order value", "- Enter the remembered order value in the order-value field"]],
  ["70924", "Errors occur", "Repeat the counter and recover only when it faults.", ["random:", "  seed: 7"], ["- repeat:", "    until: page shows You solved this automation problem", "    max: 40", "    steps:", "      - when: the \"id:b1\" is not visible", "        steps:", "          - Clear the fault", "      - Press the counter button"]],
  ["72946", "Get the number", "Enter the fixed value published by the page.", [], ["- Enter Sue's number, 00618971341641, in the number field"]],
  ["72954", "Two times", "Address the changing control by its stable visible text.", [], ["- Click \"Click me\"", "- Click \"Click me\""]],
  ["73588", "The obvious", "Generate, choose the produced value, and submit.", ["random:", "  seed: 1"], ["- Generate the answer options", "- Choose npOXTMQ57J from the answer dropdown", "- Submit the answer"]],
  ["73589", "Bubble sort", "Repeat, compare the two live values, then swap or advance.", ["viewport: { width: 1280, height: 720 }", "random:", "  seed: 3"], ["- repeat:", "    until: page shows You solved this automation problem", "    max: 120", "    steps:", "      - when: the \"css:.bubble .num:nth-child(1)\" is greater", "          than the \"css:.bubble .num:nth-child(2)\"", "        steps:", "          - Swap the two numbers", "      - when: page does not show Perfect - you did it", "        steps:", "          - Move to the next pair"]],
  ["78264", "Addition", "Read the generated numbers and enter their sum.", ["random:", "  seed: 1"], ["- Enter 152 as the addition result"]],
  ["81012", "Extracting text", "Read the generated total and enter it.", ["random:", "  seed: 1"], ["- Enter 1981.05 as the extracted total amount"]],
  ["81121", "Again and again", "Repeat until the visible label changes, then press once more.", ["random:", "  seed: 1234"], ["- repeat:", "    until: page shows Enough", "    max: 15", "    steps:", "      - Press the button again", "- Press the button once more"]],
  ["94441", "Testing methods", "Choose all four supported options in one commit.", [], ["- Select Functional, End2End, GUI, and Exploratory testing together"]],
  ["99999", "Scroll into view", "Work inside the embedded challenge and submit.", [], ["- Scroll the embedded challenge to 147 pixels", "- Enter Tosca in the text field inside the embedded challenge", "- Submit the answer"]],
].map(([id, title, summary, browser, steps]) => ({
  id, title, summary, browser, steps,
  code: yaml(id, title, browser, [...steps, "- assert: page shows You solved this automation problem"]),
}));

function solutionsDeck(Presentation) {
  const deck = Presentation.create({ slideSize: { width: W, height: H } });
  const total = 35;
  const standardSources = [`${FLOWPROOF_DOC}/authoring.md`, `${FLOWPROOF_DOC}/obstacle-course.md`, DESIGN_DOC];
  const add = (builder, sources = standardSources) => {
    const slide = deck.slides.add();
    slide.background.fill = C.panel;
    builder(slide, deck.slides.items.length);
    addFooter(slide, deck.slides.items.length, total, "Solutions");
    addNotes(slide, sources);
  };

  add((s) => {
    textBox(s, "Solutions in human language", { left: 76, top: 146, width: 1030, height: 86 }, { size: 58, bold: true });
    textBox(s, "Actions describe human intent. Structured proof remains explicit.", { left: 78, top: 270, width: 860, height: 50 }, { size: 24, color: C.body });
    textBox(s, "28", { left: 78, top: 430, width: 140, height: 62 }, { size: 46, bold: true, color: C.mintDeep });
    textBox(s, "12", { left: 254, top: 430, width: 140, height: 62 }, { size: 46, bold: true, color: C.danger });
    textBox(s, "0", { left: 430, top: 430, width: 140, height: 62 }, { size: 46, bold: true, color: C.purple });
    textBox(s, "passing", { left: 78, top: 493, width: 140, height: 28 }, { size: 17, color: C.body });
    textBox(s, "refused", { left: 254, top: 493, width: 140, height: 28 }, { size: 17, color: C.body });
    textBox(s, "sketches", { left: 430, top: 493, width: 140, height: 28 }, { size: 17, color: C.body });
    addSignal(s, `Verified for Flowproof ${VERSION}`, C.mintDeep, 574);
  });

  add((s) => {
    addTitle(s, "Every green ran twice.", "First against the live page during record, then again from the deterministic trace.");
    textBox(s, "Write the action in your own words. The authoring model grounds it to one of the live targets Flowproof supplied; invented selectors are rejected.", { left: 78, top: 240, width: 430, height: 180 }, { size: 23, color: C.body, lineSpacing: 1.24 });
    addCode(s, yaml("19875", "Tomorrow", ["clock:", "  at: \"2026-01-15T12:00:00Z\"", "  timezone: Europe/Berlin"], ["- Enter 16.01.2026 in the date field", "- assert: page shows You solved this automation problem"]), { left: 580, top: 214, width: 620, height: 388 });
    addSignal(s, "Actions can vary; assertions and control structures keep deterministic meaning", C.info, 566);
  });

  add((s) => {
    addTitle(s, "Twenty-eight pass. Twelve refuse honestly.", "The score is less important than knowing which green results correspond to real behavior.");
    textBox(s, "28", { left: 126, top: 272, width: 220, height: 104 }, { size: 82, bold: true, color: C.mintDeep, align: "center" });
    textBox(s, "Passing record + replay", { left: 90, top: 392, width: 292, height: 40 }, { size: 20, bold: true, align: "center" });
    textBox(s, "12", { left: 530, top: 272, width: 220, height: 104 }, { size: 82, bold: true, color: C.danger, align: "center" });
    textBox(s, "No honest completion", { left: 494, top: 392, width: 292, height: 40 }, { size: 20, bold: true, align: "center" });
    textBox(s, "0", { left: 934, top: 272, width: 220, height: 104 }, { size: 82, bold: true, color: C.purple, align: "center" });
    textBox(s, "Unverified sketches", { left: 898, top: 392, width: 292, height: 40 }, { size: 20, bold: true, align: "center" });
  });

  solvedFlows.forEach((flow) => {
    add((s) => {
      textBox(s, flow.id, { left: 76, top: 130, width: 330, height: 44 }, { size: 25, bold: true, typeface: MONO, color: C.violet });
      textBox(s, flow.title, { left: 76, top: 190, width: 400, height: 104 }, { size: 43, bold: true, lineSpacing: 1.02 });
      textBox(s, flow.summary, { left: 76, top: 320, width: 390, height: 150 }, { size: 22, color: C.body, lineSpacing: 1.24 });
      addSignal(s, "Passing record + replay", C.mintDeep, 566);
      addCode(s, flow.code, { left: 500, top: 92, width: 710, height: 548 }, { name: `flow-${flow.id}` });
    }, [`https://obstaclecourse.tricentis.com/Obstacles/${flow.id}`, `${FLOWPROOF_DOC}/authoring.md`, `${FLOWPROOF_DOC}/obstacle-course.md`, DESIGN_DOC]);
  });

  add((s) => {
    textBox(s, "66666", { left: 76, top: 130, width: 330, height: 44 }, { size: 25, bold: true, typeface: MONO, color: C.danger });
    textBox(s, "Hidden element", { left: 76, top: 190, width: 430, height: 104 }, { size: 43, bold: true });
    textBox(s, "A human cannot identify or click a control that the page does not render. The semantic author correctly refuses to invent a target.", { left: 76, top: 320, width: 390, height: 174 }, { size: 22, color: C.body, lineSpacing: 1.24 });
    addSignal(s, "Correctly refused", C.danger, 566);
    addCode(s, `name: obstacle 66666 - Hidden element\napp: web\nurl: https://obstaclecourse.tricentis.com/Obstacles/66666\nsteps:\n  - Click the hidden element\n\nrecord: cannot ground the target from the rendered live inventory`, { left: 520, top: 200, width: 660, height: 280 }, { size: 16, name: "hidden-refusal" });
  }, ["https://obstaclecourse.tricentis.com/Obstacles/66666", `${FLOWPROOF_DOC}/authoring.md`, `${FLOWPROOF_DOC}/obstacle-course.md`, DESIGN_DOC]);

  add((s) => {
    addTitle(s, "A page that cannot be automated is a result.", "Six pages have no completion path, four exceed honest interaction limits, and two must be refused to avoid false proof.");
    textBox(s, "The boundary is part of the framework's value: it names the reason, preserves evidence, and never turns a limitation into a green report.", { left: 160, top: 300, width: 960, height: 150 }, { size: 30, bold: true, color: C.purple, align: "center", valign: "middle", lineSpacing: 1.2 });
  });

  add((s) => {
    addTitle(s, "Twelve honest refusals.", "Each one has a specific reason rather than a generic “unsupported” label.");
    const groups = [
      ["No completion path - 6", "12952 Twins\n22505 Ids are not everything\n60469 Flying element\n73590 Find and fill\n73591 Find the changed cell\n92248 Fun with tables", C.danger],
      ["Interaction limit - 4", "16384 Test data in a service\n41040 Click me if you can\n82018 Reaction game\n87912 Be fast", C.warning],
      ["Proof refused - 2", "51130 Popup windows\n66666 Hidden element", C.info],
    ];
    groups.forEach(([heading, body, color], index) => {
      const left = 76 + index * 402;
      textBox(s, heading, { left, top: 232, width: 350, height: 40 }, { size: 23, bold: true, color });
      textBox(s, body, { left, top: 302, width: 350, height: 260 }, { size: 18, color: C.body, lineSpacing: 1.35 });
    });
  });

  add((s) => {
    textBox(s, "Plain intent at record time.", { left: 100, top: 192, width: 1080, height: 82 }, { size: 52, bold: true, align: "center" });
    textBox(s, "Deterministic proof at replay.", { left: 100, top: 308, width: 1080, height: 82 }, { size: 52, bold: true, color: C.violet, align: "center" });
    textBox(s, "Twenty-eight real greens. Twelve reasons to stop.", { left: 180, top: 458, width: 920, height: 48 }, { size: 24, color: C.body, align: "center" });
    addSignal(s, "No selectors in human-authored action steps", C.mintDeep, 570);
  });

  if (deck.slides.items.length !== total) throw new Error(`Solutions slide count is ${deck.slides.items.length}`);
  return deck;
}

async function saveDeck(PresentationFile, deck, outputPath) {
  const file = await PresentationFile.exportPptx(deck);
  await file.save(outputPath);
}

export async function buildDecks({ Presentation, PresentationFile, outputDir }) {
  validateContent();
  if (solvedFlows.length !== 28) throw new Error(`Expected 28 passing flows, got ${solvedFlows.length}`);
  await fs.mkdir(outputDir, { recursive: true });
  const training = trainingDeck(Presentation);
  const solutions = solutionsDeck(Presentation);
  await saveDeck(PresentationFile, training, path.join(outputDir, `flowproof-training-${VERSION}.pptx`));
  await saveDeck(PresentationFile, solutions, path.join(outputDir, `flowproof-solutions-${VERSION}.pptx`));
}
