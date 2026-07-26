// Diagnostic agent (NOT a fixture): answers, then makes ONE more model call
// after the final assistant message — the shape goose has when it generates a
// session title under --no-session. Isolates whether flowproof's recorder stops
// capturing at the final assistant message.
import OpenAI from "openai";

const client = new OpenAI({
  apiKey: process.env.OPENAI_API_KEY || "placeholder",
  baseURL: process.env.OPENAI_BASE_URL,
});
const model = process.env.OPENAI_MODEL || "claude-opus-5";
const prompt = process.env.FLOWPROOF_PROMPT || "What is the capital of France?";

// Call 1 — the real answer.
const a = await client.chat.completions.create({
  model,
  messages: [{ role: "user", content: prompt }],
});
const reply = a.choices[0].message.content;
console.log(reply);

// Call 2 — trailing, after the final assistant message.
const b = await client.chat.completions.create({
  model,
  messages: [{ role: "user", content: `Generate a short title for: ${prompt}` }],
});
console.error("TRAILING_CALL_REPLY:", b.choices[0].message.content);
