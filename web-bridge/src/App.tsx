import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";

type InstanceStatus = {
  instanceId: string;
  label: string | null;
  connectedAt: number;
  toolCount: number;
};

type AppStatus = {
  app: string;
  title: string | null;
  description: string | null;
  instances: InstanceStatus[];
};

type Status = {
  running: boolean;
  port: number;
  apps: AppStatus[];
};

function App() {
  const [status, setStatus] = useState<Status | null>(null);

  useEffect(() => {
    let active = true;
    const poll = async () => {
      try {
        const s = await invoke<Status>("get_status");
        if (active) setStatus(s);
      } catch {
        // server not up yet; keep polling
      }
    };
    void poll();
    const timer = setInterval(poll, 1000);
    return () => {
      active = false;
      clearInterval(timer);
    };
  }, []);

  const running = status?.running ?? false;
  const port = status?.port ?? 0;
  const apps = status?.apps ?? [];
  const base = `http://localhost:${port}`;

  return (
    <main className="shell">
      <header className="header">
        <span className="logo" aria-hidden>
          ⇄
        </span>
        <div>
          <h1>Web Bridge</h1>
          <p>
            Local host for web apps — exposes their APIs and MCP to agents and
            scripts
          </p>
        </div>
      </header>

      <section className="card">
        <div className="row">
          <span className="row-label">Server</span>
          <span className="row-value">
            <Dot state={running ? "on" : "warn"} />
            {running ? (
              <>
                Running on port <span className="badge">{port}</span>
              </>
            ) : (
              "Starting…"
            )}
          </span>
        </div>
        <div className="row">
          <span className="row-label">Discovery</span>
          <CopyField value={`${base}/apps`} />
        </div>
      </section>

      <section>
        <h2 className="section-title">
          Connected apps
          {apps.length > 0 && <span className="count">{apps.length}</span>}
        </h2>
        {apps.length === 0 ? (
          <div className="card empty">
            <p>
              No apps connected yet. A web app connects out to the bridge with
              the <code>web-bridge-sdk</code> npm package:
            </p>
            <pre>{`import { connectBridge } from "web-bridge-sdk";

connectBridge({
  app: "my-app",
  description: "What the app does",
  instructions: "How agents should use the tools",
  tools: [{
    name: "doThing",
    description: "…",
    handler: (payload) => ({ ok: true }),
  }],
});`}</pre>
            <p>
              Once connected, it appears here and in the discovery document at{" "}
              <code>{base}/apps</code>.
            </p>
          </div>
        ) : (
          apps.map((app) => <AppCard key={app.app} app={app} base={base} />)
        )}
      </section>

      <p className="footnote">localhost only · not exposed to the network</p>
    </main>
  );
}

function AppCard({ app, base }: { app: AppStatus; base: string }) {
  return (
    <div className="card app-card">
      <div>
        <span className="app-title">{app.title || app.app}</span>
        <span className="app-slug">/{app.app}</span>
      </div>
      {app.description && <p className="app-desc">{app.description}</p>}
      {app.instances.map((inst) => (
        <Instance key={inst.instanceId} app={app.app} inst={inst} base={base} />
      ))}
    </div>
  );
}

function Instance({
  app,
  inst,
  base,
}: {
  app: string;
  inst: InstanceStatus;
  base: string;
}) {
  const prefix = `${base}/${app}/${inst.instanceId}`;
  const disconnect = async () => {
    try {
      await invoke("disconnect_instance", { app, instance: inst.instanceId });
    } catch {
      // the poll will reflect the real state
    }
  };
  return (
    <div className="instance">
      <div className="instance-head">
        <span className="row-value">
          <Dot state="on" />
          <strong>{inst.label || inst.instanceId}</strong>
          {inst.label && <span className="muted">{inst.instanceId}</span>}
          <span className="muted">
            · {inst.toolCount} tool{inst.toolCount === 1 ? "" : "s"}
          </span>
        </span>
        <button className="btn danger" onClick={disconnect}>
          Disconnect
        </button>
      </div>
      <div className="endpoints">
        <Endpoint label="MCP" value={`${prefix}/mcp`} />
        <Endpoint label="API" value={`${prefix}/api`} />
      </div>
    </div>
  );
}

function Endpoint({ label, value }: { label: string; value: string }) {
  return (
    <div className="endpoint">
      <span className="endpoint-label">{label}</span>
      <CopyField value={value} />
    </div>
  );
}

function CopyField({ value }: { value: string }) {
  const [copied, setCopied] = useState(false);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(value);
      setCopied(true);
      setTimeout(() => setCopied(false), 1200);
    } catch {
      // clipboard unavailable; ignore
    }
  };
  return (
    <span className="copy-field">
      <code>{value}</code>
      <button
        className="btn"
        onClick={copy}
        title="Copy to clipboard"
        aria-label={`Copy ${value}`}
      >
        {copied ? "✓" : "Copy"}
      </button>
    </span>
  );
}

function Dot({ state }: { state: "on" | "warn" | "off" }) {
  return <span className={`dot dot-${state}`} />;
}

export default App;
