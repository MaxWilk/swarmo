import { useEffect, useState } from "react";
import * as api from "../../api";
import type { LoadTestEntry } from "../../api";
import { Icons, PromptModal, ConfirmModal, EmptyState } from "../../components/ui";
import { useLoad } from "../../stores/load";
import { reportError, notify } from "../../stores/toast";
import { ScenarioEditor } from "./ScenarioEditor";
import { UserScriptEditor } from "./UserScriptEditor";
import { useRunLauncher } from "./runLoadTest";

type Section = "client" | "load" | "runs" | "settings";

export function LoadView({ onNavigate }: { onNavigate: (s: Section) => void }) {
  const { tests, testTree, refreshTests } = useLoad();
  const [selected, setSelected] = useState<string | null>(null);
  const [newScenarioOpen, setNewScenarioOpen] = useState(false);
  const [newScriptOpen, setNewScriptOpen] = useState(false);
  const [newFolderOpen, setNewFolderOpen] = useState(false);
  // Which folder the next create lands in; empty means the root.
  const [createIn, setCreateIn] = useState("");
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [dragOver, setDragOver] = useState<string | null>(null);

  const toggleFolder = (ref: string) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(ref)) next.delete(ref);
      else next.add(ref);
      return next;
    });

  const doCreateFolder = async (name: string) => {
    try {
      await api.loadFolderCreate(createIn, name);
      setNewFolderOpen(false);
      await refreshTests();
      // Opened straight away: a folder you just made and cannot see is
      // indistinguishable from one that was not created.
      if (createIn) setExpanded((p) => new Set(p).add(createIn));
    } catch (e) {
      reportError("Could not create the folder", e);
    }
  };

  // Whether the open editor holds edits that are not on disk. The editors
  // report it, so an operation that would remount or close one can ask first.
  const [editorDirty, setEditorDirty] = useState(false);

  /** False when the user declines to lose the open editor's unsaved edits. */
  const confirmDiscard = () =>
    !editorDirty || window.confirm("The open load test has unsaved changes. Discard them?");

  /** Whether `ref` is the open test, or a folder that contains it. */
  const coversSelected = (ref: string) =>
    selected !== null && (selected === ref || selected.startsWith(`${ref}/`));

  /**
   * Point the selection at where a rename or move put it. Refs are paths,
   * so a test inside a moved folder moves with it.
   */
  const followSelected = (from: string, to: string) => {
    if (selected !== null && coversSelected(from)) {
      setSelected(to + selected.slice(from.length));
    }
  };

  const doMove = async (nodeRef: string, parentRef: string) => {
    if (coversSelected(nodeRef) && !confirmDiscard()) return;
    try {
      const moved = await api.loadTestMove(nodeRef, parentRef);
      // Before the refresh, so the selection never names a path that no
      // longer exists.
      followSelected(nodeRef, moved);
      await refreshTests();
      if (parentRef) setExpanded((p) => new Set(p).add(parentRef));
    } catch (e) {
      reportError("Could not move it", e);
    }
  };
  const [pendingDelete, setPendingDelete] = useState<LoadTestEntry | null>(null);
  const [pendingRename, setPendingRename] = useState<LoadTestEntry | null>(null);
  const [ctxMenu, setCtxMenu] = useState<{ x: number; y: number; entry: LoadTestEntry } | null>(
    null,
  );

  // Same dismissal rules as the collection tree: any click elsewhere, or
  // Escape, closes it.
  useEffect(() => {
    if (!ctxMenu) return;
    const close = () => setCtxMenu(null);
    const onEscape = (e: KeyboardEvent) => {
      if (e.key === "Escape") setCtxMenu(null);
    };
    window.addEventListener("mousedown", close);
    window.addEventListener("keydown", onEscape);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("keydown", onEscape);
    };
  }, [ctxMenu]);
  const { launch, dialog } = useRunLauncher(() => onNavigate("runs"));
  const setRunScope = useLoad((s) => s.setRunScope);

  /**
   * Open the Runs list showing only this test's history.
   *
   * Scoped by the scenario's id, so it keeps working after a rename or a
   * move — which is why runs stay a flat, time-ordered list rather than
   * living inside the load-test tree.
   */
  const showRuns = (entry: LoadTestEntry) => {
    if (!entry.id) return;
    setRunScope({ scenarioId: entry.id, name: entry.name });
    onNavigate("runs");
  };

  useEffect(() => {
    void refreshTests();
  }, [refreshTests]);


  const createScenario = async (name: string) => {
    try {
      const ref = await api.scenarioCreate(name, createIn || undefined);
      setNewScenarioOpen(false);
      await refreshTests();
      setSelected(ref);
    } catch (e) {
      reportError("Could not create scenario", e);
    }
  };

  const createScript = async (name: string) => {
    try {
      const ref = await api.userScriptCreate(name, createIn || undefined);
      setNewScriptOpen(false);
      await refreshTests();
      setSelected(ref);
    } catch (e) {
      reportError("Could not create user script", e);
    }
  };

  const doRename = async (entry: LoadTestEntry, newName: string) => {
    // The editor is keyed by its ref, so a rename remounts it.
    if (coversSelected(entry.nodeRef) && !confirmDiscard()) return;
    try {
      // A folder is a directory, not a load-test file, so it renames
      // through its own command.
      const moved =
        entry.kind === "folder"
          ? await api.loadFolderRename(entry.nodeRef, newName)
          : await api.loadTestRename(entry.nodeRef, newName);
      // The ref follows the file, so a renamed test that was open stays open.
      followSelected(entry.nodeRef, moved);
      await refreshTests();
      notify("Renamed");
    } catch (e) {
      reportError("Could not rename this load test", e);
    }
  };

  const doDuplicate = async (entry: LoadTestEntry) => {
    try {
      const copy = await api.loadTestDuplicate(entry.nodeRef);
      await refreshTests();
      setSelected(copy);
      notify("Duplicated");
    } catch (e) {
      reportError("Could not duplicate this load test", e);
    }
  };

  const doDelete = async (entry: LoadTestEntry) => {
    try {
      await api.nodeDelete(entry.nodeRef);
      if (coversSelected(entry.nodeRef)) setSelected(null);
      await refreshTests();
      notify("Deleted");
    } catch (e) {
      reportError("Could not delete", e);
    }
  };

  const selectedEntry = tests.find((t) => t.nodeRef === selected) ?? null;

  return (
    <>
      <div className="sidebar">
        <div
          className={`panel-header ${dragOver === "" ? "drop-target" : ""}`}
          // The header doubles as the root drop target, so a test can be
          // dragged back out of a folder.
          onDragOver={(e) => {
            e.preventDefault();
            setDragOver("");
          }}
          onDragLeave={() => setDragOver(null)}
          onDrop={(e) => {
            e.preventDefault();
            setDragOver(null);
            const ref = e.dataTransfer.getData("text/plain");
            if (ref) void doMove(ref, "");
          }}
        >
          <span className="grow">Load tests</span>
          <button
            className="btn icon sm ghost"
            title="New folder"
            onClick={() => {
              setCreateIn("");
              setNewFolderOpen(true);
            }}
          >
            <Icons.Folder />
          </button>
          <button
            className="btn icon sm"
            title="New scenario"
            onClick={() => {
              setCreateIn("");
              setNewScenarioOpen(true);
            }}
          >
            <Icons.Plus />
          </button>
        </div>
        <div className="scroll">
          {testTree.length === 0 && (
            <div className="hint" style={{ padding: 10 }}>
              No load tests yet.
            </div>
          )}
          <LoadTreeRows
            nodes={testTree}
            depth={0}
            selected={selected}
            expanded={expanded}
            dragOver={dragOver}
            onToggle={toggleFolder}
            onSelect={(ref) => {
              if (ref !== selected && !confirmDiscard()) return;
              setSelected(ref);
            }}
            onDelete={setPendingDelete}
            onRename={setPendingRename}
            onContextMenu={(e, entry) => {
              e.preventDefault();
              setCtxMenu({ x: e.clientX, y: e.clientY, entry });
            }}
            onDragOverNode={setDragOver}
            onDropNode={doMove}
          />
          <div className="pad">
            <button
              className="btn sm ghost"
              onClick={() => {
                setCreateIn("");
                setNewScriptOpen(true);
              }}
            >
              <Icons.Plus /> New user script
            </button>
          </div>
        </div>
      </div>

      <div className="main">
        {/* Keyed by ref, so one file's state (and dirty flag) can never be
            saved over another while the next one loads. */}
        {selectedEntry?.kind === "scenario" ? (
          <ScenarioEditor
            key={selectedEntry.nodeRef}
            nodeRef={selectedEntry.nodeRef}
            onRun={() => launch(selectedEntry.nodeRef)}
            onDirtyChange={setEditorDirty}
          />
        ) : selectedEntry?.kind === "userScript" ? (
          <UserScriptEditor
            key={selectedEntry.nodeRef}
            nodeRef={selectedEntry.nodeRef}
            onRun={() => launch(selectedEntry.nodeRef)}
            onDirtyChange={setEditorDirty}
          />
        ) : (
          <EmptyState title="No load test selected">
            Pick a scenario or user script from the sidebar, or create a new one.
          </EmptyState>
        )}
      </div>

      {newScenarioOpen && (
        <PromptModal
          title="New scenario"
          label="Name"
          confirmLabel="Create"
          onConfirm={(v) => void createScenario(v)}
          onClose={() => setNewScenarioOpen(false)}
        />
      )}
      {newScriptOpen && (
        <PromptModal
          title="New user script"
          label="Name"
          confirmLabel="Create"
          onConfirm={(v) => void createScript(v)}
          onClose={() => setNewScriptOpen(false)}
        />
      )}
      {pendingDelete && (
        <ConfirmModal
          title="Delete"
          message={`Delete "${pendingDelete.name}"? This cannot be undone.`}
          onConfirm={() => void doDelete(pendingDelete)}
          onClose={() => setPendingDelete(null)}
        />
      )}
      {ctxMenu && (
        <div
          className="modal"
          style={{ position: "fixed", left: ctxMenu.x, top: ctxMenu.y, width: 190, zIndex: 200 }}
          onMouseDown={(e) => e.stopPropagation()}
        >
          <div className="modal-body" style={{ padding: 6, gap: 0 }}>
            {ctxMenu.entry.kind === "folder" ? (
              <>
                <LoadMenuItem
                  label="New scenario here"
                  onClick={() => {
                    setCreateIn(ctxMenu.entry.nodeRef);
                    setNewScenarioOpen(true);
                    setCtxMenu(null);
                  }}
                />
                <LoadMenuItem
                  label="New folder here"
                  onClick={() => {
                    setCreateIn(ctxMenu.entry.nodeRef);
                    setNewFolderOpen(true);
                    setCtxMenu(null);
                  }}
                />
              </>
            ) : (
              <LoadMenuItem
                label="Run"
                onClick={() => {
                  setCtxMenu(null);
                  // The run reads from disk and then leaves this view, so
                  // unsaved edits in the open editor would be lost unrun.
                  if (!confirmDiscard()) return;
                  setSelected(ctxMenu.entry.nodeRef);
                  launch(ctxMenu.entry.nodeRef);
                }}
              />
            )}
            {ctxMenu.entry.id && (
              <LoadMenuItem
                label="Show runs"
                onClick={() => {
                  showRuns(ctxMenu.entry);
                  setCtxMenu(null);
                }}
              />
            )}
            <LoadMenuItem
              label="Rename"
              onClick={() => {
                setPendingRename(ctxMenu.entry);
                setCtxMenu(null);
              }}
            />
            {ctxMenu.entry.kind !== "folder" && (
              <LoadMenuItem
                label="Duplicate"
                onClick={() => {
                  void doDuplicate(ctxMenu.entry);
                  setCtxMenu(null);
                }}
              />
            )}
            <LoadMenuItem
              label="Delete"
              danger
              onClick={() => {
                setPendingDelete(ctxMenu.entry);
                setCtxMenu(null);
              }}
            />
          </div>
        </div>
      )}

      {newFolderOpen && (
        <PromptModal
          title="New folder"
          label="Name"
          confirmLabel="Create"
          onConfirm={(v) => void doCreateFolder(v)}
          onClose={() => setNewFolderOpen(false)}
        />
      )}

      {pendingRename && (
        <PromptModal
          title="Rename"
          label="Name"
          initial={pendingRename.name}
          confirmLabel="Rename"
          onConfirm={(v) => void doRename(pendingRename, v)}
          onClose={() => setPendingRename(null)}
        />
      )}

      {dialog}
    </>
  );
}

/**
 * The load-test tree.
 *
 * Folders here are organisation only — unlike a collection they carry no
 * configuration for what is inside them — so nesting is optional and a test
 * is just as valid at the root.
 */
function LoadTreeRows({
  nodes,
  depth,
  selected,
  expanded,
  dragOver,
  onToggle,
  onSelect,
  onDelete,
  onRename,
  onContextMenu,
  onDragOverNode,
  onDropNode,
}: {
  nodes: LoadTestEntry[];
  depth: number;
  selected: string | null;
  expanded: Set<string>;
  dragOver: string | null;
  onToggle: (ref: string) => void;
  onSelect: (ref: string) => void;
  onDelete: (entry: LoadTestEntry) => void;
  onRename: (entry: LoadTestEntry) => void;
  onContextMenu: (e: React.MouseEvent, entry: LoadTestEntry) => void;
  onDragOverNode: (ref: string | null) => void;
  onDropNode: (nodeRef: string, parentRef: string) => void;
}) {
  return (
    <>
      {nodes.map((entry) => {
        const isFolder = entry.kind === "folder";
        const open = expanded.has(entry.nodeRef);
        return (
          <div key={entry.nodeRef}>
            <div
              className={`tree-row ${selected === entry.nodeRef ? "selected" : ""} ${
                dragOver === entry.nodeRef ? "drop-target" : ""
              }`}
              style={{ padding: `0 6px 0 ${10 + depth * 12}px` }}
              draggable
              onDragStart={(e) => {
                e.stopPropagation();
                e.dataTransfer.setData("text/plain", entry.nodeRef);
              }}
              onDragOver={
                isFolder
                  ? (e) => {
                      e.preventDefault();
                      e.stopPropagation();
                      onDragOverNode(entry.nodeRef);
                    }
                  : undefined
              }
              onDragLeave={isFolder ? () => onDragOverNode(null) : undefined}
              onDrop={
                isFolder
                  ? (e) => {
                      e.preventDefault();
                      e.stopPropagation();
                      onDragOverNode(null);
                      const ref = e.dataTransfer.getData("text/plain");
                      if (ref && ref !== entry.nodeRef) onDropNode(ref, entry.nodeRef);
                    }
                  : undefined
              }
              onClick={() => (isFolder ? onToggle(entry.nodeRef) : onSelect(entry.nodeRef))}
              onContextMenu={(e) => onContextMenu(e, entry)}
            >
              {isFolder && (
                <span className="tree-caret" style={{ transform: open ? "rotate(90deg)" : undefined }}>
                  <Icons.Chevron />
                </span>
              )}
              <span className="tree-name">{entry.name}</span>
              {entry.kind === "userScript" && <span className="hint nowrap">js</span>}
              <span className="tree-actions">
                <button
                  className="btn icon sm ghost"
                  title="Rename"
                  onClick={(e) => {
                    e.stopPropagation();
                    onRename(entry);
                  }}
                >
                  <Icons.Edit />
                </button>
                <button
                  className="btn icon sm ghost"
                  title="Delete"
                  onClick={(e) => {
                    e.stopPropagation();
                    onDelete(entry);
                  }}
                >
                  <Icons.Trash />
                </button>
              </span>
            </div>
            {isFolder && open && entry.children && entry.children.length > 0 && (
              <LoadTreeRows
                nodes={entry.children}
                depth={depth + 1}
                selected={selected}
                expanded={expanded}
                dragOver={dragOver}
                onToggle={onToggle}
                onSelect={onSelect}
                onDelete={onDelete}
                onRename={onRename}
                onContextMenu={onContextMenu}
                onDragOverNode={onDragOverNode}
                onDropNode={onDropNode}
              />
            )}
            {isFolder && open && (!entry.children || entry.children.length === 0) && (
              <div
                className="hint"
                style={{ padding: `2px 10px 2px ${22 + depth * 12}px` }}
              >
                Empty
              </div>
            )}
          </div>
        );
      })}
    </>
  );
}

/**
 * One line in the load-test context menu.
 *
 * Deliberately the same shape as the collection tree's menu item: the two
 * sidebars should not feel like different applications.
 */
function LoadMenuItem({
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
      style={{ justifyContent: "flex-start", color: danger ? "var(--danger)" : undefined }}
      onClick={onClick}
    >
      {label}
    </button>
  );
}
