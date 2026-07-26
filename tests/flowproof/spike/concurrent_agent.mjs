// Diagnostic agent (NOT a fixture): fires TWO model calls concurrently, to test
// whether flowproof's recorder drops one when requests overlap in flight.
import OpenAI from "openai";

const client = new OpenAI({
  apiKey: process.env.OPENAI_API_KEY || "placeholder",
  baseURL: process.env.OPENAI_BASE_URL,
});
const model = process.env.OPENAI_MODEL || "claude-opus-5";
const prompt = process.env.FLOWPROOF_PROMPT || "What is the capital of France?";

const [a, b] = await Promise.all([
  client.chat.completions.create({
    model,
    messages: [{ role: "user", content: prompt }],
  }),
  client.chat.completions.create({
    model,
    messages: [{ role: "user", content: `Generate a short title for: ${prompt}` }],
  }),
]);
console.log(a.choices[0].message.content);
console.error("CONCURRENT_SECOND:", b.choices[0].message.content);
