/**
 * The only place in the frontend that calls `invoke`. Every command is wrapped
 * here with real types so views never guess at the Rust signatures.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  ContainerDef,
  CookieRecord,
  DoneEvent,
  Environment,
  GrpcRequestDef,
  GrpcSendResult,
  HistoryEntry,
  ImportReport,
  ServiceInfo,
  LoadScenario,
  LoadTestEntry,
  Preflight,
  RequestDef,
  RunAnnotation,
  RunListEntry,
  ResolvedUrl,
  RunSummary,
  TokenPreview,
  WsRequestDef,
  WsSendResult,
  SendResult,
  Settings,
  Snapshot,
  SnapshotEvent,
  TreeNode,
  WorkspaceInfo,
} from "./types";

export * from "./types";
export * from "./status";

/** Tauri returns plain strings for errors; keep them intact for the UI. */
export class CommandError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "CommandError";
  }
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(cmd, args);
  } catch (e) {
    throw new CommandError(typeof e === "string" ? e : String(e));
  }
}

// -- workspace ---------------------------------------------------------------

export const workspaceOpen = (path: string) =>
  call<WorkspaceInfo>("workspace_open", { path });
export const workspaceCreate = (path: string, name: string) =>
  call<WorkspaceInfo>("workspace_create", { path, name });
export const workspaceInfo = () => call<WorkspaceInfo | null>("workspace_info");
export const workspaceRecent = () => call<string[]>("workspace_recent");
export const workspaceClose = () => call<void>("workspace_close");
export const treeGet = () => call<TreeNode[]>("tree_get");

// -- environments ------------------------------------------------------------

export const envList = () => call<string[]>("env_list");
export const envGet = (name: string) => call<Environment>("env_get", { name });
export const envSave = (env: Environment) => call<void>("env_save", { env });
export const envCreate = (name: string) => call<string>("env_create", { name });
export const envDelete = (name: string) => call<void>("env_delete", { name });
export const envSetActive = (name: string | null) =>
  call<void>("env_set_active", { name });
export const envEffective = () => call<Record<string, string>>("env_effective");
export const runtimeVarsClear = () => call<void>("runtime_vars_clear");
/** Expand `{{...}}` exactly as a send would, for the variable picker's preview. */
export const tokenPreview = (text: string) =>
  call<TokenPreview>("token_preview", { text });

// -- requests ----------------------------------------------------------------

export const requestGet = (nodeRef: string) =>
  call<RequestDef>("request_get", { nodeRef });
/** The fully resolved URL — variables filled, query params appended. */
export const requestResolveUrl = (nodeRef: string) =>
  call<ResolvedUrl>("request_resolve_url", { nodeRef });
export const requestSave = (nodeRef: string, def: RequestDef) =>
  call<void>("request_save", { nodeRef, def });
export const requestCreate = (parentRef: string, name: string) =>
  call<string>("request_create", { parentRef, name });
export const requestRename = (nodeRef: string, name: string) =>
  call<string>("request_rename", { nodeRef, name });
export const requestDuplicate = (nodeRef: string) =>
  call<string>("request_duplicate", { nodeRef });
export const requestMove = (nodeRef: string, newParentRef: string) =>
  call<string>("request_move", { nodeRef, newParentRef });

// -- containers --------------------------------------------------------------

export const collectionCreate = (name: string) =>
  call<string>("collection_create", { name });
export const folderCreate = (parentRef: string, name: string) =>
  call<string>("folder_create", { parentRef, name });
export const containerGet = (nodeRef: string) =>
  call<ContainerDef>("container_get", { nodeRef });
export const containerSave = (nodeRef: string, def: ContainerDef) =>
  call<void>("container_save", { nodeRef, def });
export const containerRename = (nodeRef: string, name: string) =>
  call<string>("container_rename", { nodeRef, name });
export const nodeDelete = (nodeRef: string) =>
  call<void>("node_delete", { nodeRef });

// -- import ------------------------------------------------------------------

export const importPostmanCollection = (filePath: string) =>
  call<ImportReport>("import_postman_collection", { filePath });
export const importPostmanEnvironment = (filePath: string) =>
  call<string>("import_postman_environment", { filePath });

// -- execution ---------------------------------------------------------------

export const requestSend = (nodeRef: string, execId: string) =>
  call<SendResult>("request_send", { nodeRef, execId });
export const requestCancel = (execId: string) =>
  call<boolean>("request_cancel", { execId });
// -- gRPC --------------------------------------------------------------------

export const grpcRequestGet = (nodeRef: string) =>
  call<GrpcRequestDef>("grpc_request_get", { nodeRef });
export const grpcRequestSave = (nodeRef: string, def: GrpcRequestDef) =>
  call<void>("grpc_request_save", { nodeRef, def });
export const grpcRequestCreate = (parentRef: string, name: string) =>
  call<string>("grpc_request_create", { parentRef, name });
/**
 * Loads the schema (from .proto files or reflection) and lists what it offers.
 *
 * `def` is the editor's current state, not what is saved, so changing the proto
 * source or the address takes effect without saving first. It also carries the
 * metadata a reflection fetch needs when the server requires auth.
 */
export const grpcListServices = (
  nodeRef: string,
  def: GrpcRequestDef,
  refresh = false,
) => call<ServiceInfo[]>("grpc_list_services", { nodeRef, def, refresh });
export const grpcMessageTemplate = (
  nodeRef: string,
  def: GrpcRequestDef,
  service: string,
  method: string,
) => call<string>("grpc_message_template", { nodeRef, def, service, method });
export const grpcSend = (nodeRef: string, execId: string) =>
  call<GrpcSendResult>("grpc_send", { nodeRef, execId });

/** True when a ref addresses a gRPC request rather than an HTTP one. */
export const isGrpcRef = (nodeRef: string) => nodeRef.endsWith(".grpc.json");
export const isWsRef = (nodeRef: string) => nodeRef.endsWith(".ws.json");

// -- WebSocket ----------------------------------------------------------------

export const wsRequestGet = (nodeRef: string) =>
  call<WsRequestDef>("ws_request_get", { nodeRef });
export const wsRequestSave = (nodeRef: string, def: WsRequestDef) =>
  call<void>("ws_request_save", { nodeRef, def });
export const wsRequestCreate = (parentRef: string, name: string) =>
  call<string>("ws_request_create", { parentRef, name });
/** Run the session; cancel through `requestCancel` with the same execId. */
export const wsSend = (nodeRef: string, execId: string) =>
  call<WsSendResult>("ws_send", { nodeRef, execId });

export const historyList = () => call<HistoryEntry[]>("history_list");
export const historyClear = () => call<void>("history_clear");
export const historyDelete = (id: string) => call<void>("history_delete", { id });
export const cookiesList = () => call<CookieRecord[]>("cookies_list");
export const cookiesClear = () => call<void>("cookies_clear");

// -- load tests --------------------------------------------------------------

export const loadList = () => call<LoadTestEntry[]>("load_list");
export const scenarioGet = (nodeRef: string) =>
  call<LoadScenario>("scenario_get", { nodeRef });
export const scenarioSave = (nodeRef: string, scenario: LoadScenario) =>
  call<void>("scenario_save", { nodeRef, scenario });
/** The load tests nested by folder, for the sidebar. */
export const loadTree = () => call<LoadTestEntry[]>("load_tree");
export const loadFolderCreate = (parentRef: string, name: string) =>
  call<string>("load_folder_create", { parentRef, name });
export const loadFolderRename = (nodeRef: string, name: string) =>
  call<string>("load_folder_rename", { nodeRef, name });
export const loadTestMove = (nodeRef: string, newParentRef: string) =>
  call<string>("load_test_move", { nodeRef, newParentRef });

export const scenarioCreate = (name: string, parentRef?: string) =>
  call<string>("scenario_create", { name, parentRef: parentRef ?? null });
export const userScriptCreate = (name: string, parentRef?: string) =>
  call<string>("user_script_create", { name, parentRef: parentRef ?? null });
export const userScriptTemplate = () => call<string>("user_script_template");
export const textGet = (nodeRef: string) => call<string>("text_get", { nodeRef });
export const textSave = (nodeRef: string, text: string) =>
  call<void>("text_save", { nodeRef, text });
export const loadPromote = (requestRefs: string[], name: string) =>
  call<string>("load_promote", { requestRefs, name });
export const loadPreflight = (nodeRef: string) =>
  call<Preflight>("load_preflight", { nodeRef });
export const loadApproveHosts = (hosts: string[]) =>
  call<void>("load_approve_hosts", { hosts });
export const loadRun = (nodeRef: string) => call<string>("load_run", { nodeRef });
export const loadStop = (runId: string) => call<boolean>("load_stop", { runId });
export const loadActiveRuns = () => call<string[]>("load_active_runs");

// -- runs --------------------------------------------------------------------

/** Run an auth command once; the token is masked, never returned in full. */
export const authCommandTest = (command: string) =>
  call<{ masked: string; length: number; expiresInSec: number | null }>("auth_command_test", {
    command,
  });
export const authCommandApprove = (command: string) =>
  call<void>("auth_command_approve", { command });
export const authCommandsApproved = () => call<string[]>("auth_commands_approved");

export const requestLocate = (nodeRef: string, id?: string | null) =>
  call<string | null>("request_locate", { nodeRef, id: id ?? null });
export const loadTestLocate = (nodeRef: string, id?: string | null) =>
  call<string | null>("load_test_locate", { nodeRef, id: id ?? null });
export const loadTestDuplicate = (nodeRef: string) =>
  call<string>("load_test_duplicate", { nodeRef });
export const loadTestRename = (nodeRef: string, newName: string) =>
  call<string>("load_test_rename", { nodeRef, newName });
export const environmentRename = (oldName: string, newName: string) =>
  call<void>("environment_rename", { oldName, newName });

export const curlParse = (text: string) =>
  call<{ def: RequestDef; warnings: string[] }>("curl_parse", { text });
export const curlImport = (parentRef: string, text: string) =>
  call<string>("curl_import", { parentRef, text });

export const runsList = () => call<RunListEntry[]>("runs_list");
export const runGet = (runId: string) => call<RunSummary>("run_get", { runId });
export const runTimeline = (runId: string) =>
  call<Snapshot[]>("run_timeline", { runId });
export const runExport = (runId: string, path: string, format: "html" | "json") =>
  call<void>("run_export", { runId, path, format });
export const runAnnotationGet = (runId: string) =>
  call<RunAnnotation>("run_annotation_get", { runId });
export const runAnnotationSet = (
  runId: string,
  label: string | null,
  notes: string | null,
) => call<RunAnnotation>("run_annotation_set", { runId, label, notes });
export const runDelete = (runId: string) => call<void>("run_delete", { runId });

// -- settings ----------------------------------------------------------------

export const settingsGet = () => call<Settings>("settings_get");
export const settingsSave = (settings: Settings) =>
  call<void>("settings_save", { settings });
export const approvedHostsClear = () => call<void>("approved_hosts_clear");

// -- events ------------------------------------------------------------------

export const onLoadSnapshot = (fn: (e: SnapshotEvent) => void): Promise<UnlistenFn> =>
  listen<SnapshotEvent>("load://snapshot", (ev) => fn(ev.payload));

export const onLoadDone = (fn: (e: DoneEvent) => void): Promise<UnlistenFn> =>
  listen<DoneEvent>("load://done", (ev) => fn(ev.payload));
