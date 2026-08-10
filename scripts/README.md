# scripts

## mcp-probe.mjs

Drive the `oab-mcp` core directly over stdio — **without the desktop app** — for
fast credential/region/tool debugging. Same MCP flow the Tauri bridge uses
(`initialize` → `tools/call`), so it exercises the exact AWS credential
resolution the app would, but in a terminal loop with the raw JSON-RPC and the
core's stderr in view.

```sh
# against the core bundled inside the shipped .app, with your profile:
AWS_PROFILE=brettchien AWS_REGION=ap-east-2 OAB_CLUSTER=oab \
  node scripts/mcp-probe.mjs "/Applications/OAB Studio.app/Contents/MacOS/oab-mcp"

# a specific tool + args:
node scripts/mcp-probe.mjs ./oab-mcp deploy_get '{"service":"oab-prod-orca"}'
```

Whatever `AWS_*` / `AWS_PROFILE` you export is exactly what the app's sidecar
would inherit — so wrong account/region shows up here as an `isError` result or
a stderr line, the same credential-drift symptom, in one shot.
