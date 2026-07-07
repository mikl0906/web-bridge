# web-bridge-sdk

Connect a web app to the [Web Bridge](../README.md) desktop hub and expose its
operations to local agents and scripts — as an MCP server and a plain REST API
at `http://localhost:7421/<app>/<instanceId>/…`.

Zero dependencies; works anywhere with a global `WebSocket` (browsers,
Node ≥ 22).

## Usage

```ts
import { connectBridge } from "web-bridge-sdk";

const bridge = connectBridge({
  app: "todo",                       // URL slug: /todo/…
  title: "Todo",
  description: "A simple todo list", // shown in GET /apps discovery
  instructions:                      // served to agents via MCP initialize
    "Use addTask to create tasks; listTasks to read them.",
  tools: [
    {
      name: "addTask",
      description: "Add a task to the list. Returns the created task.",
      inputSchema: {
        type: "object",
        properties: { title: { type: "string" } },
        required: ["title"],
      },
      annotations: { readOnlyHint: false },
      handler: ({ title }) => store.add(title),
    },
  ],
});
```

That's it — call `connectBridge` once at startup. It retries with exponential
backoff until the bridge appears and reconnects if it drops, so the order in
which the app and the bridge start doesn't matter.

## Notes

- **Handlers** can be async. The return value is the tool result; a thrown
  error becomes `{ message }` (plus `issues`/`details` if present on the
  error, so zod-style validation errors pass through intact).
- **Tool names** double as REST paths: `POST …/api/<name>` with the JSON body
  as the payload.
- **`instanceId`** is optional. Omit it for a bridge-assigned random id
  (announced via the `welcome` frame — read `bridge.instanceId` /
  `bridge.mcpUrl` once connected, or `subscribe` to changes). Provide one to
  keep URLs stable across reloads — but persist it **per tab**
  (sessionStorage): the id names one live connection, and a second connection
  claiming it takes over while the first is rejected for good.
- **Dynamic catalogs**: `bridge.addTool(…)` / `bridge.removeTool(name)` /
  `bridge.update({ instanceLabel: "new doc name" })` re-announce immediately.
- **Status**: `bridge.status` is `"disconnected" | "connecting" | "connected"
  | "error"`; observe it with `subscribe(listener)` or the `onStatusChange`
  option. `bridge.close()` disconnects permanently.
- **Port**: defaults to 7421; a `?bridgePort=` URL param or the `port` option
  overrides it.

## Building

```sh
npm install
npm run build   # emits dist/ (ESM + type declarations)
```
