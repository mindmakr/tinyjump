# Plugins

How tinyjump gains capabilities (AI models, email, payments, device sensors) without
growing its core. Status: server plugins, `tj:ai` and the `tj:agent` library are built, and
the durable cart's shop assistant shows them. Device plugins come next.

## Goals

- **The core stays small and provider-free.** A capability is a plugin, and apps that do not
  use it do not carry it.
- **Replaceable and upgradeable.** You can swap the AI provider, or update a plugin,
  without touching the runtime or the app's code.
- **Isolated.** A plugin that crashes or hangs cannot take the server down.
- **Typed and testable.** Apps call plugins through typed functions. Tests swap in a fake.
- **Secrets stay on the server.** API keys live in the server's environment, never on
  devices.

## The app's side

A plugin's API is a `.tj` module the app imports by name:

```ts
import { complete } from "tj:ai"

@server @read("owner") object Assistant {
  reply: string = ""
  @allow("owner") async ask(question: string) {
    this.reply = await complete("You are a helpful shop assistant.", [{ role: "user", text: question }])
  }
}
```

Each plugin ships that module, `plugin.tj`: types and small `async` functions. Modules of
plugins that ship with tinyjump live in `plugins/<name>/plugin.tj`. The compiler adds them
to any app that imports them, so every host has them. Inside,
they call the built-in `plugin(name, method, args)`. That built-in is allowed only in
plugin modules, so app code has one way to reach a plugin: the typed import. A call becomes
the effect `{ kind: "plugin", plugin, method, args }`. The caller's `await` waits for the
answer, like `fetch`. Plugin calls run on the server, from `@server` objects. A device that
needs a plugin calls a server object, which keeps keys and rules on the server.

## The server's side: plugins are MCP servers

A server plugin is a separate process that speaks the
[Model Context Protocol](https://modelcontextprotocol.io) over stdio. `plugin(name, method,
args)` becomes `tools/call` with `name = method` and `arguments = args`. The answer's
`structuredContent` (or its text, parsed as JSON when possible) is the result. An `isError`
answer becomes an error. Why MCP:

- **One protocol, already standard.** Any existing MCP server (GitHub, Slack, databases,
  search) can be a tinyjump plugin, written in any language.
- **Process isolation.** A plugin is restarted after a crash, with backoff. Calls get a
  timeout (30 seconds by default), and a hung plugin cannot block the runtime.
- **The same tools serve the agent loop.** The agent can be offered a plugin's tools as
  well as the app's own methods.

`tj serve` starts the plugins named in `plugins.json`, next to the app:

```json
{ "ai": { "command": ["tj", "plugin", "ai"], "env": ["ANTHROPIC_API_KEY"] } }
```

- `env` lists the variables passed through to the plugin. A plugin gets nothing else from
  the server's environment, so secrets reach only the plugin that needs them.
- At start, `tj serve` checks that every plugin completes the MCP handshake. A broken
  plugin fails at startup, not mid-request.
- `tj dev` and the tests can use `--plugin ai=fake:<file.json>`, a scripted fake whose
  answers are listed in a file. Tests then stay deterministic and free. The durable cart
  ships `fake-ai.json`.

## Device plugins (later)

Cameras, sensors, files and notifications must run inside the app, so on devices a plugin
is native code registered with the host (Java, Swift or JavaScript) under the same name. It
answers the same effect. The app's side does not change. These come with the device
capabilities work.

## The agent loop, as a plugin and a library

The first plugin is `tj:ai`, a model call. Its implementation is `tj plugin ai`, an MCP
server that calls the Anthropic Messages API with `ANTHROPIC_API_KEY`; the model is
configurable. On top of it, `tj:agent` is a library written in tinyjump:

- **An agent's state is a durable object:** its messages, its pending tool calls and its
  status. It survives restarts and can be inspected like any object.
- **Its tools are the app's own methods,** listed by name, for example
  `tools: ["Cart.add", "Cart.remove"]`. The model sees their names, parameter types and doc
  comments. Each tool call runs as the agent's caller, so access-control rules apply to the
  agent exactly as to a person. An agent acting for a user can do what that user can do,
  and nothing more.
- **Each step is a server call that runs once** (Idempotency-Key). An agent resumes after a
  restart without repeating a tool call.
- **Approval:** a tool can be marked as needing approval. The agent then waits, durably, for
  an `approve()` call from a person.
- **One memory per person.** Whatever goes into an agent's messages (questions, tool
  results) reaches the model again on the next turn of that conversation. Keep a
  conversation per person, `AgentMemory.get(caller.id)` with `@read("owner")`, so one
  person's data never reaches another person's answer. The shop assistant in
  `examples/durable-cart` shares one conversation on purpose, as a public counter for
  anonymous shoppers; do not copy that into an app with signed-in users or `@pii` data.

## Rollout

1. **Done: core.** The `plugin` effect, `tj:` module names, and the rule that only plugin
   modules may call `plugin()`.
2. **Done: server.** `plugins.json`, the stdio MCP client (startup check, timeouts,
   restart after a crash, cleared environment) and the scripted fake.
3. **Done: `tj plugin ai`.** Claude through the Messages API:
   - the model is `claude-opus-5-5` by default (`TJ_AI_MODEL`);
   - effort is `medium` by default (`TJ_AI_EFFORT`);
   - the API's refusal fallback is on;
   - assistant turns are replayed unchanged.
4. **Done: showcase.** `examples/durable-cart/assistant.tj` is a shop assistant. It adds
   and removes items on the shopper's behalf, keeps the model's raw conversation in a
   server-only object, and shows a public transcript.
5. **Done: `tj:agent`** (`plugins/agent/plugin.tj`). `act(system, messages, skills,
   maxSteps)` offers an app's methods as tools and sends each call through
   `invoke(object, method, args)`:
   - each tool call runs as the current caller, under the access rules;
   - arguments the app fixes (`fixed`) cannot be chosen by the model;
   - the assistant may add tea for a shopper but may not restock, unless the shopper is
     staff;
   - `invoke` is allowed only in plugin modules, so app code cannot use it to act as
     someone else.
6. **Done: `tj:privacy`** (`plugins/privacy/plugin.tj`): a person's export and erasure
   from app code (docs/privacy.md). The server carries it out itself rather than through an
   MCP server, because it acts on the runtime's own objects.
