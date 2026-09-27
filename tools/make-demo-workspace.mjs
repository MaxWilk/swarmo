// Creates a demo Swarmo workspace pointed at the local echo and gRPC servers.
// Run: node tools/make-demo-workspace.mjs [outDir] [httpPort] [grpcPort]
import { copyFileSync, mkdirSync, writeFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { randomUUID } from "node:crypto";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const out = process.argv[2] ?? join(root, "demo-workspace");
const port = process.argv[3] ?? "8787";
const grpcPort = process.argv[4] ?? "50051";

const w = (p, obj) => {
  mkdirSync(dirname(p), { recursive: true });
  writeFileSync(p, typeof obj === "string" ? obj : JSON.stringify(obj, null, 2));
};

const req = (name, over = {}) => ({
  version: 1,
  id: randomUUID(),
  name,
  method: "GET",
  url: "",
  params: [],
  headers: [],
  auth: { type: "inherit" },
  body: { type: "none" },
  scripts: { preRequest: "", postResponse: "" },
  settings: { followRedirects: true, timeoutMs: 30000, verifyTls: true },
  ...over,
});

w(join(out, "swarmo.json"), {
  version: 1,
  name: "Demo workspace",
  activeEnvironment: "local",
});
w(join(out, ".gitignore"), ".swarmo/\n");

w(join(out, "environments", "local.env.json"), {
  version: 1,
  name: "local",
  variables: [
    { key: "baseUrl", value: `http://127.0.0.1:${port}`, secret: false, enabled: true },
    { key: "grpcHost", value: `http://127.0.0.1:${grpcPort}`, secret: false, enabled: true },
    { key: "apiKey", value: "", secret: true, enabled: true },
  ],
});

// The gRPC schema, copied in so proto paths are workspace-relative.
mkdirSync(join(out, "protos"), { recursive: true });
copyFileSync(
  join(root, "crates", "grpc-test-server", "protos", "testing.proto"),
  join(out, "protos", "testing.proto"),
);

const coll = join(out, "collections", "Demo API");
w(join(coll, "collection.json"), {
  version: 1,
  id: randomUUID(),
  name: "Demo API",
  headers: [{ key: "Accept", value: "application/json", enabled: true }],
  auth: { type: "none" },
  scripts: { preRequest: "", postResponse: "" },
});

w(
  join(coll, "Get JSON.req.json"),
  req("Get JSON", {
    url: "{{baseUrl}}/json",
    scripts: {
      preRequest: "",
      postResponse: [
        "sw.test('status is 200', () => sw.expect(sw.response.status).toBe(200));",
        "sw.test('has three items', () => sw.expect(sw.response.json().items.length).toBe(3));",
      ].join("\n"),
    },
  }),
);

w(
  join(coll, "Login.req.json"),
  req("Login", {
    method: "POST",
    url: "{{baseUrl}}/token",
    body: { type: "json", text: '{\n  "user": "demo"\n}' },
    scripts: {
      preRequest: "",
      postResponse: [
        "// Stash the token so later requests can use {{token}}.",
        "sw.env.set('token', sw.response.json().token);",
        "sw.test('got a token', () => sw.expect(sw.env.get('token')).toBeTruthy());",
      ].join("\n"),
    },
  }),
);

w(
  join(coll, "Echo With Token.req.json"),
  req("Echo With Token", {
    method: "POST",
    url: "{{baseUrl}}/echo",
    headers: [{ key: "Authorization", value: "Bearer {{token}}", enabled: true }],
    body: { type: "json", text: '{\n  "sku": "A-1",\n  "qty": 2\n}' },
  }),
);

w(
  join(coll, "Slow.req.json"),
  req("Slow", { url: "{{baseUrl}}/delay/120" }),
);

// ---------------------------------------------------------------------------
// gRPC
// ---------------------------------------------------------------------------

const grpcReq = (name, over = {}) => ({
  version: 1,
  id: randomUUID(),
  name,
  address: "{{grpcHost}}",
  protoSource: {
    kind: "files",
    files: ["protos/testing.proto"],
    includePaths: ["protos"],
  },
  service: "swarmo.testing.TestService",
  method: "Echo",
  metadata: [],
  message: "{}",
  scripts: { preRequest: "", postResponse: "" },
  settings: { timeoutMs: 30000, verifyTls: true },
  ...over,
});

const gcoll = join(out, "collections", "Grpc Demo");
w(join(gcoll, "collection.json"), {
  version: 1,
  id: randomUUID(),
  name: "Grpc Demo",
  headers: [],
  auth: { type: "none" },
  scripts: { preRequest: "", postResponse: "" },
});

w(
  join(gcoll, "Echo.grpc.json"),
  grpcReq("Echo", {
    message: JSON.stringify(
      {
        message: "hello from Swarmo",
        number: 42,
        flag: true,
        nested: { label: "inner", values: [1, 2, 3] },
        at: "2024-03-01T12:30:00Z",
        tags: { env: "demo" },
      },
      null,
      2,
    ),
    scripts: {
      preRequest: "",
      postResponse: [
        "// 0 is the gRPC status code for OK.",
        "sw.test('status is OK', () => sw.expect(sw.response.status).toBe(0));",
        "sw.test('echoed the message', () =>",
        "  sw.expect(sw.response.json().message).toBe('hello from Swarmo'));",
      ].join("\n"),
    },
  }),
);

w(
  join(gcoll, "Login.grpc.json"),
  grpcReq("Login", {
    method: "Login",
    message: '{\n  "user": "demo"\n}',
    scripts: {
      preRequest: "",
      postResponse: [
        "// Stash the token so later requests can use {{grpcToken}}.",
        "sw.env.set('grpcToken', sw.response.json().token);",
        "sw.test('got a token', () => sw.expect(sw.env.get('grpcToken')).toBeTruthy());",
      ].join("\n"),
    },
  }),
);

w(
  join(gcoll, "Authed Echo.grpc.json"),
  grpcReq("Authed Echo", {
    metadata: [
      { key: "authorization", value: "Bearer {{grpcToken}}", enabled: true },
    ],
    message: '{\n  "message": "authenticated"\n}',
  }),
);

w(
  join(gcoll, "Fail.grpc.json"),
  grpcReq("Fail", {
    method: "Fail",
    message: '{\n  "code": 5,\n  "message": "no such order"\n}',
    scripts: {
      preRequest: "",
      postResponse:
        "sw.test('is NOT_FOUND', () => sw.expect(sw.response.status).toBe(5));",
    },
  }),
);

w(
  join(gcoll, "Echo By Reflection.grpc.json"),
  grpcReq("Echo By Reflection", {
    protoSource: { kind: "reflection" },
    message: '{\n  "message": "found via reflection"\n}',
  }),
);

// Every field randomized, including a base64 string tensor. Note that numeric
// and boolean tokens are unquoted so the JSON stays the right shape.
w(
  join(gcoll, "Randomized Echo.grpc.json"),
  grpcReq("Randomized Echo", {
    message: [
      "{",
      '  "message": "{{$pick(vivo,samsung,xiaomi,apple)}}",',
      '  "number": {{$int(1,100)}},',
      '  "flag": {{$bool}},',
      '  "nested": { "label": "{{$string(6)}}", "values": [] },',
      '  "at": "{{$now}}",',
      '  "tags": { "trace": "{{$uuid}}", "tier": "{{$pick(free,pro)}}" }',
      "}",
    ].join("\n"),
  }),
);

// The simplest possible way to ask for a fixed rate: no stages at all.
w(join(out, "loadtests", "constant-rate.load.json"), {
  version: 1,
  name: "constant-rate",
  mode: "open",
  arrivalRatePerSec: 150,
  durationSec: 20,
  maxVus: 100,
  environment: "local",
  steps: [
    {
      requestRef: "collections/Grpc Demo/Randomized Echo.grpc.json",
      capture: [],
      tag: "echo",
    },
  ],
  thresholds: [
    { metric: "http_req_failed", stat: "rate", op: "<", value: 0.01, abortOnFail: false },
  ],
  runScripts: false,
  newConnectionPerIteration: false,
  verifyTls: true,
  timeoutMs: 30000,
});

// A gRPC scenario: log in, capture the token, then call with it as metadata.
w(join(out, "loadtests", "grpc-smoke.load.json"), {
  version: 1,
  name: "grpc-smoke",
  mode: "closed",
  stages: [
    { durationSec: 3, target: 20 },
    { durationSec: 5, target: 20 },
    { durationSec: 2, target: 0 },
  ],
  maxVus: 50,
  environment: "local",
  steps: [
    {
      requestRef: "collections/Grpc Demo/Login.grpc.json",
      thinkTimeMs: [50, 150],
      capture: [{ from: "body", jsonPath: "$.token", as: "grpcToken" }],
      tag: "grpc login",
    },
    {
      requestRef: "collections/Grpc Demo/Authed Echo.grpc.json",
      thinkTimeMs: [100, 300],
      capture: [],
      tag: "grpc echo",
    },
  ],
  thresholds: [
    { metric: "http_req_duration", stat: "p95", op: "<", valueMs: 500, abortOnFail: false },
    { metric: "http_req_failed", stat: "rate", op: "<", value: 0.01, abortOnFail: false },
  ],
  runScripts: false,
  newConnectionPerIteration: false,
  verifyTls: true,
  timeoutMs: 30000,
});

// One scenario driving both protocols, to show they mix freely.
w(join(out, "loadtests", "mixed-protocols.load.json"), {
  version: 1,
  name: "mixed-protocols",
  mode: "closed",
  stages: [
    { durationSec: 2, target: 10 },
    { durationSec: 5, target: 10 },
  ],
  maxVus: 20,
  environment: "local",
  steps: [
    {
      requestRef: "collections/Demo API/Get JSON.req.json",
      thinkTimeMs: [50, 150],
      capture: [],
      tag: "http json",
    },
    {
      requestRef: "collections/Grpc Demo/Echo.grpc.json",
      thinkTimeMs: [50, 150],
      capture: [],
      tag: "grpc echo",
    },
  ],
  thresholds: [
    { metric: "http_req_failed", stat: "rate", op: "<", value: 0.01, abortOnFail: false },
  ],
  runScripts: false,
  newConnectionPerIteration: false,
  verifyTls: true,
  timeoutMs: 30000,
});

w(
  join(out, "loadtests", "grpc-shopper.user.js"),
  `// Scripted virtual users calling gRPC. ctx.grpc sits alongside ctx.http, so a
// single script can drive both protocols.
export const options = {
  environment: "local",
  mode: "closed",
  stages: [
    { durationSec: 2, target: 12 },
    { durationSec: 6, target: 12 },
    { durationSec: 2, target: 0 }
  ],
  maxVus: 20,
  userMix: [
    { exec: "caller", weight: 3 },
    { exec: "browser", weight: 1 }
  ],
  // Where ctx.grpc gets its schema. Swap for { reflection: true, address: "{{grpcHost}}" }
  // to ask the server instead of compiling .proto files.
  grpc: {
    protoFiles: ["protos/testing.proto"],
    includePaths: ["protos"]
  },
  thresholds: [
    { metric: "http_req_duration", stat: "p95", op: "<", valueMs: 800 },
    { metric: "http_req_failed", stat: "rate", op: "<", value: 0.01 }
  ]
};

export async function caller(ctx) {
  // ctx.vars persists across iterations, so each user logs in only once.
  if (!ctx.vars.token) {
    const login = await ctx.grpc.call("{{grpcHost}}", "swarmo.testing.TestService/Login", {
      message: { user: "demo" },
      tag: "grpc login"
    });
    ctx.check(login, { "login ok": r => r.code === 0 });
    ctx.vars.token = login.json().token;
  }

  const res = await ctx.grpc.call("{{grpcHost}}", "swarmo.testing.TestService/Echo", {
    message: { message: "order placed", number: ctx.vu.iteration },
    metadata: { authorization: \`Bearer \${ctx.vars.token}\` },
    tag: "grpc echo"
  });
  ctx.check(res, {
    "echo ok": r => r.code === 0,
    "token was sent": r => r.json().metadata.authorization.startsWith("Bearer ")
  });
  await ctx.sleep(0.3, 0.9);
}

export async function browser(ctx) {
  const res = await ctx.http.get("{{baseUrl}}/json", { tag: "http json" });
  ctx.check(res, { "status is 200": r => r.status === 200 });
  await ctx.sleep(0.2, 0.6);
}
`,
);

// A declarative scenario: log in, capture the token, then use it.
w(join(out, "loadtests", "checkout-smoke.load.json"), {
  version: 1,
  name: "checkout-smoke",
  mode: "closed",
  stages: [
    { durationSec: 3, target: 20 },
    { durationSec: 5, target: 20 },
    { durationSec: 2, target: 0 },
  ],
  maxVus: 50,
  environment: "local",
  steps: [
    {
      requestRef: "collections/Demo API/Login.req.json",
      thinkTimeMs: [50, 150],
      capture: [{ from: "body", jsonPath: "$.token", as: "token" }],
      tag: "login",
    },
    {
      requestRef: "collections/Demo API/Echo With Token.req.json",
      thinkTimeMs: [100, 300],
      capture: [],
      tag: "create order",
    },
  ],
  thresholds: [
    { metric: "http_req_duration", stat: "p95", op: "<", valueMs: 500, abortOnFail: false },
    { metric: "http_req_failed", stat: "rate", op: "<", value: 0.01, abortOnFail: false },
  ],
  runScripts: false,
  newConnectionPerIteration: false,
  verifyTls: true,
  timeoutMs: 30000,
});

// An open-model scenario, to exercise arrival-rate scheduling.
w(join(out, "loadtests", "steady-rate.load.json"), {
  version: 1,
  name: "steady-rate",
  mode: "open",
  stages: [
    { durationSec: 2, target: 200 },
    { durationSec: 5, target: 200 },
  ],
  maxVus: 100,
  environment: "local",
  steps: [
    {
      requestRef: "collections/Demo API/Get JSON.req.json",
      thinkTimeMs: null,
      capture: [],
      tag: "json",
    },
  ],
  thresholds: [
    { metric: "http_req_duration", stat: "p99", op: "<", valueMs: 250, abortOnFail: false },
  ],
  runScripts: false,
  newConnectionPerIteration: false,
  verifyTls: true,
  timeoutMs: 30000,
});

w(
  join(out, "loadtests", "shopper.user.js"),
  `// A scripted virtual-user mix: three browsers for every one buyer.
export const options = {
  environment: "local",
  mode: "closed",
  stages: [
    { durationSec: 2, target: 12 },
    { durationSec: 6, target: 12 },
    { durationSec: 2, target: 0 }
  ],
  maxVus: 20,
  userMix: [
    { exec: "browse", weight: 3 },
    { exec: "buy", weight: 1 }
  ],
  thresholds: [
    { metric: "http_req_duration", stat: "p95", op: "<", valueMs: 800 },
    { metric: "http_req_failed", stat: "rate", op: "<", value: 0.01 }
  ]
};

export async function browse(ctx) {
  const res = await ctx.http.get("{{baseUrl}}/json", { tag: "list items" });
  ctx.check(res, {
    "status is 200": r => r.status === 200,
    "has items": r => Array.isArray(r.json().items)
  });
  await ctx.sleep(0.2, 0.6);
}

export async function buy(ctx) {
  // ctx.vars persists across iterations, so each user logs in only once.
  if (!ctx.vars.token) {
    const login = await ctx.http.post("{{baseUrl}}/token", { tag: "login" });
    ctx.vars.token = login.json().token;
  }

  const res = await ctx.http.post("{{baseUrl}}/echo", {
    headers: { Authorization: \`Bearer \${ctx.vars.token}\` },
    json: { sku: "A-1", qty: 1 },
    tag: "create order"
  });
  ctx.check(res, {
    "order accepted": r => r.status === 200,
    "token was sent": r => r.json().headers.authorization.startsWith("Bearer ")
  });
  await ctx.sleep(0.3, 0.9);
}
`,
);

console.log("demo workspace written to", out);
