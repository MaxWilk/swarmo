import { useEffect, useRef, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import * as api from "../../api";
import type { ImportReport, NodeKind, TreeNode } from "../../api";
import { CurlImportModal } from "./CurlImportModal";
import { useWorkspace } from "../../stores/workspace";
import { useTabs } from "../../stores/tabs";
import { notify, reportError } from "../../stores/toast";
import {
  ConfirmModal,
  Icons,
  MethodLabel,
  Modal,
  PromptModal,
} from "../../components/ui";
import { EnvironmentModal } from "./EnvironmentModal";

interface ContextMenuState {
  x: number;
  y: number;
  node: TreeNode;
}

type PendingPrompt =
  | { kind: "newCollection" }
  | { kind: "newFolder"; parentRef: string }
  | { kind: "newRequest"; parentRef: string }
  | { kind: "newGrpcRequest" | "newWsRequest"; parentRef: string }
  | { kind: "rename"; node: TreeNode }
  | { kind: "promote"; node: TreeNode };

type PendingConfirm = { node: TreeNode };

/** Simple case-insensitive substring filter that also keeps ancestors visible. */
function filterTree(nodes: TreeNode[], query: string): TreeNode[] {
  if (!query.trim()) return nodes;
  const q = query.toLowerCase();
  const walk = (list: TreeNode[]): TreeNode[] =>
    list
      .map((n): TreeNode | null => {
        const children = walk(n.children);
        const selfMatch = n.name.toLowerCase().includes(q);
        if (selfMatch || children.length) {
          return { ...n, children };
        }
        return null;
      })
      .filter((n): n is TreeNode => n !== null);
  return walk(nodes);
}

export function CollectionSidebar() {
  const { tree, environments, activeEnv, setActiveEnv } = useWorkspace();
  const open_ = useTabs((s) => s.open);
  const rekey = useTabs((s) => s.rekey);

  const [expanded, setExpanded] = useState<Set<string>>(() => new Set());
  const [seededDefaults, setSeededDefaults] = useState(false);
  const [filter, setFilter] = useState("");
  const [ctxMenu, setCtxMenu] = useState<ContextMenuState | null>(null);
  const [addMenuOpen, setAddMenuOpen] = useState(false);
  const [showEnvModal, setShowEnvModal] = useState(false);
  const [prompt, setPrompt] = useState<PendingPrompt | null>(null);
  const [confirm, setConfirm] = useState<PendingConfirm | null>(null);
  const [importReport, setImportReport] = useState<ImportReport | null>(null);
  // The collection a pasted cURL command will land in, or null when closed.
  const [curlTarget, setCurlTarget] = useState<{ ref: string; name: string } | null>(null);
  const [dragRef, setDragRef] = useState<string | null>(null);
  const [dropTarget, setDropTarget] = useState<string | null>(null);
  const addMenuHost = useRef<HTMLDivElement>(null);

  // Collections expanded by default the first time the tree loads.
  useEffect(() => {
    if (seededDefaults || !tree.length) return;
    setExpanded(new Set(tree.map((n) => n.nodeRef)));
    setSeededDefaults(true);
  }, [tree, seededDefaults]);

  useEffect(() => {
    if (!ctxMenu) return;
    const close = () => setCtxMenu(null);
    window.addEventListener("mousedown", close);
    window.addEventListener("keydown", onEscape);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("keydown", onEscape);
    };
    function onEscape(e: KeyboardEvent) {
      if (e.key === "Escape") setCtxMenu(null);
    }
  }, [ctxMenu]);

  useEffect(() => {
    if (!addMenuOpen) return;
    const close = (e: MouseEvent) => {
      if (!addMenuHost.current?.contains(e.target as Node)) setAddMenuOpen(false);
    };
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, [addMenuOpen]);

  const toggle = (ref: string) =>
    setExpanded((s) => {
      const next = new Set(s);
      if (next.has(ref)) next.delete(ref);
      else next.add(ref);
      return next;
    });

  const visibleTree = filterTree(tree, filter);

  async function doRefresh() {
    await useWorkspace.getState().refreshTree();
  }

  async function handleNewCollection(name: string) {
    try {
      await api.collectionCreate(name);
      await doRefresh();
    } catch (e) {
      reportError("Could not create the collection", e);
    }
  }

  async function handleNewFolder(parentRef: string, name: string) {
    try {
      await api.folderCreate(parentRef, name);
      await doRefresh();
    } catch (e) {
      reportError("Could not create the folder", e);
    }
  }

  async function handleNewRequest(parentRef: string, name: string) {
    try {
      const ref = await api.requestCreate(parentRef, name);
      await doRefresh();
      await open_(ref);
    } catch (e) {
      reportError("Could not create the request", e);
    }
  }

  async function handleNewWsRequest(parentRef: string, name: string) {
    try {
      const ref = await api.wsRequestCreate(parentRef, name);
      await doRefresh();
      await open_(ref);
    } catch (e) {
      reportError("Could not create the WebSocket request", e);
    }
  }

  async function handleNewGrpcRequest(parentRef: string, name: string) {
    try {
      const ref = await api.grpcRequestCreate(parentRef, name);
      await doRefresh();
      await open_(ref);
    } catch (e) {
      reportError("Could not create the gRPC request", e);
    }
  }

  async function handleRename(node: TreeNode, name: string) {
    try {
      const newRef =
        node.kind === "request"
          ? await api.requestRename(node.nodeRef, name)
          : await api.containerRename(node.nodeRef, name);
      // A container rename moves every request inside it, so open tabs have to
      // follow even though their own names are unchanged.
      rekey(node.nodeRef, newRef, node.kind === "request" ? name : "");
      await doRefresh();
    } catch (e) {
      reportError("Could not rename", e);
    }
  }

  async function handleDuplicate(node: TreeNode) {
    try {
      await api.requestDuplicate(node.nodeRef);
      await doRefresh();
    } catch (e) {
      reportError("Could not duplicate the request", e);
    }
  }

  async function handleDelete(node: TreeNode) {
    try {
      await api.nodeDelete(node.nodeRef);
      // Any editor still open on the deleted request — or on a request
      // inside a deleted folder — would write the file straight back on
      // its next save, directory and all.
      const gone = (ref: string) => ref === node.nodeRef || ref.startsWith(`${node.nodeRef}/`);
      const { tabs, close } = useTabs.getState();
      for (const t of tabs) {
        if (gone(t.nodeRef)) close(t.nodeRef);
      }
      await doRefresh();
    } catch (e) {
      reportError("Could not delete", e);
    }
  }

  async function handlePromote(node: TreeNode, name: string) {
    try {
      await api.loadPromote([node.nodeRef], name);
      notify("Promoted to a load test", `"${node.name}" is now available under Load.`);
    } catch (e) {
      reportError("Could not promote the request", e);
    }
  }

  async function handleImportCollection() {
    setAddMenuOpen(false);
    try {
      const path = await open({ filters: [{ name: "JSON", extensions: ["json"] }] });
      if (!path || Array.isArray(path)) return;
      const report = await api.importPostmanCollection(path);
      await doRefresh();
      setImportReport(report);
    } catch (e) {
      reportError("Could not import the Postman collection", e);
    }
  }

  async function handleImportEnvironment() {
    setAddMenuOpen(false);
    try {
      const path = await open({ filters: [{ name: "JSON", extensions: ["json"] }] });
      if (!path || Array.isArray(path)) return;
      await api.importPostmanEnvironment(path);
      await useWorkspace.getState().refreshEnvs();
      notify("Environment imported");
    } catch (e) {
      reportError("Could not import the Postman environment", e);
    }
  }

  async function handleDrop(nodeRef: string, newParentRef: string) {
    if (nodeRef === newParentRef) return;
    try {
      const newRef = await api.requestMove(nodeRef, newParentRef);
      await doRefresh();
      const node = findByRef(tree, nodeRef);
      rekey(nodeRef, newRef, node?.name ?? "");
    } catch (e) {
      reportError("Could not move the request", e);
    }
  }

  return (
    <aside className="sidebar">
      <div className="panel-header">
        <select
          className="select"
          value={activeEnv ?? ""}
          onChange={(e) => void setActiveEnv(e.target.value || null)}
          title="Active environment"
        >
          <option value="">No environment</option>
          {environments.map((name) => (
            <option key={name} value={name}>
              {name}
            </option>
          ))}
        </select>
        <button
          className="btn ghost icon sm"
          title="Manage environments"
          onClick={() => setShowEnvModal(true)}
        >
          <Icons.Settings />
        </button>
        <div className="right" ref={addMenuHost} style={{ position: "relative" }}>
          <button
            className="btn ghost icon sm"
            title="Add"
            onClick={() => setAddMenuOpen((v) => !v)}
          >
            <Icons.Plus />
          </button>
          {addMenuOpen && (
            <div
              className="modal"
              style={{ position: "absolute", right: 0, top: 30, width: 220, zIndex: 50 }}
            >
              <div className="modal-body" style={{ padding: 6, gap: 0 }}>
                <MenuItem
                  label="New collection"
                  onClick={() => {
                    setAddMenuOpen(false);
                    setPrompt({ kind: "newCollection" });
                  }}
                />
                <MenuItem
                  label="Import Postman collection"
                  onClick={() => void handleImportCollection()}
                />
                <MenuItem
                  label="Import Postman environment"
                  onClick={() => void handleImportEnvironment()}
                />
                <MenuItem
                  label="Import from cURL…"
                  onClick={() => {
                    setAddMenuOpen(false);
                    // The first collection, which is where a new request
                    // would go too.
                    const target = tree.find((n) => n.kind === "collection");
                    if (!target) {
                      reportError(
                        "Create a collection first",
                        "There is nowhere to put an imported request yet.",
                      );
                      return;
                    }
                    setCurlTarget({ ref: target.nodeRef, name: target.name });
                  }}
                />
              </div>
            </div>
          )}
        </div>
      </div>

      <div style={{ padding: "8px 8px 4px" }}>
        <input
          className="input sm"
          placeholder="Filter requests"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
      </div>

      <div className="scroll">
        {visibleTree.map((node) => (
          <TreeRow
            key={node.nodeRef}
            node={node}
            depth={0}
            expanded={expanded}
            onToggle={toggle}
            onOpen={(ref) => void open_(ref)}
            onContextMenu={(e, n) => {
              e.preventDefault();
              setCtxMenu({ x: e.clientX, y: e.clientY, node: n });
            }}
            dragRef={dragRef}
            dropTarget={dropTarget}
            onDragStart={setDragRef}
            onDragOverNode={setDropTarget}
            onDropNode={(target) => {
              if (dragRef) void handleDrop(dragRef, target);
              setDragRef(null);
              setDropTarget(null);
            }}
          />
        ))}
        {!tree.length && <div className="empty">No collections yet. Use + to add one.</div>}
      </div>

      {ctxMenu && (
        <div
          className="modal"
          style={{ position: "fixed", left: ctxMenu.x, top: ctxMenu.y, width: 200, zIndex: 200 }}
          onMouseDown={(e) => e.stopPropagation()}
        >
          <div className="modal-body" style={{ padding: 6, gap: 0 }}>
            {ctxMenu.node.kind !== "request" ? (
              <>
                <MenuItem
                  label="New request"
                  onClick={() => {
                    setPrompt({ kind: "newRequest", parentRef: ctxMenu.node.nodeRef });
                    setCtxMenu(null);
                  }}
                />
                <MenuItem
                  label="New gRPC request"
                  onClick={() => {
                    setPrompt({ kind: "newGrpcRequest", parentRef: ctxMenu.node.nodeRef });
                    setCtxMenu(null);
                  }}
                />
                <MenuItem
                  label="New WebSocket request"
                  onClick={() => {
                    setPrompt({ kind: "newWsRequest", parentRef: ctxMenu.node.nodeRef });
                    setCtxMenu(null);
                  }}
                />
                <MenuItem
                  label="New folder"
                  onClick={() => {
                    setPrompt({ kind: "newFolder", parentRef: ctxMenu.node.nodeRef });
                    setCtxMenu(null);
                  }}
                />
                <MenuItem
                  label="Rename"
                  onClick={() => {
                    setPrompt({ kind: "rename", node: ctxMenu.node });
                    setCtxMenu(null);
                  }}
                />
                <MenuItem
                  label="Delete"
                  danger
                  onClick={() => {
                    setConfirm({ node: ctxMenu.node });
                    setCtxMenu(null);
                  }}
                />
              </>
            ) : (
              <>
                <MenuItem
                  label="Rename"
                  onClick={() => {
                    setPrompt({ kind: "rename", node: ctxMenu.node });
                    setCtxMenu(null);
                  }}
                />
                <MenuItem
                  label="Duplicate"
                  onClick={() => {
                    void handleDuplicate(ctxMenu.node);
                    setCtxMenu(null);
                  }}
                />
                <MenuItem
                  label="Promote to load test"
                  onClick={() => {
                    setPrompt({ kind: "promote", node: ctxMenu.node });
                    setCtxMenu(null);
                  }}
                />
                <MenuItem
                  label="Delete"
                  danger
                  onClick={() => {
                    setConfirm({ node: ctxMenu.node });
                    setCtxMenu(null);
                  }}
                />
              </>
            )}
          </div>
        </div>
      )}

      {prompt?.kind === "newCollection" && (
        <PromptModal
          title="New collection"
          label="Name"
          onClose={() => setPrompt(null)}
          onConfirm={(name) => {
            setPrompt(null);
            void handleNewCollection(name);
          }}
        />
      )}
      {prompt?.kind === "newFolder" && (
        <PromptModal
          title="New folder"
          label="Name"
          onClose={() => setPrompt(null)}
          onConfirm={(name) => {
            const parentRef = prompt.parentRef;
            setPrompt(null);
            void handleNewFolder(parentRef, name);
          }}
        />
      )}
      {prompt?.kind === "newRequest" && (
        <PromptModal
          title="New request"
          label="Name"
          onClose={() => setPrompt(null)}
          onConfirm={(name) => {
            const parentRef = prompt.parentRef;
            setPrompt(null);
            void handleNewRequest(parentRef, name);
          }}
        />
      )}
      {prompt?.kind === "newWsRequest" && (
        <PromptModal
          title="New WebSocket request"
          label="Name"
          onClose={() => setPrompt(null)}
          onConfirm={(name) => {
            const parentRef = prompt.parentRef;
            setPrompt(null);
            void handleNewWsRequest(parentRef, name);
          }}
        />
      )}
      {prompt?.kind === "newGrpcRequest" && (
        <PromptModal
          title="New gRPC request"
          label="Name"
          onClose={() => setPrompt(null)}
          onConfirm={(name) => {
            const parentRef = prompt.parentRef;
            setPrompt(null);
            void handleNewGrpcRequest(parentRef, name);
          }}
        />
      )}
      {prompt?.kind === "rename" && (
        <PromptModal
          title="Rename"
          label="Name"
          initial={prompt.node.name}
          confirmLabel="Rename"
          onClose={() => setPrompt(null)}
          onConfirm={(name) => {
            const node = prompt.node;
            setPrompt(null);
            void handleRename(node, name);
          }}
        />
      )}
      {prompt?.kind === "promote" && (
        <PromptModal
          title="Promote to load test"
          label="Scenario name"
          initial={prompt.node.name}
          confirmLabel="Promote"
          onClose={() => setPrompt(null)}
          onConfirm={(name) => {
            const node = prompt.node;
            setPrompt(null);
            void handlePromote(node, name);
          }}
        />
      )}

      {confirm && (
        <ConfirmModal
          title={`Delete "${confirm.node.name}"?`}
          message={
            confirm.node.kind === "request"
              ? "This request will be permanently deleted."
              : "This will permanently delete everything inside it."
          }
          onClose={() => setConfirm(null)}
          onConfirm={() => void handleDelete(confirm.node)}
        />
      )}

      {showEnvModal && <EnvironmentModal onClose={() => setShowEnvModal(false)} />}

      {curlTarget && (
        <CurlImportModal
          parentRef={curlTarget.ref}
          parentName={curlTarget.name}
          onClose={() => setCurlTarget(null)}
          onImported={(nodeRef) => {
            void useWorkspace.getState().refreshTree();
            void open_(nodeRef);
            notify("Request imported");
          }}
        />
      )}

      {importReport && (
        <Modal title="Import complete" onClose={() => setImportReport(null)} wide>
          <div className="col selectable">
            <div>
              Imported <strong>{importReport.collectionName}</strong>:{" "}
              {importReport.requestsImported} request(s), {importReport.foldersImported}{" "}
              folder(s).
            </div>
            {importReport.environmentCreated && (
              <div>Environment created: {importReport.environmentCreated}</div>
            )}
            {importReport.warnings.length > 0 && (
              <div className="col">
                <div className="muted">Warnings ({importReport.warnings.length}):</div>
                {importReport.warnings.map((w, i) => (
                  <div key={i} className="mono faint">
                    {w}
                  </div>
                ))}
              </div>
            )}
          </div>
        </Modal>
      )}
    </aside>
  );
}

function findByRef(nodes: TreeNode[], ref: string): TreeNode | null {
  for (const n of nodes) {
    if (n.nodeRef === ref) return n;
    const found = findByRef(n.children, ref);
    if (found) return found;
  }
  return null;
}

function MenuItem({
  label,
  onClick,
  danger,
}: {
  label: string;
  onClick: () => void;
  danger?: boolean;
}) {
  return (
    <button
      className="btn ghost sm"
      style={{
        justifyContent: "flex-start",
        width: "100%",
        color: danger ? "var(--danger)" : undefined,
      }}
      onClick={onClick}
    >
      {label}
    </button>
  );
}

function iconFor(kind: NodeKind) {
  if (kind === "collection") return Icons.Collection;
  if (kind === "folder") return Icons.Folder;
  return null;
}

function TreeRow({
  node,
  depth,
  expanded,
  onToggle,
  onOpen,
  onContextMenu,
  dragRef,
  dropTarget,
  onDragStart,
  onDragOverNode,
  onDropNode,
}: {
  node: TreeNode;
  depth: number;
  expanded: Set<string>;
  onToggle: (ref: string) => void;
  onOpen: (ref: string) => void;
  onContextMenu: (e: React.MouseEvent, n: TreeNode) => void;
  dragRef: string | null;
  dropTarget: string | null;
  onDragStart: (ref: string) => void;
  onDragOverNode: (ref: string | null) => void;
  onDropNode: (targetRef: string) => void;
}) {
  const isContainer = node.kind !== "request";
  const isOpen = expanded.has(node.nodeRef);
  const Icon = iconFor(node.kind);
  const isDropTarget = isContainer && dropTarget === node.nodeRef && dragRef !== node.nodeRef;

  return (
    <div>
      <div
        className={`tree-row ${isDropTarget ? "drop-target" : ""}`}
        style={{ paddingLeft: 6 + depth * 14 }}
        onClick={() => (isContainer ? onToggle(node.nodeRef) : onOpen(node.nodeRef))}
        onContextMenu={(e) => onContextMenu(e, node)}
        draggable={node.kind === "request"}
        onDragStart={() => onDragStart(node.nodeRef)}
        onDragOver={(e) => {
          if (!isContainer) return;
          e.preventDefault();
          onDragOverNode(node.nodeRef);
        }}
        onDragLeave={() => {
          if (isContainer) onDragOverNode(null);
        }}
        onDrop={(e) => {
          if (!isContainer) return;
          e.preventDefault();
          onDropNode(node.nodeRef);
        }}
      >
        <span className="tree-caret" style={{ transform: isOpen ? "rotate(90deg)" : undefined }}>
          {isContainer && <Icons.Chevron />}
        </span>
        {node.kind === "request" ? (
          <MethodLabel method={node.method ?? "GET"} />
        ) : (
          Icon && <Icon className="faint" />
        )}
        <span className="tree-name">{node.name}</span>
      </div>
      {isContainer && isOpen && (
        <div>
          {node.children.map((child) => (
            <TreeRow
              key={child.nodeRef}
              node={child}
              depth={depth + 1}
              expanded={expanded}
              onToggle={onToggle}
              onOpen={onOpen}
              onContextMenu={onContextMenu}
              dragRef={dragRef}
              dropTarget={dropTarget}
              onDragStart={onDragStart}
              onDragOverNode={onDragOverNode}
              onDropNode={onDropNode}
            />
          ))}
        </div>
      )}
    </div>
  );
}
