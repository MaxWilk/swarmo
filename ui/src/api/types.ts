/**
 * Types mirroring the Rust structs in swarmo-core / swarmo-app.
 * Everything crossing the Tauri boundary is camelCase.
 */

// -- workspace ---------------------------------------------------------------

export interface WorkspaceInfo {
  path: string;
  name: string;
  activeEnvironment: string | null;
  environments: string[];
}

export type NodeKind = "collection" | "folder" | "request";

/** Which wire protocol a request or a recorded send used. */
export type Protocol = "http" | "grpc" | "ws";

export interface TreeNode {
  nodeRef: string;
  name: string;
  kind: NodeKind;
  /** A request's stable id; absent for folders and collections. */
  id?: string | null;
  method?: string | null;
  children: TreeNode[];
}

// -- environments ------------------------------------------------------------

export interface EnvVariable {
  key: string;
  value: string;
  secret: boolean;
  enabled: boolean;
}

export interface Environment {
  version: number;
  name: string;
  variables: EnvVariable[];
}

// -- requests ----------------------------------------------------------------

export interface KeyValue {
  key: string;
  value: string;
  enabled: boolean;
  description?: string | null;
}

export type Auth =
  | { type: "inherit" }
  | { type: "none" }
  | { type: "basic"; username: string; password: string }
  | { type: "bearer"; token: string }
  | { type: "apiKeyHeader"; headerName: string; value: string }
  /**
   * A token produced by running a local command, e.g.
   * `gcloud auth print-identity-token`. Fetched lazily, cached in memory only,
   * and refreshed once if the server rejects it.
   */
  | { type: "commandToken"; command: string; headerName: string; prefix: string };

export type AuthType = Auth["type"];

export type MultipartKind = "text" | "file";

export interface MultipartPart {
  key: string;
  kind: MultipartKind;
  value: string;
  enabled: boolean;
  contentType?: string | null;
}

export type Body =
  | { type: "none" }
  | { type: "json"; text: string }
  | { type: "text"; text: string; contentType?: string | null }
  | { type: "form"; fields: KeyValue[] }
  | { type: "multipart"; parts: MultipartPart[] }
  | { type: "graphql"; query: string; variables: string }
  | { type: "binary"; path: string };

export type BodyType = Body["type"];

export interface Scripts {
  preRequest: string;
  postResponse: string;
}

export interface RequestSettings {
  followRedirects: boolean;
  timeoutMs: number;
  verifyTls: boolean;
}

export interface RequestDef {
  version: number;
  id: string;
  name: string;
  method: string;
  url: string;
  params: KeyValue[];
  headers: KeyValue[];
  auth: Auth;
  body: Body;
  scripts: Scripts;
  settings: RequestSettings;
}

export interface ContainerDef {
  version: number;
  id: string;
  name: string;
  headers: KeyValue[];
  auth: Auth;
  scripts: Scripts;
  description?: string | null;
}

// -- gRPC --------------------------------------------------------------------

export type ProtoSource =
  /** Compile every .proto under `root`, using it as the single import root. */
  | { kind: "directory"; root: string; entryFiles: string[] }
  | { kind: "files"; files: string[]; includePaths: string[] }
  | { kind: "reflection" };

export type ProtoSourceKind = ProtoSource["kind"];

export interface GrpcSettings {
  timeoutMs: number;
  verifyTls: boolean;
  /** Largest response message to accept. Defaults to 16 MB. */
  maxResponseBytes: number;
  /**
   * For a server- or bidirectional-streaming method: stop after this many
   * messages and close the stream. Unset reads until the server ends it (or
   * the deadline passes) — which, for an unbounded feed, means the deadline.
   */
  streamMaxMessages?: number | null;
}

export interface GrpcRequestDef {
  version: number;
  id: string;
  name: string;
  /** http://host:port (plaintext) or https://host:port (TLS). */
  address: string;
  protoSource: ProtoSource;
  /** Fully-qualified service name, e.g. orders.v1.OrderService. */
  service: string;
  method: string;
  /** The gRPC equivalent of headers. */
  metadata: KeyValue[];
  /** Becomes a metadata entry at send time; inherited when set to "inherit". */
  auth: Auth;
  /** The request message as protobuf-JSON text. */
  message: string;
  scripts: Scripts;
  settings: GrpcSettings;
}

export interface MethodInfo {
  name: string;
  input: string;
  output: string;
  clientStreaming: boolean;
  serverStreaming: boolean;
}

export interface ServiceInfo {
  name: string;
  methods: MethodInfo[];
}

export interface GrpcResult {
  /** gRPC status code, 0..=16. 0 is OK. */
  code: number;
  codeName: string;
  statusMessage: string;
  /** Pretty-printed response message ("" on a non-OK status). */
  responseJson: string;
  responseRawJson: string;
  headers: [string, string][];
  trailers: [string, string][];
  durationMs: number;
  responseBytes: number;
  /** "unary" | "server_streaming" | "client_streaming" | "bidi". */
  kind?: string;
  /** Every received message as compact JSON, for a stream; empty for unary. */
  messages?: string[];
  /** Messages received; 1 for a unary reply. */
  messageCount?: number;
  /** Time to the first streamed message, when there was one. */
  firstMessageMs?: number | null;
}

// -- WebSocket ----------------------------------------------------------------

export type WsPayloadKind = "text" | "binary";

/** What to do after sending a message, before the next. */
export type WsWait =
  | { kind: "none" }
  | { kind: "reply" }
  | { kind: "count"; count: number }
  | { kind: "millis"; ms: number };

export interface WsMessageDef {
  kind: WsPayloadKind;
  /** Text, or base64 for a binary frame. Interpolated like any body. */
  body: string;
  wait: WsWait;
  enabled: boolean;
}

export interface WsSettings {
  connectTimeoutMs: number;
  /** How long one wait may take. */
  timeoutMs: number;
  verifyTls: boolean;
  /** Close cleanly once every message has been handled. */
  closeAfter: boolean;
}

/**
 * A WebSocket request is a short session: connect, send messages, wait for
 * what comes back, close. The same file runs in the client and in a load
 * test.
 */
export interface WsRequestDef {
  version: number;
  id: string;
  name: string;
  url: string;
  headers: KeyValue[];
  subprotocols: string[];
  auth: Auth;
  messages: WsMessageDef[];
  settings: WsSettings;
}

export interface WsFrame {
  direction: "out" | "in";
  kind: WsPayloadKind;
  body: string;
  bytes: number;
  /** Milliseconds since the session started. */
  atMs: number;
}

/** A message that waited, and what it got. */
export interface WsExchange {
  messageIndex: number;
  /** Send-to-first-frame; null when nothing arrived in time. */
  latencyMs: number | null;
  framesReceived: number;
  bytesOut: number;
  bytesIn: number;
  timedOut: boolean;
}

export interface WsResult {
  connected: boolean;
  connectMs: number | null;
  durationMs: number;
  messagesSent: number;
  messagesReceived: number;
  bytesOut: number;
  bytesIn: number;
  exchanges: WsExchange[];
  closeCode: number | null;
  closeReason: string;
  frames: WsFrame[];
  error: string | null;
  subprotocol: string | null;
}

export interface SentWsRequest {
  url: string;
  headers: [string, string][];
  subprotocols: string[];
  messages: { kind: WsPayloadKind; body: string; wait: WsWait }[];
}

export interface WsSendResult {
  execId: string;
  requestRef: string;
  /** The measured session, when the handshake got that far. */
  result: WsResult | null;
  /** Why there is no session: a bad URL, an unreachable host. */
  error: string | null;
  unresolved: string[];
  sent: SentWsRequest;
  variablesSet: Record<string, string>;
}

export interface SentGrpcRequest {
  address: string;
  service: string;
  method: string;
  metadata: [string, string][];
  message: string;
}

export interface GrpcSendResult {
  execId: string;
  requestRef: string;
  response: GrpcResult | null;
  error: string | null;
  scriptError: string | null;
  tests: TestResult[];
  console: ConsoleLine[];
  unresolved: string[];
  sent: SentGrpcRequest;
  variablesSet: Record<string, string>;
}


// -- execution ---------------------------------------------------------------

export interface Timings {
  ttfbMs: number;
  downloadMs: number;
  totalMs: number;
}

export interface ResponseHeader {
  key: string;
  value: string;
}

export type BodyPreview =
  | { kind: "text"; text: string; raw: string; language: string }
  | { kind: "image"; dataUrl: string }
  | { kind: "file"; path: string; size: number }
  | { kind: "binary"; size: number; base64Head: string }
  | { kind: "empty" };

export interface CookieRecord {
  domain: string;
  raw: string;
}

export interface ExecResult {
  status: number;
  statusText: string;
  httpVersion: string;
  headers: ResponseHeader[];
  body: BodyPreview;
  bodySize: number;
  timings: Timings;
  finalUrl: string;
  setCookies: CookieRecord[];
}

export interface TestResult {
  name: string;
  passed: boolean;
  error?: string | null;
}

export type LogLevel = "log" | "info" | "warn" | "error";

export interface ConsoleLine {
  level: LogLevel;
  text: string;
}

export interface SentRequest {
  method: string;
  url: string;
  headers: [string, string][];
  body: string | null;
}

export interface SendResult {
  execId: string;
  requestRef: string;
  response: ExecResult | null;
  error: string | null;
  scriptError: string | null;
  tests: TestResult[];
  console: ConsoleLine[];
  unresolved: string[];
  sent: SentRequest;
  variablesSet: Record<string, string>;
}

/** What was actually put on the wire, after variables were resolved. */
export interface HistorySentRequest {
  headers: [string, string][];
  body?: string | null;
  /** The body was cut to keep the log small; it is not the whole thing. */
  bodyTruncated: boolean;
}

/** What came back, kept so a send can be reviewed after the fact. */
export interface HistoryResponse {
  headers: [string, string][];
  body?: string | null;
  /** The body was cut to keep the log small; it is not the whole thing. */
  bodyTruncated: boolean;
}

export interface HistoryEntry {
  id: string;
  /** Where the request was when this was sent. */
  requestRef: string;
  /** Which request it was, so a later rename does not orphan the entry. */
  requestId?: string | null;
  name: string;
  protocol: Protocol;
  /** The HTTP verb, "GRPC", or "WS". */
  method: string;
  url: string;
  /** The HTTP status, or the gRPC code. Zero when the send never completed. */
  status: number;
  statusText?: string | null;
  durationMs: number;
  responseBytes: number;
  at: number;
  ok: boolean;
  /** Why the send failed, when it produced no response at all. */
  error?: string | null;
  request: HistorySentRequest;
  response: HistoryResponse;
}

// -- import ------------------------------------------------------------------

export interface ImportReport {
  collectionRef: string;
  collectionName: string;
  requestsImported: number;
  foldersImported: number;
  environmentCreated: string | null;
  warnings: string[];
}

// -- load tests --------------------------------------------------------------

export type LoadMode = "closed" | "open";
/** A load test, or a folder grouping them. */
export type LoadTestKind = "scenario" | "userScript" | "folder";

export interface LoadTestEntry {
  nodeRef: string;
  name: string;
  kind: LoadTestKind;
  /** The scenario's stable id; absent for user scripts and folders. */
  id?: string | null;
  /** Populated only by the tree listing; empty in the flat one. */
  children?: LoadTestEntry[];
}

export interface Stage {
  durationSec: number;
  target: number;
  /**
   * Add `target` to where the ramp has reached, instead of setting it.
   *
   * This is what makes a repeated block useful: "another hundred a second"
   * means something different each time round. A relative target of zero
   * holds the current rate.
   */
  relative?: boolean;
}

/** A group of stages run several times over. */
export interface RepeatBlock {
  times: number;
  stages: Stage[];
  /**
   * Multiply every duration in the block by this, once per repetition.
   * 1.5 makes each pass half again as long as the one before.
   */
  durationScale?: number | null;
}

/**
 * One round of a fixed-count run: send this many, then pause.
 *
 * The counterpart of `Stage` for runs measured in requests rather than time.
 */
export interface Blast {
  iterations: number;
  concurrency: number;
  /** Seconds to wait after this round, before the next one starts. */
  gapSec?: number;
  /**
   * Add `iterations` to the previous round instead of setting it outright —
   * what makes a repeated block worth having, since "another thousand" means
   * something different each time round.
   */
  relative?: boolean;
}

/** A group of rounds run several times over. */
export interface BlastRepeat {
  times: number;
  blasts: Blast[];
  /** Multiply every count in the block by this, once per repetition. */
  iterationScale?: number | null;
}

/** One entry in a fixed-count run: either a round, or a repeated block. */
export type BlastItem = Blast | BlastRepeat;

export function isBlastRepeat(item: BlastItem): item is BlastRepeat {
  return (item as BlastRepeat).blasts !== undefined;
}

/**
 * The rounds as the run will execute them, with repeats unrolled and relative
 * counts resolved. Mirrors `swarmo_core::model::flatten_blasts`.
 */
export function flattenBlasts(items: BlastItem[]): Blast[] {
  const out: Blast[] = [];
  let current = 0;
  const push = (b: Blast) => {
    const iterations = Math.round(
      b.relative ? Math.max(0, current + b.iterations) : b.iterations,
    );
    current = iterations;
    out.push({
      iterations,
      concurrency: Math.max(1, b.concurrency),
      gapSec: b.gapSec ?? 0,
    });
  };
  for (const item of items) {
    if (isBlastRepeat(item)) {
      const scale =
        item.iterationScale && item.iterationScale > 0 ? item.iterationScale : 1;
      for (let pass = 0; pass < Math.max(1, item.times); pass++) {
        // From the original count each pass, not the previous one, so rounding
        // does not compound. Mirrors `BlastItem::blasts` in Rust.
        const factor = Math.pow(scale, pass);
        for (const b of item.blasts) {
          push({ ...b, iterations: Math.round(b.iterations * factor) });
        }
      }
    } else {
      push(item);
    }
  }
  return out;
}

/**
 * The rounds a scenario means, however it happens to spell them.
 *
 * An explicit `blasts` list wins; a plain `iterations` is the single round it
 * has always meant. Mirrors `LoadScenario::rounds` in Rust so the editor and
 * the engine never disagree about what a scenario says.
 */
export function scenarioRounds(s: LoadScenario): BlastItem[] {
  if (s.blasts?.length) return s.blasts;
  if (s.iterations != null) {
    return [{ iterations: s.iterations, concurrency: s.concurrency ?? 16, gapSec: 0 }];
  }
  return [];
}

/**
 * End a run early once it has shown what it was asked to find.
 *
 * Not a threshold: breaching a threshold means the run failed, whereas
 * reaching a stop condition is the point. Measured over the most recent
 * interval, so a service that fails after ten clean minutes stops promptly.
 */
export interface StopCondition {
  /** "errorRate" | "p95" | "p99" | "rps" */
  metric: string;
  above?: number | null;
  below?: number | null;
  /** Consecutive intervals it must hold for, so one blip does not end a run. */
  forIntervals: number;
}

/** One entry in a ramp: either a stage, or a repeated block. */
export type StageItem = Stage | RepeatBlock;

export function isRepeat(item: StageItem): item is RepeatBlock {
  return (item as RepeatBlock).stages !== undefined;
}

/**
 * The ramp as the run will execute it, with repeats unrolled and relative
 * targets resolved. Mirrors `swarmo_core::model::flatten_stages`.
 */
export function flattenStages(items: StageItem[]): Stage[] {
  const out: Stage[] = [];
  let current = 0;
  const push = (s: Stage) => {
    const target = s.relative ? Math.max(0, current + s.target) : s.target;
    current = target;
    out.push({ durationSec: s.durationSec, target });
  };
  for (const item of items) {
    if (isRepeat(item)) {
      const scale = item.durationScale && item.durationScale > 0 ? item.durationScale : 1;
      for (let pass = 0; pass < Math.max(1, item.times); pass++) {
        // From the original duration each pass, not the previous one, so
        // rounding does not compound. Mirrors `StageItem::stages` in Rust.
        const factor = Math.pow(scale, pass);
        for (const s of item.stages) {
          push({ ...s, durationSec: Math.round(s.durationSec * factor) });
        }
      }
    } else {
      push(item);
    }
  }
  return out;
}

export type CaptureSource = "body" | "header";

export interface Capture {
  from: CaptureSource;
  jsonPath?: string | null;
  name?: string | null;
  as: string;
}

export interface LoadStep {
  requestRef: string;
  /** Which request this is, independent of where it lives. */
  requestId?: string | null;
  /** Run concurrently with the step above, instead of after it. */
  parallel?: boolean;
  thinkTimeMs?: [number, number] | null;
  capture: Capture[];
  tag?: string | null;
}

export type ThresholdOp = "<" | "<=" | ">" | ">=";

export interface Threshold {
  metric: string;
  stat: string;
  op: ThresholdOp;
  valueMs?: number | null;
  value?: number | null;
  abortOnFail: boolean;
}

export interface LoadScenario {
  version: number;
  name: string;
  mode: LoadMode;
  /** Ramps, and repeated blocks of them. */
  stages: StageItem[];
  /** End the run early once this holds, for finding a breaking point. */
  stopWhen?: StopCondition | null;
  /**
   * Run exactly this many iterations and stop — "send 5,000 requests and tell
   * me how long it took". Stages and rates are ignored when set.
   */
  iterations?: number | null;
  /** Workers running those iterations at once. */
  concurrency?: number | null;
  /**
   * Several rounds of a fixed-count run, with pauses between them. Supersedes
   * `iterations`/`concurrency` when non-empty.
   */
  blasts?: BlastItem[];
  /**
   * Hold one rate for the whole run instead of ramping (open mode only).
   * Stage targets are ignored when this is set.
   */
  arrivalRatePerSec?: number | null;
  /** Run length when there are no stages. */
  durationSec?: number | null;
  maxVus: number;
  environment?: string | null;
  steps: LoadStep[];
  thresholds: Threshold[];
  runScripts: boolean;
  newConnectionPerIteration: boolean;
  verifyTls: boolean;
  timeoutMs: number;
}

export interface Preflight {
  name: string;
  mode: LoadMode;
  durationSec: number;
  peakTarget: number;
  maxVus: number;
  hosts: string[];
  unapprovedHosts: string[];
  /** Commands this run would execute to produce auth tokens. */
  authCommands: string[];
  /** Of those, the ones not yet approved in this workspace. */
  unapprovedAuthCommands: string[];
  scripted: boolean;
}

// -- run results -------------------------------------------------------------

export interface TagStats {
  tag: string;
  count: number;
  errors: number;
  /** The fastest response seen. */
  min: number;
  p50: number;
  p90: number;
  p95: number;
  p99: number;
  /** The far tail — at 10k requests, the slowest 10. */
  p999: number;
  max: number;
  avg: number;
  /** Response bytes received for this tag. */
  bytesIn: number;
  /** Request body bytes sent for this tag. */
  bytesOut: number;
}

/**
 * How many responses carried a given status.
 *
 * Keyed by protocol as well as code: the two schemes collide at zero, where
 * gRPC means OK and HTTP means the request never got a response at all.
 */
export interface StatusCount {
  protocol: Protocol;
  code: number;
  count: number;
}

/**
 * One bar of a latency distribution.
 *
 * Percentiles say where the mass sits but not what shape it is; this is what
 * tells a bimodal run apart from a uniformly mediocre one.
 */
export interface DistBucket {
  upperMs: number;
  count: number;
}

/** How many requests failed with a given cause. */
export interface ErrorCount {
  message: string;
  count: number;
}

export interface CheckStats {
  name: string;
  passes: number;
  fails: number;
}

export interface ThresholdResult {
  description: string;
  metric: string;
  stat: string;
  target: number;
  actual: number;
  passed: boolean;
}

export interface Snapshot {
  elapsedSec: number;
  activeVus: number;
  targetVus: number;
  rps: number;
  errorRate: number;
  p50: number;
  p95: number;
  p99: number;
  perTag: TagStats[];
  checks: CheckStats[];
  totalRequests: number;
  totalErrors: number;
  samplesDropped: number;
  droppedIterations: number;
  vusSaturated: boolean;
  thresholdResults: ThresholdResult[];
  /** Response bytes received so far. */
  bytesIn: number;
  /** Bytes received per second over the last interval. */
  bytesPerSec: number;
  /** Request body bytes sent, and the recent send rate. */
  bytesOut: number;
  bytesOutPerSec: number;
  /** Failures as a share of the last interval only. */
  intervalErrorRate: number;
  intervalP95: number;
  intervalP99: number;
}

export type RunState = "running" | "passed" | "failed" | "stopped" | "errored";

/**
 * How one round of a fixed-count run went.
 *
 * A repeated blast is only worth running twice if the passes can be compared,
 * and a whole-run average hides exactly what you were looking for.
 */
export interface RoundStats {
  /** 1-based, in execution order. */
  index: number;
  /** Requests the round was asked to send. */
  planned: number;
  concurrency: number;
  /** Seconds the round itself took, excluding the pause after it. */
  wallSec: number;
  /** Requests per second within the round, so gaps do not depress it. */
  rps: number;
  /** The pause that followed this round. */
  gapSec?: number;
  /** Counts, latency and bytes for this round alone. */
  stats: TagStats;
}

export interface RunSummary {
  version: number;
  runId: string;
  scenarioName: string;
  scenarioRef: string;
  /** The scenario's stable id, so "run again" survives a rename. */
  scenarioId?: string | null;
  startedAt: number;
  endedAt: number;
  durationSec: number;
  state: RunState;
  error?: string | null;
  totalRequests: number;
  totalErrors: number;
  errorRate: number;
  rps: number;
  overall: TagStats;
  perTag: TagStats[];
  checks: CheckStats[];
  thresholds: ThresholdResult[];
  samplesDropped: number;
  droppedIterations: number;
  statusCodes: StatusCount[];
  /** Failures grouped by cause, most frequent first. */
  errorsByMessage: ErrorCount[];
  bytesIn: number;
  bytesPerSec: number;
  /** The highest one-second rate reached, as opposed to the mean. */
  peakRps: number;
  /** Total request body bytes sent, and the mean send rate. */
  bytesOut: number;
  bytesOutPerSec: number;
  /** Empty for runs recorded before the distribution was captured. */
  latencyDistribution: DistBucket[];
  /** How many times an auth token had to be refreshed mid-run. */
  tokenRefreshes: number;
  /** Per-round results for a fixed-count run; absent for other shapes. */
  rounds?: RoundStats[];
  /** Why the run ended before its ramp finished, if it did. */
  stoppedBecause?: string | null;
}

export interface RunListEntry {
  runId: string;
  scenarioName: string;
  /** The scenario's stable id, so scoping survives a rename or a move. */
  scenarioId?: string | null;
  /** Where the scenario lived when this ran — a display hint, not an identity. */
  scenarioRef?: string;
  startedAt: number;
  state: RunState;
  totalRequests: number;
  errorRate: number;
  p95: number;
  /** The user's own name for this run, shown instead of the scenario name. */
  label?: string | null;
  hasNotes: boolean;
}

/** A user's own name and notes for a run, stored beside its summary. */
export interface RunAnnotation {
  label?: string | null;
  notes?: string | null;
}

// -- settings ----------------------------------------------------------------

export interface Settings {
  theme: "system" | "light" | "dark";
  proxy: string | null;
  defaultTimeoutMs: number;
  defaultVerifyTls: boolean;
  recentWorkspaces: string[];
}

// -- events ------------------------------------------------------------------

export interface SnapshotEvent {
  runId: string;
  snapshot: Snapshot;
}

export interface DoneEvent {
  runId: string;
  summary: RunSummary;
}

/** The URL a send would hit, resolved without sending. */
export interface ResolvedUrl {
  url: string;
  /** Variables that had no value; they remain as `{{name}}` in `url`. */
  unresolved: string[];
}

/** One expansion of `{{...}}` text, for the variable picker's live preview. */
export interface TokenPreview {
  /** The expanded text, cut short when very long. */
  value: string;
  truncated: boolean;
  /** Full length before truncation. */
  length: number;
  /** Tokens that could not be understood, left in place in `value`. */
  unresolved: string[];
}
