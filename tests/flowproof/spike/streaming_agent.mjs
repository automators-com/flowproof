// Diagnostic agent (NOT a fixture): two SEQUENTIAL model calls, both streamed,
// which is the shape goose has (goose only ever sends stream:true, and makes a
// trailing session-title call). Tests whether flowproof's recorder captures a
// second STREAMING call.
import OpenAI from "openai";

const client = new OpenAI({
  apiKey: process.env.OPENAI_API_KEY || "placeholder",
  baseURL: process.env.OPENAI_BASE_URL,
});
const model = process.env.OPENAI_MODEL || "claude-opus-5";
const prompt = process.env.FLOWPROOF_PROMPT || "What is the capital of France?";

async function ask(content) {
  const stream = await client.chat.completions.create({
    model,
    messages: [{ role: "user", content }],
    stream: true,
  });
  let out = "";
  for await (const chunk of stream) out += chunk.choices[0]?.delta?.content || "";
  return out;
}

console.log(await ask(prompt));
console.error("TRAILING:", await ask(`Generate a short title for: ${prompt}`));
