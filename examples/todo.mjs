// A minimal app connected to the bridge through the SDK: an in-memory todo
// list exposing three tools. Run it (with the bridge or `cargo run --example
// serve` running) and the list becomes scriptable:
//
//   node examples/todo.mjs [instanceId]
//   curl http://localhost:7421/apps
//   curl -X POST http://localhost:7421/todo/api/addTask \
//        -H 'content-type: application/json' -d '{"title":"buy milk"}'
//
// In a real web app the identical code runs in the browser; the SDK only
// needs a global WebSocket (browsers, Node >= 22).
import { connectBridge } from "../sdk/dist/index.js";

const tasks = [];
let counter = 0;

const bridge = connectBridge({
  app: "todo",
  title: "Todo",
  description: "A simple in-memory todo list (bridge SDK example).",
  instructions:
    "A todo list. Use addTask to create tasks, listTasks to read them, " +
    "and completeTask to mark one done by id.",
  instanceId: process.argv[2],
  instanceLabel: "example list",
  onStatusChange: (status, error) =>
    console.error(`[todo] ${status}${error ? `: ${error}` : ""}`),
  tools: [
    {
      name: "addTask",
      description: "Add a task to the list. Returns the created task.",
      inputSchema: {
        type: "object",
        properties: { title: { type: "string", description: "Task title" } },
        required: ["title"],
      },
      annotations: { readOnlyHint: false, destructiveHint: false },
      handler: ({ title }) => {
        if (typeof title !== "string" || !title.trim()) {
          throw new Error("title must be a non-empty string");
        }
        const task = { id: `t${++counter}`, title: title.trim(), done: false };
        tasks.push(task);
        return { task };
      },
    },
    {
      name: "listTasks",
      description: "List all tasks.",
      annotations: { readOnlyHint: true },
      handler: () => ({ tasks }),
    },
    {
      name: "completeTask",
      description: "Mark a task as done by id.",
      inputSchema: {
        type: "object",
        properties: { id: { type: "string" } },
        required: ["id"],
      },
      handler: ({ id }) => {
        const task = tasks.find((t) => t.id === id);
        if (!task) throw new Error(`No task with id ${id}`);
        task.done = true;
        return { task };
      },
    },
  ],
});

const shutdown = () => {
  bridge.close();
  process.exit(0);
};
process.on("SIGINT", shutdown);
process.on("SIGTERM", shutdown);
