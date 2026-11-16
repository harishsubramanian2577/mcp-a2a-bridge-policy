// Copyright 2026 Salesforce, Inc. All rights reserved.
// End-to-end check with the official MCP TypeScript SDK, the client in MCP Inspector. Unlike the
// Python SDK, it resumes with the last event id seen on the latest stream, so a question answered
// after the first resume only works if the waiting stream repeats that id. Run by run.sh.
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";
import { ElicitRequestSchema } from "@modelcontextprotocol/sdk/types.js";

const SUPPORT = "http://flex-1:8083/support/";

function check(name, condition, detail = "") {
  console.log(`  ${condition ? "PASS" : "FAIL"}  ${name}${detail ? `  (${detail})` : ""}`);
  if (!condition) process.exit(1);
}

async function answerAfter(delayMs, answer) {
  const transport = new StreamableHTTPClientTransport(new URL(SUPPORT),
    { requestInit: { headers: { "x-client-id": `ts-${delayMs}` } } });
  const client = new Client({ name: "ts-check", version: "1" }, { capabilities: { elicitation: {} } });
  const asked = [];
  client.setRequestHandler(ElicitRequestSchema, async (request) => {
    asked.push(request.params.message);
    await new Promise((resolve) => setTimeout(resolve, delayMs));
    return { action: "accept", content: { answer } };
  });
  await client.connect(transport);
  try {
    const result = await client.callTool({ name: "ask_agent", arguments: { message: "ask me" } },
      undefined, { timeout: 30000 });
    return { asked, text: result.content?.[0]?.text ?? "" };
  } catch (error) {
    return { asked, text: `error: ${error.message}` };
  } finally {
    await client.close();
  }
}

console.log("TypeScript SDK client with elicitation");
let run = await answerAfter(0, "Lima");
check("an answer before the first resume returns the result", run.asked.length === 1
  && run.text === "echo: Lima", run.text);
run = await answerAfter(5000, "Quito");
check("an answer after the client has resumed returns the result", run.asked.length === 1
  && run.text === "echo: Quito", run.text);
console.log("All MCP TypeScript SDK checks passed.");
