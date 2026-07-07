/**
 * web-bridge-sdk — connect a web app to the Web Bridge desktop hub.
 *
 * The bridge is a small desktop app listening on localhost (port 7421 by
 * default). Your app connects *out* to it over a WebSocket, announces a name,
 * a description, MCP usage instructions, and a tool catalog — and the bridge
 * exposes those tools to any local client (agents, scripts, MCP clients) at
 *
 *   http://localhost:7421/<app>/<instanceId>/mcp        (MCP Streamable HTTP)
 *   http://localhost:7421/<app>/<instanceId>/api/<tool> (plain REST)
 *
 * Everything stays on the machine; the bridge never understands your domain —
 * it just relays tool calls to the handlers you register here.
 *
 * ```ts
 * import { connectBridge } from "web-bridge-sdk";
 *
 * const bridge = connectBridge({
 *   app: "todo",
 *   title: "Todo",
 *   description: "A simple todo list",
 *   instructions: "Use addTask to create tasks. Tasks have a title and done flag.",
 *   tools: [
 *     {
 *       name: "addTask",
 *       description: "Add a task to the list",
 *       inputSchema: {
 *         type: "object",
 *         properties: { title: { type: "string" } },
 *         required: ["title"],
 *       },
 *       handler: ({ title }) => store.add(title),
 *     },
 *   ],
 * });
 * ```
 *
 * The connection retries with exponential backoff until the bridge appears
 * and reconnects if it drops, so it is safe to call `connectBridge` once at
 * app startup regardless of whether the bridge is already running.
 */

/** Connection lifecycle, observable via `subscribe`/`onStatusChange`. */
export type BridgeStatus = "disconnected" | "connecting" | "connected" | "error";

/**
 * One operation your app exposes. `name` becomes both the MCP tool name and
 * the REST path segment (`POST …/api/<name>`).
 */
export interface BridgeTool {
  /** Tool name: 1-64 chars of [a-zA-Z0-9_-]. */
  name: string;
  /** What the tool does — shown to agents in `tools/list`. */
  description: string;
  /**
   * JSON Schema for the tool's payload, rooted at `type: "object"` (an MCP
   * requirement). Defaults to an unconstrained object.
   */
  inputSchema?: Record<string, unknown>;
  /** MCP tool annotations, e.g. `{ readOnlyHint: true, destructiveHint: false }`. */
  annotations?: Record<string, unknown>;
  /**
   * Executes the call. The return value is serialized as the result; a thrown
   * error becomes `{ message }` (plus `issues`/`details` when present on the
   * error, so zod-style validation errors survive the trip).
   */
  handler: (payload: any) => unknown | Promise<unknown>;
}

export interface BridgeOptions {
  /**
   * App name — the first URL path segment. Lowercase slug: 1-64 chars of
   * [a-z0-9_-], not one of the reserved route words (api, mcp, ws, apps,
   * status, tools).
   */
  app: string;
  /** Human-readable app name for the bridge UI and discovery. */
  title?: string;
  /** One-paragraph description of the app, shown in `GET /apps` discovery. */
  description?: string;
  /**
   * Usage instructions for agents, served through MCP `initialize` — how to
   * combine the tools, domain conventions, caveats.
   */
  instructions?: string;
  /**
   * Stable instance id (same slug rules as `app`). Provide one to keep URLs
   * stable across page reloads; omit to let the bridge assign a random one,
   * announced via `welcome` (see `instanceId`). Persist it per tab
   * (sessionStorage), not per origin (localStorage): the id names one live
   * connection, and when a second connection claims it the bridge hands the
   * id to the newcomer and rejects the older one for good.
   */
  instanceId?: string;
  /** Label for this instance, e.g. the open document's name. */
  instanceLabel?: string;
  /** Initial tool catalog; extend later with `addTool`. */
  tools?: BridgeTool[];
  /** Bridge port. Default: `?bridgePort=` URL param if present, else 7421. */
  port?: number;
  /** Full WebSocket URL override; wins over `port`. */
  url?: string;
  /**
   * How many consecutive failed connection attempts before giving up
   * (a successful connection resets the count). Default: retry forever.
   */
  maxAttempts?: number;
  /** Called on every status change. */
  onStatusChange?: (status: BridgeStatus, error: string | null) => void;
}

export interface BridgeConnection {
  readonly status: BridgeStatus;
  /** Human-readable reason when `status` is "error", else null. */
  readonly error: string | null;
  /** Instance id assigned/confirmed by the bridge; null until connected. */
  readonly instanceId: string | null;
  /** This instance's base URL, e.g. http://localhost:7421/todo/a1b2c3d4. */
  readonly baseUrl: string | null;
  /** This instance's MCP endpoint; null until connected. */
  readonly mcpUrl: string | null;
  /** Register (or replace) a tool; re-announces the catalog if connected. */
  addTool(tool: BridgeTool): void;
  /** Unregister a tool by name; re-announces the catalog if connected. */
  removeTool(name: string): void;
  /** Update metadata (e.g. `instanceLabel` after a document rename). */
  update(
    patch: Partial<
      Pick<BridgeOptions, "title" | "description" | "instructions" | "instanceLabel">
    >,
  ): void;
  /** Subscribe to status changes; returns an unsubscribe function. */
  subscribe(listener: () => void): () => void;
  /** Disconnect permanently (cancels reconnection). */
  close(): void;
}

export const DEFAULT_BRIDGE_PORT = 7421;

const RECONNECT_BASE_MS = 1000;
const RECONNECT_MAX_MS = 30000;

const SLUG_RE = /^[a-z0-9][a-z0-9_-]{0,63}$/;
const RESERVED_SLUGS = new Set(["api", "mcp", "ws", "apps", "status", "tools"]);

/** Frames relayed by the bridge: a tool call to execute. */
type BridgeRequest = { id: string; action: string; payload?: unknown };

/**
 * Connect to the bridge and keep the connection alive. Returns immediately;
 * the connection is established (and re-established) in the background.
 */
export function connectBridge(options: BridgeOptions): BridgeConnection {
  validateSlug(options.app, "app");
  if (options.instanceId !== undefined) validateSlug(options.instanceId, "instanceId");

  const tools = new Map<string, BridgeTool>();
  for (const tool of options.tools ?? []) tools.set(tool.name, tool);

  const meta = {
    title: options.title,
    description: options.description,
    instructions: options.instructions,
    instanceLabel: options.instanceLabel,
  };

  const url = options.url ?? `ws://localhost:${resolvePort(options.port)}/ws/app`;
  const maxAttempts = options.maxAttempts ?? Infinity;

  let status: BridgeStatus = "disconnected";
  let error: string | null = null;
  let instanceId: string | null = null;
  let baseUrl: string | null = null;
  let mcpUrl: string | null = null;

  let ws: WebSocket | null = null;
  let closed = false;
  let attempts = 0;
  let retryTimer: ReturnType<typeof setTimeout> | null = null;

  const listeners = new Set<() => void>();

  function setStatus(next: BridgeStatus, message: string | null = null) {
    status = next;
    error = message;
    if (next !== "connected") {
      instanceId = null;
      baseUrl = null;
      mcpUrl = null;
    }
    for (const l of listeners) l();
    options.onStatusChange?.(status, error);
  }

  function helloFrame(): string {
    return JSON.stringify({
      type: "hello",
      protocolVersion: 1,
      app: options.app,
      title: meta.title,
      description: meta.description,
      instructions: meta.instructions,
      instanceId: options.instanceId,
      instanceLabel: meta.instanceLabel,
      tools: [...tools.values()].map((t) => ({
        name: t.name,
        description: t.description,
        inputSchema: t.inputSchema ?? { type: "object" },
        annotations: t.annotations ?? {},
      })),
    });
  }

  /** Re-announce metadata + tool catalog on the live socket, if any. */
  function resendHello() {
    if (ws && ws.readyState === WebSocket.OPEN) ws.send(helloFrame());
  }

  async function handleRequest(socket: WebSocket, req: BridgeRequest) {
    let reply: Record<string, unknown>;
    const tool = tools.get(req.action);
    if (!tool) {
      reply = {
        id: req.id,
        ok: false,
        error: { message: `Unknown tool: ${req.action}` },
      };
    } else {
      try {
        const result = await tool.handler(req.payload ?? {});
        reply = { id: req.id, ok: true, result: result ?? null };
      } catch (e) {
        reply = { id: req.id, ok: false, error: formatError(e) };
      }
    }
    // Only reply if this socket is still the active one.
    if (ws === socket && socket.readyState === WebSocket.OPEN) {
      socket.send(JSON.stringify(reply));
    }
  }

  function connect() {
    if (closed) return;
    setStatus("connecting");

    let socket: WebSocket;
    try {
      socket = new WebSocket(url);
    } catch (e) {
      setStatus("error", e instanceof Error ? e.message : String(e));
      scheduleReconnect();
      return;
    }
    ws = socket;

    socket.onopen = () => {
      if (ws !== socket) return;
      attempts = 0; // connected — reset the backoff for any future drop
      socket.send(helloFrame());
      // Status flips to "connected" when the bridge's `welcome` arrives.
    };

    socket.onmessage = (event) => {
      if (ws !== socket) return;
      let frame: any;
      try {
        frame = JSON.parse(event.data as string);
      } catch {
        return; // ignore malformed frames
      }
      if (frame.type === "welcome") {
        instanceId = frame.instanceId ?? null;
        baseUrl = frame.baseUrl ?? null;
        mcpUrl = frame.mcpUrl ?? null;
        setStatus("connected");
        return;
      }
      if (frame.type === "error") {
        // The bridge rejected us (bad app name, reserved slug, …). Retrying
        // would fail identically, so stop for good.
        closed = true;
        setStatus("error", String(frame.message ?? "Rejected by the bridge."));
        return;
      }
      if (typeof frame.id === "string" && typeof frame.action === "string") {
        void handleRequest(socket, frame as BridgeRequest);
      }
    };

    socket.onclose = () => {
      if (ws !== socket) return;
      ws = null;
      if (closed) {
        if (status !== "error") setStatus("disconnected");
        return;
      }
      setStatus("disconnected");
      scheduleReconnect();
    };

    // Errors always produce a close event; reconnection is handled there.
    socket.onerror = () => {};
  }

  function scheduleReconnect() {
    if (closed || retryTimer) return;
    if (attempts >= maxAttempts) {
      closed = true;
      setStatus(
        "error",
        `Couldn't reach the bridge at ${url} after ${attempts} attempts.`,
      );
      return;
    }
    const delay = Math.min(RECONNECT_BASE_MS * 2 ** attempts, RECONNECT_MAX_MS);
    attempts++;
    retryTimer = setTimeout(() => {
      retryTimer = null;
      connect();
    }, delay);
  }

  connect();

  return {
    get status() {
      return status;
    },
    get error() {
      return error;
    },
    get instanceId() {
      return instanceId;
    },
    get baseUrl() {
      return baseUrl;
    },
    get mcpUrl() {
      return mcpUrl;
    },
    addTool(tool: BridgeTool) {
      tools.set(tool.name, tool);
      resendHello();
    },
    removeTool(name: string) {
      if (tools.delete(name)) resendHello();
    },
    update(patch) {
      Object.assign(meta, patch);
      resendHello();
    },
    subscribe(listener: () => void) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    close() {
      closed = true;
      if (retryTimer) {
        clearTimeout(retryTimer);
        retryTimer = null;
      }
      const socket = ws;
      ws = null;
      socket?.close();
      setStatus("disconnected");
    },
  };
}

/** `?bridgePort=` URL param (browser only) beats the default; explicit wins. */
function resolvePort(explicit?: number): number {
  if (explicit !== undefined) return explicit;
  if (typeof location !== "undefined") {
    const param = new URLSearchParams(location.search).get("bridgePort");
    const parsed = param ? Number.parseInt(param, 10) : NaN;
    if (Number.isFinite(parsed) && parsed >= 1 && parsed <= 65535) return parsed;
  }
  return DEFAULT_BRIDGE_PORT;
}

function validateSlug(s: string, what: string): void {
  if (!SLUG_RE.test(s) || RESERVED_SLUGS.has(s)) {
    throw new Error(
      `Invalid ${what} '${s}': use 1-64 chars of [a-z0-9_-], starting with a letter or digit, and not a reserved word.`,
    );
  }
}

function formatError(e: unknown): { message: string; issues?: unknown; details?: unknown } {
  const out: { message: string; issues?: unknown; details?: unknown } = {
    message: e instanceof Error ? e.message : String(e),
  };
  if (e && typeof e === "object") {
    if ("issues" in e) out.issues = (e as any).issues;
    if ("details" in e) out.details = (e as any).details;
  }
  return out;
}
