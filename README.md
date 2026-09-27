# Swarmo

An API workbench where debugging a request and load-testing it are the same
artifact. Compose and send HTTP requests, script assertions against the
responses, then promote any of those requests into a load scenario — ramping
virtual users or a fixed arrival rate — without leaving the app or rewriting
anything.

Local-first: a workspace is a plain folder of JSON files you can commit to git.
No account, no sync, no telemetry. The only network requests Swarmo makes are
the ones you ask it to send.

## What's in it

**API client.** Collections and folders, environments with variables
(`{{baseUrl}}`), all the usual body types (JSON, form, multipart, GraphQL,
binary), inherited auth and headers, pre-request and post-response JavaScript,
response timings, a searchable history of every send, and import from Postman
collections and environments or from a pasted cURL command.

**gRPC.** A first-class citizen, sitting in the same collections as HTTP
requests. Point at `.proto` files or use server reflection; Swarmo compiles
schemas itself, so there is no `protoc` to install and no generated code.
Unary, server-streaming, client-streaming and bidirectional methods all work: a
client stream takes its messages as a JSON array, and the response pane shows
every message received and the time to the first one. The same `sw` / `pm`
scripts run, with `sw.response.status` carrying the gRPC status code.

**WebSocket.** A WebSocket request is a short scripted session: connect, send
an ordered list of text or binary messages, wait after each one for a reply, a
number of frames or a time window, then close. The client shows the full
transcript and a per-message latency table.

**Load engine.** Two engines behind one scheduler:

- *Native* runs declarative `*.load.json` scenarios entirely in Rust with no
  JavaScript in the hot path. This is the high-throughput path.
- *Scripted* runs `*.user.js` files where each virtual user is a JavaScript
  program with per-user state that persists across iterations — the
  Locust-style model, with a weighted mix of user types.

Both support a **closed model** (stages ramp concurrent virtual users) and an
**open model** (stages ramp arrivals per second). In the open model, latency is
measured from the time an iteration was *scheduled*, not when a worker picked it
up, so queueing delay shows up in the percentiles instead of disappearing —
the coordinated-omission problem that makes naive load tests look faster than
the system really is.

Both engines drive HTTP and gRPC, and declarative scenarios can also include
WebSocket steps. A scenario can mix protocols step by step, captures work the
same way (a gRPC response is decoded to JSON first, so `$.token` needs no special
case), and scripted users get `ctx.grpc` alongside `ctx.http`. A gRPC stream is
recorded as one sample covering its lifetime; a WebSocket step as a connect
sample plus one sample per message that waited for an answer.

Runs keep their full timeline. You can compare two runs side by side, apply
pass/fail thresholds, and export a self-contained HTML report.

## Repository layout

```
crates/
  swarmo-core/       data model, workspace file store, interpolation, Postman import
  swarmo-http/       single-shot HTTP execution for the client UI
  swarmo-grpc/       dynamic protobuf, reflection, unary and streaming gRPC calls
  swarmo-ws/         WebSocket sessions, for the client and the load engine
  swarmo-script/     the sandboxed QuickJS runtime (sw / pm / ctx APIs)
  swarmo-load/       schedulers, virtual users, metrics pipeline
  swarmo-app/        the Tauri desktop app (all #[tauri::command] functions)
  swarmo-cli/        `swarmo`, the headless runner for CI
  echo-server/       a local HTTP and WebSocket test server, used by tests and demos
  grpc-test-server/  a local gRPC test server, likewise
ui/                  React + TypeScript frontend
tools/               CI script, icon and demo-workspace generators, helper scripts
```

The library crates build and test without Tauri installed. `swarmo-core` also
builds without any gRPC dependency, so the data model and file store stay
protocol-agnostic.

## Building

Prerequisites: a recent stable Rust toolchain, Node LTS, and the platform
webview (WebView2 on Windows, WebKitGTK on Linux, built in on macOS).

```bash
npm run setup
```

Run the desktop app in development:

```bash
npm run dev
```

Build installers (NSIS on Windows, dmg on macOS, AppImage/deb on Linux):

```bash
npm run build
```

Run everything the CI script runs — rustfmt, clippy with warnings denied, the
Rust test suite, the TypeScript typecheck, the UI tests and the UI build:

```bash
npm run ci
```

The Windows installer comes out around 4.6 MB and the installed binary around
17 MB, because the UI runs in the system webview rather than a bundled browser.

## Trying it without clicking anything

Generate a demo workspace, start the echo server, and run a load test headlessly:

```bash
node tools/make-demo-workspace.mjs
```

```bash
cargo run -p echo-server -- 8787
```

```bash
cargo run -p grpc-test-server -- 50051
```

```bash
cargo run -p swarmo-cli -- run demo-workspace loadtests/shopper.user.js --yes
```

The demo workspace also has `grpc-smoke.load.json` (log in, capture the token,
call with it as metadata), `mixed-protocols.load.json` (HTTP and gRPC in one
run) and `grpc-shopper.user.js` (scripted `ctx.grpc` users).

`swarmo run` exits 0 when every threshold passed and 1 when one failed, so it
drops straight into CI. `--json` prints the full summary instead of a table.

For reference, on a mid-range desktop the native engine holds a requested 8,000
requests/sec against a local server at a p99 of about 5 ms with no dropped
metric samples — the generator is not the limiting factor at that rate.

## A note on safety

Swarmo is a traffic generator. Before any run it asks you to confirm the hosts
it is about to hit, and it remembers your answer per workspace. The CLI refuses
to start without `--yes` when it is not attached to a terminal. Only test
systems you own or have permission to test.

Workspaces are meant to be shared, and a workspace can contain scripts and
commands. Read [`SECURITY.md`](SECURITY.md) before opening one you did not
write.

## Documentation

- [`docs/formats.md`](docs/formats.md) — every on-disk file format
- [`docs/generators.md`](docs/generators.md) — randomising values in requests and load tests
- [`docs/scripting.md`](docs/scripting.md) — the `sw`, `pm` and `ctx` APIs
- [`docs/qa-walkthrough.md`](docs/qa-walkthrough.md) — the manual test pass
- [`SECURITY.md`](SECURITY.md) — what a workspace can do, and how to report a vulnerability

## License

[MIT](LICENSE)
