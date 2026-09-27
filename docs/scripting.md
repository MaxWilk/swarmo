# Scripting in Swarmo

Swarmo embeds QuickJS. There are two script contexts:

- **Request scripts** — pre-request and post-response hooks on a request,
  folder or collection. They use `sw` (and `pm`, for Postman compatibility).
- **Virtual-user scripts** — `*.user.js` files driving a load test. They use
  `ctx`.

## The sandbox

Scripts get no filesystem, no process, no environment variables, and no module
loader. `require`, `process`, `std` and `os` are all `undefined`. The only way
out is the APIs below.

Request scripts are capped at 64 MB of memory and 5 seconds of wall clock; an
infinite loop is interrupted and reported as a timeout rather than hanging the
app. Virtual-user scripts have the same memory cap but no time limit, because
they are stopped by the run ending or being cancelled.

`await` works at the top level of a request script. Host calls resolve
immediately rather than returning real promises, so `await` is optional — but
harmless, which keeps copied-in Postman and k6 snippets working.

## Request scripts: `sw`

### Variables

```js
sw.env.get("baseUrl");          // read (runtime overrides, then environment)
sw.env.set("token", "abc");     // set for the rest of the session
sw.env.has("apiKey");
sw.env.toObject();              // everything visible right now
```

Variables set here are **not** written to disk. They live for the session and
are visible to later requests, which is what makes a "log in once, then use the
token" flow work. Settings → Clear script-set variables resets them.

`sw.vars` is an alias of `sw.env`.

### The request (pre-request only, mutable)

```js
sw.request.method = "POST";
sw.request.url += "/retry";
sw.request.headers.push(["X-Trace", crypto.randomUUID()]);
sw.request.body = JSON.stringify({ retried: true });
```

Pre-request scripts run **before** `{{variable}}` interpolation, so a script can
set a variable that the URL then uses. The request object here still contains
the raw templates.

### The response (post-response only, frozen)

```js
sw.response.status;        // 201
sw.response.statusText;    // "Created"
sw.response.headers;       // lowercased keys
sw.response.body;          // the raw text
sw.response.json();        // throws a clear error if the body is not JSON
sw.response.durationMs;
sw.response.ok;            // 2xx
```

### Assertions

```js
sw.test("status is 201", () => {
  sw.expect(sw.response.status).toBe(201);
});
```

`sw.test` never throws; a failing assertion is recorded and shown in the
response pane's Tests tab. The callback may be `async`: if its promise
rejects, the test fails. Matchers: `toBe` (strict equality), `toEqual` (deep),
`toContain`, `toBeLessThan`, `toBeGreaterThan`, `toMatch`, `toBeTruthy`,
`toBeFalsy`, `toBeDefined`.

### Ad-hoc requests

```js
const res = await sw.sendRequest({
  method: "POST",
  url: "https://auth.example.com/token",
  json: { client_id: "x" }
});
sw.env.set("token", res.json().access_token);
```

These go through the same client as the UI, so cookies and the proxy setting
apply. Useful for token refresh in a collection-level pre-request script.

### Other globals

`console.log/info/warn/error` (captured into the Scripts tab's console panel),
`JSON`, `Math`, `Date`, `crypto.randomUUID()`, `btoa`, `atob`, and
`sw.sleep(ms)`. `btoa` and `atob` behave as in a browser: they work on
one-character-per-byte strings, `atob` accepts unpadded input, and both throw
on invalid input rather than returning an empty string.

## Postman compatibility: `pm`

Imported Postman scripts mostly run unchanged. `pm.environment`, `pm.globals`,
`pm.variables` and `pm.collectionVariables` all map onto the same variable
layer. `pm.test`, `pm.expect`,
`pm.response.code/status/json()/text()/responseTime/headers.get()`,
`pm.sendRequest(opts, callback)` and the legacy
`postman.setEnvironmentVariable` are all implemented.

`pm.expect` follows chai. Chain words (`to`, `be`, `have`, `that`, `and` …) read
naturally, `not` negates whatever follows, and the property forms assert when
they are read, with no call: `.ok`, `.true`, `.false`, `.null`, `.undefined`,
`.NaN`, `.exist`, `.empty`. The methods are `equal`, `eql` (or `deep.equal`),
`a`/`an`, `include`/`contain`, `match`, `string`, `property` (without a value
it moves on to the property, so `.to.have.property('id').that.is.a('number')`
works), `lengthOf`, `members` and `include.members`, `keys`, `above`, `below`,
`at.least`, `at.most`, `within`, `closeTo`, `oneOf`, `instanceOf`, `satisfy` and
`throw`.

On `pm.response` (or a response passed to `pm.expect`) Postman's assertions
work too: `.to.have.status(201)` or `.to.have.status('Created')`,
`.to.have.header(name[, value])`, `.to.have.body(text)`,
`.to.have.jsonBody(path[, value])`, `.to.be.json`, and the status classes
`.to.be.ok` (exactly 200, as in Postman), `.success` (2xx), `.info`,
`.redirection`, `.clientError`, `.serverError`, `.error`, `.accepted`,
`.badRequest`, `.unauthorized`, `.forbidden`, `.notFound` and `.rateLimited`.

`pm.sendRequest` takes Postman's request shape — `header` (an object or a
`[{key, value}]` list), `url` as a string or `{raw}`, and `body` with `mode`
`raw`, `urlencoded` or `graphql` — as well as `sw.sendRequest`'s. The response
handed to the callback is `sw.sendRequest`'s, with Postman's `code`,
`responseTime` and `headers.get()` added; note that its `status` is the
number, not the status text.

What is **not** implemented — `pm.cookies`, `pm.iterationData`, `pm.execution`,
`pm.visualizer`, `pm.vault`, `postman.setNextRequest` — throws
`Swarmo: pm.<x> is not supported` rather than silently doing nothing. The
importer detects these while importing, prefixes the script with a
`// SWARMO-IMPORT-WARNING:` comment, and lists them in the import report, so you
find out at import time instead of in the middle of a test run.

## gRPC requests use the same `sw` and `pm` APIs

A gRPC request runs the same scripts an HTTP request does — there is no second
scripting API to learn — with a few fields carrying gRPC meanings:

| | |
|---|---|
| `sw.response.status` | The **gRPC status code**, 0–16. `0` is OK. |
| `sw.response.statusText` | The code's name, e.g. `"NOT_FOUND"`. |
| `sw.response.json()` | The decoded response message. |
| `sw.response.headers` | Initial metadata, plus trailers prefixed `trailer-`. |
| `sw.request.method` | Always `"GRPC"`. |
| `sw.request.url` | `address/package.Service/Method`. |
| `sw.request.headers` | The request metadata. |
| `sw.request.body` | The request message as JSON text. |

So the success assertion is:

```js
sw.test("call succeeded", () => sw.expect(sw.response.status).toBe(0));
```

Everything else behaves as it does for HTTP. A pre-request script may rewrite
`sw.request.url` to retarget the call, but the result must still be
`address/package.Service/Method` — anything else is rejected with a clear error
rather than guessed at. `sw.sendRequest` remains HTTP-only.

## Randomising values without a script

Before reaching for a script, note that request templates support
`{{$int(1,50)}}`, `{{$pick(a,b,c)}}`, `{{$string(8)}}`, `{{$uuid}}` and friends
directly, re-evaluated every iteration. See
[`generators.md`](generators.md) — it covers most "vary the payload" cases with
no JavaScript at all.

## Virtual-user scripts: `ctx`

Each virtual user gets its own JavaScript context, so `ctx.vars` is genuinely
per-user state that survives across iterations.

```js
export const options = {
  environment: "local",
  mode: "closed",                       // or "open"
  stages: [{ durationSec: 60, target: 50 }],
  maxVus: 200,
  userMix: [{ exec: "shopper", weight: 3 }, { exec: "admin", weight: 1 }],
  thresholds: [{ metric: "http_req_duration", stat: "p95", op: "<", valueMs: 800 }]
};

export async function shopper(ctx) {
  // Runs once per iteration, over and over, for the length of the run.
  if (!ctx.vars.token) {
    const login = await ctx.http.post("{{baseUrl}}/login", {
      json: { user: "demo" },
      tag: "login"
    });
    ctx.vars.token = login.json().token;
  }

  const res = await ctx.http.get("{{baseUrl}}/products", {
    headers: { Authorization: `Bearer ${ctx.vars.token}` },
    tag: "list products"
  });

  ctx.check(res, {
    "status is 200": r => r.status === 200,
    "has products": r => r.json().items.length > 0
  });

  await ctx.sleep(1, 3);
}
```

### `ctx.http`

`get`, `post`, `put`, `patch`, `delete`, `head`, `options`, and
`request(method, url, opts)`. Options: `headers`, `params`, `json`, `body`,
`form`, and `tag`.

`{{variables}}` in the URL, header values and a string `body` are interpolated
automatically from the run's environment.

**Always pass a `tag`.** It names the metric series. Without one the tag falls
back to `METHOD url`, and a URL containing an id would create a new series per
id, which bloats the report and makes percentiles meaningless.

### `ctx.grpc`

`ctx.grpc` sits alongside `ctx.http`, so one script can drive both protocols.
It needs an `options.grpc` block saying where the schema comes from; without
one, calls fail with a message telling you to add it.

```js
export const options = {
  // …stages, userMix, thresholds as usual…
  grpc: { protoDir: "protos" }
  // or: grpc: { protoFiles: [...], includePaths: [...] }
  // or: grpc: { reflection: true, address: "{{grpcHost}}" }
};

export async function buyer(ctx) {
  if (!ctx.vars.token) {
    const login = await ctx.grpc.call("{{grpcHost}}", "orders.v1.OrderService/Login", {
      message: { user: "demo" },
      tag: "login"
    });
    ctx.vars.token = login.json().token;
  }

  const res = await ctx.grpc.call("{{grpcHost}}", "orders.v1.OrderService/CreateOrder", {
    message: { sku: "A-1", qty: 1 },
    metadata: { authorization: `Bearer ${ctx.vars.token}` },
    tag: "create order"
  });

  ctx.check(res, {
    "accepted": r => r.code === 0,
    "has an id": r => r.json().orderId.length > 0
  });
}
```

`ctx.grpc.call(address, "package.Service/Method", opts)` takes `message` (an
object, or a JSON string), `metadata`, and `tag`. There is also
`ctx.grpc.unary(address, method, message, opts?)` as sugar. `{{variables}}`
interpolate in the address, in metadata values, and in a string `message`.

The response has `code` (0–16), `codeName`, `ok`, `statusMessage`, `json()`,
`text()`, `headers`, `trailers` and `durationMs`. A non-OK status is data, not
an exception — the call returns normally so your checks can assert on it.
Calling `json()` on a failed call throws, because there is no response message.

Without a `tag`, the metric series is named `Service/Method`, which is already
stable — unlike a URL, a gRPC method name never embeds an id — so tagging is
less critical here than it is for `ctx.http`.

`protoDir` points at a folder and works the rest out: it scans recursively,
reads the `import` statements, and derives the import roots from them, so it
does not matter which level of the tree you point at. That is the option to
reach for first.

Other `options.grpc` fields: `verifyTls`, `timeoutMs`, `maxResponseBytes`
(default 16 MB — raise it if you see a "message too large" error), and
`metadata`, which is sent with the reflection request that fetches the schema.
That last one matters when the server requires auth even for reflection:

```js
grpc: {
  reflection: true,
  address: "{{grpcHost}}",
  metadata: { authorization: "Bearer {{token}}" }
}
```

### The rest of `ctx`

| | |
|---|---|
| `ctx.check(res, {name: fn})` | Records a named pass/fail counter; returns whether all passed. A check that throws counts as a failure. Works for HTTP and gRPC responses alike. |
| `ctx.sleep(sec)` / `ctx.sleep(min, max)` | Think time. One argument is fixed, two is uniform random. |
| `ctx.vars` | Per-virtual-user state, persisting across iterations. |
| `ctx.env(key)` | Read a variable from the run's environment. |
| `ctx.vu.id`, `ctx.vu.iteration` | Which user this is, and which pass. |
| `ctx.group(name, fn)` | Prefixes tags and checks inside with `name :: `. `fn` may be async: `await ctx.group(...)` and the prefix lasts until it settles. |
| `ctx.log(...)` | Console output. |
| `ctx.fail(msg)` | Abort this iteration. |

### Module syntax

`export const options` and `export async function` are supported, as is
`export default` for a single unnamed user type. `import` is **not**: a script
is always one self-contained file. An `import` line is commented out with a
note rather than being a hard error.

### An iteration that throws

The error is reported once per virtual user (to avoid flooding the log), any
checks recorded before the throw are still counted, and that user moves on to
its next iteration. A script that fails to compile at all fails the run
immediately, before any traffic is sent.
