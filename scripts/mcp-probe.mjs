#!/usr/bin/env node
// mcp-probe — drive the oab-mcp core directly over stdio, without the desktop
// app. Same MCP flow the Tauri bridge uses (initialize → tools/call), so it
// exercises the exact credential/region resolution the app would — but in a
// fast terminal loop with the raw JSON-RPC and the core's stderr in view.
//
// Usage:
//   node scripts/mcp-probe.mjs <oab-mcp-binary> [tool] [json-args]
//
// Examples:
//   # against the shipped core inside the .app bundle, with your profile:
//   AWS_PROFILE=brettchien AWS_REGION=ap-east-2 OAB_CLUSTER=oab \
//     node scripts/mcp-probe.mjs "/Applications/OAB Studio.app/Contents/MacOS/oab-mcp"
//
//   # a specific tool + args:
//   node scripts/mcp-probe.mjs ./target/debug/oab-mcp deploy_get '{"service":"oab-prod-orca"}'
//
// The core resolves AWS creds from the standard chain (env → profile → …), so
// whatever AWS_* / AWS_PROFILE you export here is exactly what the app's sidecar
// would inherit. A wrong account/region shows up as an isError result or a
// stderr line — the credential-drift bug, visible in one shot.

import { spawn } from "node:child_process";

const bin = process.argv[2];
if (!bin) {
  console.error("usage: node mcp-probe.mjs <oab-mcp-binary> [tool] [json-args]");
  process.exit(2);
}
const tool = process.argv[3] || "deploy_list";
const toolArgs = process.argv[4] ? JSON.parse(process.argv[4]) : {};
const TIMEOUT_MS = Number(process.env.MCP_PROBE_TIMEOUT_MS || 30000);

// stderr:inherit → the core's own logs (AWS errors, rmcp) stream straight through.
const child = spawn(bin, [], { stdio: ["pipe", "pipe", "inherit"], env: process.env });
child.on("error", (e) => {
  console.error(`spawn failed: ${e.message}`);
  process.exit(1);
});

let buf = "";
let nextId = 1;
const pending = new Map();

child.stdout.on("data", (d) => {
  buf += d.toString();
  let nl;
  while ((nl = buf.indexOf("\n")) >= 0) {
    const line = buf.slice(0, nl);
    buf = buf.slice(nl + 1);
    if (!line.trim()) continue;
    let msg;
    try {
      msg = JSON.parse(line);
    } catch {
      console.log("« (non-json)", line);
      continue;
    }
    console.log("«", JSON.stringify(msg));
    if (msg.id != null && pending.has(msg.id)) {
      pending.get(msg.id)(msg);
      pending.delete(msg.id);
    }
  }
});

function send(obj) {
  console.log("»", JSON.stringify(obj));
  child.stdin.write(JSON.stringify(obj) + "\n");
}
function request(method, params) {
  const id = nextId++;
  return new Promise((res) => {
    pending.set(id, res);
    send({ jsonrpc: "2.0", id, method, params });
  });
}

const timer = setTimeout(() => {
  console.error(`\n[timeout after ${TIMEOUT_MS}ms — core did not respond]`);
  child.kill("SIGKILL");
  process.exit(1);
}, TIMEOUT_MS);

try {
  await request("initialize", {
    protocolVersion: "2024-11-05",
    capabilities: {},
    clientInfo: { name: "mcp-probe", version: "0" },
  });
  send({ jsonrpc: "2.0", method: "notifications/initialized" });

  const resp = await request("tools/call", { name: tool, arguments: toolArgs });
  const result = resp.result ?? {};
  const text = result.content?.[0]?.text;

  console.log(`\n=== ${tool} ===`);
  console.log("isError:", result.isError ?? false);
  if (text !== undefined) {
    try {
      console.log(JSON.stringify(JSON.parse(text), null, 2));
    } catch {
      console.log(text);
    }
  } else {
    console.log(JSON.stringify(resp, null, 2));
  }
} finally {
  clearTimeout(timer);
  child.kill("SIGKILL");
}
process.exit(0);
