#!/usr/bin/env node
// A real, minimal agent built on the official OpenAI Node SDK.
//
// Not a flowproof test double: it is the same `openai` client production
// agents use, doing a genuine tool-calling loop. flowproof points its base
// URL at the record/replay proxy through the standard `OPENAI_BASE_URL`
// environment variable, and hands it the task through `FLOWPROOF_PROMPT`.
//
// This is the Node twin of weather_agent.py. It exists so the npm install
// path has a first example that needs no Python: a reader who arrived via
// `npx flowproof` should not have to set up a second language runtime to
// see their first green agent test.
//
// The `get_weather` tool returns a VOLATILE reading (a live timestamp), so a
// deterministic replay is only possible because flowproof substitutes the
// spec's mock at the model boundary.
//
// Run:  npm install openai   (then let flowproof start it)
import OpenAI from "openai";

const client = new OpenAI({
  baseURL: process.env.OPENAI_BASE_URL,
  apiKey: process.env.OPENAI_API_KEY ?? "unused-at-replay",
});
const MODEL = process.env.FLOWPROOF_AGENT_MODEL ?? "claude-sonnet-4-5";

const TOOLS = [
  {
    type: "function",
    function: {
      name: "get_weather",
      description: "Current weather for a city.",
      parameters: {
        type: "object",
        properties: { city: { type: "string" } },
        required: ["city"],
      },
    },
  },
];

function getWeather(city) {
  // A real tool with a volatile result: the observation time changes every
  // run. flowproof's mock is what makes replay deterministic.
  return { city, sky: "clear", observed_at_ns: String(process.hrtime.bigint()) };
}

async function main() {
  const messages = [{ role: "user", content: process.env.FLOWPROOF_PROMPT }];
  for (let i = 0; i < 6; i++) {
    const resp = await client.chat.completions.create({
      model: MODEL,
      messages,
      tools: TOOLS,
    });
    const msg = resp.choices[0].message;
    if (msg.tool_calls?.length) {
      messages.push(msg);
      for (const call of msg.tool_calls) {
        const args = JSON.parse(call.function.arguments);
        messages.push({
          role: "tool",
          tool_call_id: call.id,
          content: JSON.stringify(getWeather(args.city)),
        });
      }
      continue;
    }
    // The reply is what `assert: reply contains ...` reads.
    console.log(msg.content ?? "");
    return;
  }
  console.log("(agent gave up)");
}

main().catch((err) => {
  console.error(err?.message ?? String(err));
  process.exit(1);
});
