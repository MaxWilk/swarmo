# Manual QA walkthrough

The automated suite covers the engines; this pass covers the things only a human
can see. It should complete with **zero errors in the webview console** and no
unexpected toasts.

Setup:

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
npm run dev
```

## 1. First run and workspace

1. The window opens on the welcome screen with no workspace loaded.
2. **Open workspace** → pick `demo-workspace`. The sidebar shows the
   *Demo API* collection with four requests.
3. Close and relaunch the app. The workspace is offered under **Recent**.
4. Point **Open workspace** at a folder that is not a workspace. The error names
   the missing `swarmo.json` instead of failing silently.

## 2. Sending requests

1. Open **Get JSON** and press <kbd>Ctrl</kbd>+<kbd>Enter</kbd>. A 200 comes
   back, the JSON body is pretty-printed and foldable, and the Tests tab shows
   two passing tests.
2. Switch the body view to **Raw**. It is the unformatted original.
3. The Timing tab shows TTFB, download and total, and the bars are proportional.
4. Open **Login** and send it. The Tests tab passes, and the console panel in the
   Scripts tab is empty (nothing logged).
5. Open **Echo With Token** and send it. The Request tab shows
   `Authorization: Bearer tok_abc123` — the token the previous script captured,
   already interpolated.
6. Edit the URL to `{{missing}}/x` and send. A warning names the unresolved
   variable, and the request still goes out with the literal text rather than an
   empty string.
7. Open **Slow**, set the timeout to 50 ms in Settings, and send. The error says
   the request timed out. Restore the timeout.
8. Start a send against **Slow** and press **Cancel** while it is in flight. The
   button returns to Send and no result is recorded.

## 3. Editing and the tree

1. Add a header, switch tabs away and back — the edit survives and the tab shows
   a dirty dot.
2. <kbd>Ctrl</kbd>+<kbd>S</kbd> clears the dirty dot. Check the `.req.json` file
   on disk has changed.
3. <kbd>Ctrl</kbd>+<kbd>P</kbd>, type a few non-adjacent letters of a request
   name, press <kbd>Enter</kbd>. It opens.
4. Right-click a request → **Rename**. The tab title and the filename both
   change, and the tab stays open and functional.
5. Right-click → **Duplicate**, then **Delete** the copy.
6. Drag a request onto a different collection. It moves on disk; the open tab
   follows it.
7. Create a folder, then a request inside it. Set the folder's auth and confirm
   the child request inherits it (visible in the response's Request tab).
8. Name a request `CON` and one `a/b:c`. Both save; check the filenames were
   made safe while the displayed names were not.

## 4. Environments

1. Open the environment editor. Set `apiKey` to any value and save.
2. Confirm `environments/local.env.json` still has `"value": ""` for `apiKey`,
   and `.swarmo/secrets.env.json` holds the real value.
3. Create a second environment, switch to it with the dropdown, and confirm
   `{{baseUrl}}` now resolves differently in the URL bar.

## 5. Postman import

1. Import `crates/swarmo-core/tests/fixtures/basic.postman_collection.json`.
   The report shows 3 requests, 1 folder and a new environment.
2. Import `edge.postman_collection.json`. The report lists warnings for the
   OAuth2 auth, the `pm.cookies` script and the query-string API key — and every
   request is still present.
3. Open the request whose script used `pm.cookies` and send it. It fails loudly
   with `pm.cookies is not supported` rather than passing quietly.
4. Import `prod.postman_environment.json` and confirm the secret landed in
   `.swarmo/`, not in the committed file.

## 6. Load tests

1. Go to **Load**. Both scenarios and the user script are listed.
2. Open `checkout-smoke`. The stages table shows the ramp and the preview
   sparkline matches it. The two steps resolve to real request names.
3. Switch to the JSON tab, break the JSON, and confirm a parse error appears and
   nothing is saved. Fix it.
4. Press **Run**. The confirmation dialog names `127.0.0.1` and warns about
   permission. Cancel it — nothing runs.
5. Press **Run** again and confirm. The app switches to the Run screen.
6. During the run: the charts update about once a second, the VU count climbs to
   20 and drains to 0, and the window stays responsive (scroll the per-tag
   table while it runs).
7. At the end the run is marked **passed**, both thresholds show green, and
   `login` and `create order` have roughly equal counts — proof the capture fed
   the second step.
8. Run `steady-rate`. Requests per second holds near 200 and active VUs stays
   low, because the open model only uses the workers it needs.
9. Run `shopper.user.js`. The checks table lists all four checks with zero
   failures, and `login` runs only a handful of times — once per buying user, not
   once per iteration.
10. Start a run and press **Stop**. It stops within about two seconds and is
    marked **stopped**, with the partial results kept.

## 7. gRPC

1. Open **Grpc Demo → Echo**. The service and method dropdowns are already
   populated, which means the `.proto` compiled.
2. Press <kbd>Ctrl</kbd>+<kbd>Enter</kbd>. The status pill shows a green
   `0 OK`, the response message is pretty-printed JSON, and the Tests tab shows
   two passes.
3. In the Metadata tab add `x-trace: abc` and call again. The response echoes it
   back under `metadata`, confirming it went over the wire.
4. Open the method dropdown. `StreamNumbers` is present but disabled, with a
   tooltip explaining streaming is not supported.
5. Open **Fail** and call it. The pill is a red `5 NOT_FOUND`, the status
   message reads "no such order", the Response tab explains a failed call has
   no message, and the test asserting code 5 passes — a failed call is data, not
   an error.
6. Break the message: replace it with `{"nope": 1}` and call. The error names
   the message type and the offending field. Undo.
7. Open **Login**, call it, then open **Authed Echo** and call it. The Request
   tab shows `Authorization: Bearer grpc_tok_123` — the token the previous
   script captured.
8. Open **Echo By Reflection** (its Proto tab is set to Server reflection) and
   press **Fetch schema**. It reports the services found; calling then works
   with no `.proto` file at all.
9. In the Proto tab of **Echo**, point a file path at something that does not
   exist and press Refresh schema. The error names the missing path. Undo.
10. Stop the gRPC server and call **Echo**. The pill shows `14 UNAVAILABLE`
    rather than hanging. Restart the server.

## 8. gRPC load tests

1. Run `grpc-smoke`. The confirmation dialog lists `127.0.0.1`. During the run
   the charts update as usual; at the end `grpc login` and `grpc echo` have
   roughly equal counts, proving the token capture fed the second step.
2. Run `mixed-protocols`. One run reports both an `http json` tag and a
   `grpc echo` tag, and the status-code table contains both `200` and `0`.
3. Run `grpc-shopper.user.js`. Every check passes, `grpc login` runs only a
   handful of times (once per calling user), and both `ctx.grpc` and `ctx.http`
   tags appear.
4. Edit `grpc-smoke` to reference `StreamNumbers` and press Run. It refuses at
   planning time, before any traffic, saying streaming is not supported.

## 9. Rates and random values

1. Open `constant-rate`. The Load profile shows **Hold a constant rate** with
   150 requests per second and no stages table in play.
2. Run it. The live chart sits flat at 150 from the first second — no ramp —
   and the summary reports 150 req/s over the full duration.
3. Switch the mode to Closed. The constant-rate option becomes unavailable and
   the rate is cleared, because a closed-mode target is virtual users, not a
   rate. Switch back.
4. Switch to **Ramp through stages** and add one stage of 60s targeting 100.
   Read the hint: it ramps 0 → 100 and averages 50, which is why the flat option
   exists.
5. Open **Grpc Demo → Randomized Echo**. Every field uses a generator. Call it
   twice; the response differs each time.
6. Expand **Random values** under the Message tab. The token table is there, and
   the same panel appears under an HTTP request's Body tab.
7. Break a token — `{{$nope(1)}}` — and call. It is reported as unresolved and
   left in the message rather than silently becoming empty.
8. Run `constant-rate` and confirm the responses vary: the run should succeed
   with a mix of generated values rather than 3000 identical requests.

## 10. Runs history

1. Go to **Runs**. Every run above is listed, newest first.
2. Open an old run. The charts and tables re-render from disk identically.
3. Delete a run. It disappears from the list and from `.swarmo/runs/`.

## 11. Appearance and errors

1. Settings → Theme → Dark, then Light, then Match the system. Every screen
   (including the editors and charts) recolours with no unreadable text.
2. Resize the window down to about 900px wide. Nothing overflows horizontally;
   wide tables scroll inside their own container.
3. Stop the echo server and send a request. The error explains the connection
   failed; it does not hang.
4. Corrupt a `.req.json` file by hand. The tree still loads, marking that one
   entry unreadable instead of showing an empty sidebar.
5. Settings → Clear script-set variables, then send **Echo With Token**. The
   Authorization header now shows the literal `{{token}}` and is flagged
   unresolved.
