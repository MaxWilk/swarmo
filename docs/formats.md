# Swarmo file formats

A Swarmo workspace is a directory of plain JSON files. Nothing is hidden in a
database, and everything except `.swarmo/` is meant to be committed to git.

```
my-api-workspace/
├── swarmo.json                     workspace manifest
├── .gitignore                      contains ".swarmo/"
├── environments/
│   └── local.env.json
├── collections/
│   └── Orders API/                 a collection is a directory
│       ├── collection.json         collection-level auth, headers, scripts
│       ├── Create Order.req.json   an HTTP request
│       ├── Get Order.grpc.json     a gRPC request
│       ├── Order Feed.ws.json      a WebSocket request
│       └── Admin/                  a nested folder
│           ├── folder.json         optional folder-level settings
│           └── Delete Order.req.json
├── protos/                         your .proto files (referenced, not managed)
├── loadtests/
│   ├── checkout-smoke.load.json    declarative scenario
│   └── shopper.user.js             scripted virtual users
└── .swarmo/                        gitignored, per-machine
    ├── runs/<runId>/{run.json,timeline.json}
    ├── history.json                every send, credential headers redacted
    └── secrets.env.json
```

Nodes are addressed by a **ref**: the workspace-relative path with forward
slashes, e.g. `collections/Orders API/Create Order.req.json`.

Display names and filenames are kept separate. A request called
`Has/Illegal:Chars?` is stored as `Has_Illegal_Chars_.req.json` with its real
name inside the file, and Windows device names like `CON` get an underscore
prefix. Colliding names get ` 2`, ` 3` suffixes.

Every write is atomic: Swarmo writes a temp file in the same directory and
renames it over the target, so an interrupted save never truncates your work.

---

## `swarmo.json`

```json
{
  "version": 1,
  "name": "My API Workspace",
  "activeEnvironment": "local"
}
```

Approvals are deliberately **not** stored here. The hosts you have confirmed
for load testing and the auth commands you have allowed to run live in the
app's own config directory, keyed by the workspace's path, so a workspace you
clone cannot arrive having approved itself. Manifests written by older
versions may still carry `approvedLoadHosts` or `approvedAuthCommands`; those
keys are ignored and dropped on the next save. Settings → Reset load-test host
approvals makes Swarmo ask again before the next run.

## `environments/<name>.env.json`

```json
{
  "version": 1,
  "name": "local",
  "variables": [
    { "key": "baseUrl", "value": "http://localhost:8080", "secret": false, "enabled": true },
    { "key": "apiKey",  "value": "",                      "secret": true,  "enabled": true }
  ]
}
```

A variable marked `"secret": true` always has an empty `value` in this file. Its
real value lives in `.swarmo/secrets.env.json`, keyed `"<envName>/<key>"`, which
is gitignored. That is what lets you commit the environment without committing
the credential.

### Variable resolution

`{{name}}` is substituted in URLs, params, headers, auth fields and bodies.
First match wins:

1. runtime overrides — set by scripts (`sw.env.set`) or by load-test captures
2. the active environment, with secrets merged in

Nested references resolve up to five levels, so `{{baseUrl}}` may itself contain
`{{host}}`. An unresolved variable is left as literal `{{name}}` text and
reported to the UI rather than silently becoming an empty string.

## `*.req.json`

```json
{
  "version": 1,
  "id": "6f9a1c9e-…",
  "name": "Create Order",
  "method": "POST",
  "url": "{{baseUrl}}/orders",
  "params":  [{ "key": "dryRun", "value": "true", "enabled": false }],
  "headers": [{ "key": "Content-Type", "value": "application/json", "enabled": true }],
  "auth": { "type": "bearer", "token": "{{apiKey}}" },
  "body": { "type": "json", "text": "{\n  \"sku\": \"A-1\"\n}" },
  "scripts": {
    "preRequest": "sw.env.set('ts', Date.now());",
    "postResponse": "sw.test('created', () => sw.expect(sw.response.status).toBe(201));"
  },
  "settings": { "followRedirects": true, "timeoutMs": 30000, "verifyTls": true }
}
```

**`auth.type`** is one of:

| type | fields |
|---|---|
| `inherit` | none — take the nearest ancestor's auth (the default) |
| `none` | none — explicitly send no auth, stopping inheritance |
| `basic` | `username`, `password` |
| `bearer` | `token` |
| `apiKeyHeader` | `headerName`, `value` |

**`body.type`** is one of `none`, `json`, `text`, `form`, `multipart`,
`graphql`, `binary`. A `graphql` body is sent as a JSON POST of
`{query, variables}`. A `multipart` part has `kind: "text" | "file"`, where a
`file` part's `value` is a path on disk.

## `*.grpc.json`

A gRPC request lives in the same tree as HTTP requests, in the same collections
and folders, and can be a step in the same load scenario.

```json
{
  "version": 1,
  "id": "…",
  "name": "Get Order",
  "address": "{{grpcHost}}",
  "protoSource": { "kind": "directory", "root": "protos", "entryFiles": [] },
  "service": "orders.v1.OrderService",
  "method": "GetOrder",
  "metadata": [{ "key": "x-tenant", "value": "acme", "enabled": true }],
  "auth": { "type": "bearer", "token": "{{token}}" },
  "message": "{\n  \"order_id\": \"{{orderId}}\"\n}",
  "scripts": { "preRequest": "", "postResponse": "" },
  "settings": {
    "timeoutMs": 30000,
    "verifyTls": true,
    "maxResponseBytes": 16777216
  }
}
```

**`address`** is `http://host:port` for plaintext h2c or `https://host:port` for
TLS. A scheme-less address is treated as `https://`.

**`protoSource`** is one of three:

- `{"kind": "directory", "root": "protos", "entryFiles": []}` — point at a
  folder and Swarmo works the rest out. This is the easy path, and the one to
  reach for first.

  It scans the folder recursively, reads the `import` statements it finds, and
  derives the import roots from them, so you do not have to know which level the
  imports were written against. Picking the folder *named after your service*
  works even when its imports are relative to the directory above, and a
  dependency living in a sibling directory is picked up too (it looks up to five
  levels above the folder you chose, and only for imports it cannot otherwise
  satisfy).

  A stray `.proto` in the tree that does not compile on its own — vendored trees
  collect these — does not sink the whole folder: Swarmo retries with just the
  files that declare a `service`, which is all a gRPC client needs.

  `entryFiles` optionally narrows what gets compiled (their imports are still
  pulled in); empty compiles everything found. If an import genuinely is not
  present anywhere, the error names the missing files.
- `{"kind": "files", "files": [...], "includePaths": [...]}` — name the files
  explicitly. `includePaths` are the import roots (the `-I` flags); when
  omitted, each file's own directory is used.
- `{"kind": "reflection"}` — ask the server for its schema over the gRPC
  reflection service. Nothing else is needed, but the server must have
  reflection enabled, and many do not (TensorFlow Serving, for one).

  Fetching a schema this way is itself an RPC, so a server that requires auth
  will reject it like any other call. The request's metadata — including
  whatever its `auth` setting produces — is sent with the reflection request
  too, so a bearer token set on the request or its collection covers both.

Relative paths are workspace-relative. Swarmo compiles `.proto` files itself
and never needs a `protoc` binary installed.

**`metadata`** is the gRPC equivalent of headers. Keys are lowercased on send. A
key ending in `-bin` carries binary metadata, so its value must be base64.
Keys beginning `grpc-` are reserved by the protocol and are rejected — use the
`timeoutMs` setting rather than sending `grpc-timeout` yourself.

**`auth`** uses the same shapes HTTP requests use (`inherit`, `none`, `basic`,
`bearer`, `apiKeyHeader`) and becomes a metadata entry at send time: `bearer`
and `basic` produce `authorization`, and `apiKeyHeader` uses its `headerName`
as the metadata key. An explicit metadata row of the same key wins, so a request
can always override what it inherited.

**`settings.maxResponseBytes`** caps the response message size. gRPC libraries
normally default to 4 MB, which real payloads exceed more often than people
expect; Swarmo allows 16 MB by default and lets you change it per request.

**`message`** is the request message in protobuf-JSON (the proto3 JSON mapping,
the same dialect `grpcurl` uses): field names in lowerCamelCase, enums by name,
64-bit integers as strings, `Timestamp` as an RFC 3339 string.

**Inheritance.** Collection and folder *scripts* and *auth* apply to gRPC
requests exactly as they do to HTTP ones — one bearer token on a collection
covers every call inside it, with the same nearest-explicit-wins rule. Plain
collection and folder *headers* do not: metadata is lowercase-only, has binary-key
conventions and reserves the `grpc-` prefix, so quietly reinterpreting an HTTP
header as metadata would be a surprise rather than a convenience.

**Streaming.** Server-, client- and bidirectional-streaming methods use the
same file. For a client or bidirectional stream, `message` is a JSON array of
messages sent in order; a single object is treated as a list of one. For a
server or bidirectional stream, `settings.streamMaxMessages` stops reading
after that many messages and closes the stream. Leave it out to read until the
server ends the stream or the deadline passes, but set it for a feed that never
ends, or every load-test iteration runs to its timeout. Under load, a stream is
one sample whose latency is the stream's whole lifetime.

## `*.ws.json`

A WebSocket request is a short scripted session rather than an open console:
connect, send each message in order, wait as each one says, then close.

```json
{
  "version": 1,
  "id": "…",
  "name": "Order Feed",
  "url": "wss://{{host}}/ws",
  "headers": [{ "key": "x-tenant", "value": "acme", "enabled": true }],
  "subprotocols": ["graphql-transport-ws"],
  "auth": { "type": "inherit" },
  "messages": [
    { "kind": "text", "body": "{\"type\":\"subscribe\"}", "wait": { "kind": "reply" }, "enabled": true },
    { "kind": "text", "body": "ping", "wait": { "kind": "count", "count": 3 }, "enabled": true },
    { "kind": "binary", "body": "AAEC", "wait": { "kind": "millis", "ms": 2000 }, "enabled": true }
  ],
  "settings": {
    "connectTimeoutMs": 10000,
    "timeoutMs": 10000,
    "verifyTls": true,
    "closeAfter": true
  }
}
```

**`url`** is `ws://` or `wss://`; `http://` and `https://` are accepted and
mapped. **`headers`** and **`auth`** go on the handshake, and inherit from
collections and folders the way HTTP requests do. **`subprotocols`** are
offered as `Sec-WebSocket-Protocol`, in order.

Each message's **`kind`** is `text`, or `binary` with `body` as base64.
Bodies are interpolated like any other body. **`wait`** says what happens before
the next message is sent:

- `none` sends and moves straight on; no latency is recorded for it.
- `reply` waits for the next frame. Latency is send to first frame back.
- `count` waits for that many frames.
- `millis` collects whatever arrives for that long.

**`settings.timeoutMs`** caps each wait. A wait that runs out fails that message,
not the whole session. **`closeAfter`** closes cleanly once every message has
been handled; turn it off for a subscription that should run until the server
closes it, and end the list with a `millis` wait. A server that closes the
connection before every message has been sent fails the session.

WebSocket requests do not run scripts: a session has no single response for a
post-response script to check.

## `collection.json` and `folder.json`

```json
{
  "version": 1,
  "id": "…",
  "name": "Orders API",
  "headers": [{ "key": "Accept", "value": "application/json", "enabled": true }],
  "auth": { "type": "bearer", "token": "{{apiKey}}" },
  "scripts": { "preRequest": "", "postResponse": "" }
}
```

Inheritance, outermost to innermost:

- **Headers** merge, and the innermost level wins on a key conflict
  (case-insensitively).
- **Auth** uses the nearest explicit declaration. A request set to `inherit`
  walks up until it finds something that is not `inherit`; `none` stops the walk.
- **Scripts** all run. Pre-request scripts run outside-in (collection, then
  folder, then request); post-response scripts run inside-out.

## `*.load.json` — declarative scenarios

```json
{
  "version": 1,
  "name": "checkout-smoke",
  "mode": "closed",
  "stages": [
    { "durationSec": 30, "target": 20 },
    { "durationSec": 60, "target": 20 },
    { "durationSec": 10, "target": 0 }
  ],
  "maxVus": 200,
  "environment": "local",
  "steps": [
    {
      "requestRef": "collections/Orders API/Login.req.json",
      "thinkTimeMs": [500, 1500],
      "capture": [{ "from": "body", "jsonPath": "$.token", "as": "token" }],
      "tag": "login"
    },
    {
      "requestRef": "collections/Orders API/Create Order.req.json",
      "thinkTimeMs": [200, 800]
    }
  ],
  "thresholds": [
    { "metric": "http_req_duration", "stat": "p95", "op": "<", "valueMs": 500 },
    { "metric": "http_req_failed",   "stat": "rate", "op": "<", "value": 0.01 }
  ],
  "verifyTls": true,
  "timeoutMs": 30000,
  "newConnectionPerIteration": false
}
```

**`mode`** is `closed` (stages ramp the number of concurrent virtual users, each
looping over the steps) or `open` (stages ramp arrivals per second, each arrival
running the steps once).

**Stages** ramp linearly from the previous stage's target, and the first ramps
up from zero. A stage with `durationSec: 0` jumps straight to its target. Above,
load ramps 0 → 20 users over 30s, holds for 60s, then drains over 10s.

That "from the previous target" rule catches people out: a lone
`{"durationSec": 60, "target": 100}` sweeps 0 → 100 and averages 50, rather than
holding 100.

**To hold a flat rate**, set `arrivalRatePerSec` instead. It applies for the
whole run, stage targets are ignored, and `durationSec` supplies the length when
there are no stages at all:

```json
{
  "mode": "open",
  "arrivalRatePerSec": 150,
  "durationSec": 300,
  "maxVus": 400
}
```

`arrivalRatePerSec` is open mode only — in closed mode the target is a number of
virtual users, not a rate, and the combination is refused rather than guessed at.

**Sizing `maxVus` in open mode.** It is not the load; it is the pool of workers
that execute arrivals. By Little's Law you need roughly `rate × latency` of
them, so 150/s against a 1.5s p95 wants ~225, and more for headroom. Too few and
arrivals queue: `droppedIterations` and `vusSaturated` in the run summary tell
you that happened, and until they are zero the latency figures are measuring
Swarmo rather than the server.

**`thinkTimeMs: [min, max]`** sleeps a uniform random time after that step.

**`capture`** pulls a value out of a response into a variable that later steps
can use as `{{name}}`. `from: "body"` takes a `jsonPath` supporting dot access
and numeric indices (`$.data.items[0].id`); `from: "header"` takes a `name`.
This is what lets a scriptless scenario do log-in-then-act.

**`tag`** names the metric series. Without one, the request's name is used, so
`/orders/1` and `/orders/2` aggregate together instead of exploding into
thousands of series.

**gRPC steps.** A step whose `requestRef` points at a `*.grpc.json` works
exactly like an HTTP one: same think time, same tags, same captures (the
response is decoded to JSON first, so `$.token` works unchanged), same
thresholds. HTTP and gRPC steps mix freely in a single scenario. Two extra
notes:

- The `statusCodes` table then holds both HTTP codes and gRPC codes. They never
  collide, because gRPC uses 0–16 and HTTP starts at 100; `0` means gRPC OK.
- `"grpcChannels": 8` sets how many HTTP/2 connections to open per gRPC address.
  One connection shares a single flow-control window, so a pool matters under
  load; this is the gRPC analogue of HTTP connection reuse. The default scales
  with the CPU count, and virtual users are spread across the pool.

**WebSocket steps.** A step whose `requestRef` points at a `*.ws.json` runs one
whole session per iteration. It records a `<tag> connect` sample for the
handshake, plus one sample under the step's tag for every message that waited
for an answer. A session has no single body, so a WebSocket step cannot capture
values for later steps. A `ws://` or `wss://` host is confirmed before the run
like any other.

**Thresholds** are checked every second during the run and again at the end.
`metric` is one of `http_req_duration`, `http_req_failed`, `http_reqs`,
`checks`; `stat` is one of `p50 p90 p95 p99 avg max rate count`; `op` is one of
`< <= > >=`. Duration metrics use `valueMs`, rate and count metrics use `value`.
The `http_` prefix is historical: these metrics cover every request in the run,
gRPC included, so one latency budget applies across protocols.
A failing threshold marks the run failed; adding `"abortOnFail": true` also
stops it.

## `*.user.js` — scripted virtual users

See [`scripting.md`](scripting.md) for the full `ctx` API. In short:

```js
export const options = {
  environment: "local",
  mode: "closed",
  stages: [{ durationSec: 60, target: 50 }],
  maxVus: 200,
  userMix: [
    { exec: "shopper", weight: 3 },
    { exec: "admin",   weight: 1 }
  ],
  thresholds: [{ metric: "http_req_duration", stat: "p95", op: "<", valueMs: 800 }]
};

export async function shopper(ctx) { /* … */ }
export async function admin(ctx)   { /* … */ }
```

`userMix` weights are exact, not statistical: a 3:1 mix over 8 virtual users
really assigns 6 and 2. With no `userMix`, Swarmo calls the default export.

To make gRPC calls from a script, add an `options.grpc` block naming where the
schema comes from — it is compiled once at planning time and shared by every
virtual user:

```js
grpc: { protoDir: "protos" }                                    // easiest
grpc: { protoFiles: ["protos/orders.proto"], includePaths: ["protos"] }
grpc: { reflection: true, address: "{{grpcHost}}" }
```

`protoDir` also accepts `protoFiles` as entry points within that folder, and
`maxResponseBytes` raises the response size cap for scripted calls.

Scripted runs are capped at 500 virtual users because each one gets its own OS
thread and JavaScript context. For higher throughput, use a declarative
scenario, which has no JavaScript in the hot path.

## `.swarmo/runs/<runId>/`

`run.json` is the final `RunSummary`: totals, per-tag percentiles, checks,
threshold results and status-code counts. `timeline.json` is the array of
one-second snapshots, which is what the Run screen replays when you open a past
run.

Raw per-request samples are never stored. Latencies are folded into HDR
histograms as they arrive, so memory is bounded by the number of distinct tags
rather than by the number of requests — a ten-minute run at 10k requests/sec
costs the same as a ten-second one.
