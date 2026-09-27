import { useEffect, useMemo, useRef, useState } from "react";
import type { TreeNode } from "../../api";
import { useWorkspace } from "../../stores/workspace";
import { isDirty, isGrpcTab, isWsTab, useTabs } from "../../stores/tabs";
import { Icons, MethodLabel, Modal } from "../../components/ui";
import { CollectionSidebar } from "./CollectionSidebar";
import { WsRequestEditor } from "./WsRequestEditor";
import { GrpcRequestEditor } from "./GrpcRequestEditor";
import { RequestEditor } from "./RequestEditor";

interface QuickOpenEntry {
  nodeRef: string;
  method: string;
  path: string;
}

/** Subsequence fuzzy match: every character of `query` must appear in order in `text`. */
function fuzzyMatch(query: string, text: string): boolean {
  if (!query) return true;
  const q = query.toLowerCase();
  const t = text.toLowerCase();
  let qi = 0;
  for (let ti = 0; ti < t.length && qi < q.length; ti++) {
    if (t[ti] === q[qi]) qi++;
  }
  return qi === q.length;
}

function collectRequests(tree: TreeNode[]): QuickOpenEntry[] {
  const out: QuickOpenEntry[] = [];
  const walk = (nodes: TreeNode[], trail: string[]) => {
    for (const n of nodes) {
      if (n.kind === "request") {
        out.push({ nodeRef: n.nodeRef, method: n.method ?? "GET", path: [...trail, n.name].join(" / ") });
      } else {
        walk(n.children, [...trail, n.name]);
      }
    }
  };
  walk(tree, []);
  return out;
}

export function ClientView({
  onNavigate: _onNavigate,
}: {
  onNavigate: (s: "client" | "load" | "runs" | "settings") => void;
}) {
  const { tabs, activeRef, activate, close } = useTabs();

  // Ten minutes of edits should not vanish on a reflexive Ctrl+W.
  // Read tabs from the store, not this render: the Ctrl+W handler below is
  // registered once per active tab and would otherwise see stale edits.
  const closeTab = (nodeRef: string) => {
    const tab = useTabs.getState().tabs.find((t) => t.nodeRef === nodeRef);
    if (tab && isDirty(tab)) {
      const ok = window.confirm(`"${tab.name}" has unsaved changes. Close it and discard them?`);
      if (!ok) return;
    }
    close(nodeRef);
  };
  const tree = useWorkspace((s) => s.tree);
  const activeTab = tabs.find((t) => t.nodeRef === activeRef) ?? null;

  const [quickOpen, setQuickOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [selIndex, setSelIndex] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);

  const allRequests = useMemo(() => collectRequests(tree), [tree]);
  const filtered = useMemo(
    () => allRequests.filter((r) => fuzzyMatch(query, r.path)).slice(0, 50),
    [allRequests, query],
  );

  useEffect(() => setSelIndex(0), [query]);

  useEffect(() => {
    if (quickOpen) setTimeout(() => inputRef.current?.focus(), 0);
  }, [quickOpen]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = e.ctrlKey || e.metaKey;
      if (!mod) return;
      if (e.key.toLowerCase() === "s") {
        e.preventDefault();
        if (activeRef) void useTabs.getState().save(activeRef);
      } else if (e.key === "Enter") {
        e.preventDefault();
        if (activeRef) void useTabs.getState().send(activeRef);
      } else if (e.key.toLowerCase() === "p") {
        e.preventDefault();
        setQuery("");
        setQuickOpen(true);
      } else if (e.key.toLowerCase() === "w") {
        e.preventDefault();
        if (activeRef) closeTab(activeRef);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [activeRef, close]);

  return (
    <>
      <CollectionSidebar />
      <div className="main">
        <div className="tabstrip">
          {tabs.map((t) => (
            <div
              key={t.nodeRef}
              className={`tab ${t.nodeRef === activeRef ? "active" : ""}`}
              onClick={() => activate(t.nodeRef)}
            >
              <MethodLabel method={isGrpcTab(t) ? "GRPC" : isWsTab(t) ? "WS" : t.def.method} />
              <span className="tab-label">{t.name}</span>
              {isDirty(t) && <span className="tab-dirty" />}
              <button
                className="tab-close"
                title="Close"
                onClick={(e) => {
                  e.stopPropagation();
                  closeTab(t.nodeRef);
                }}
              >
                <Icons.Close />
              </button>
            </div>
          ))}
        </div>

        {activeTab ? (
          isGrpcTab(activeTab) ? (
            <GrpcRequestEditor key={activeTab.def.id} tab={activeTab} />
          ) : isWsTab(activeTab) ? (
            <WsRequestEditor key={activeTab.def.id} tab={activeTab} />
          ) : (
            <RequestEditor key={activeTab.def.id} tab={activeTab} />
          )
        ) : (
          <div className="empty">
            <h2>No request open</h2>
            <p>Pick a request from the sidebar, or press Ctrl/Cmd+P to search.</p>
          </div>
        )}
      </div>

      {quickOpen && (
        <Modal title="Go to request" onClose={() => setQuickOpen(false)}>
          <div className="col">
            <input
              ref={inputRef}
              className="input"
              placeholder="Type to filter…"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "ArrowDown") {
                  e.preventDefault();
                  setSelIndex((i) => Math.min(i + 1, filtered.length - 1));
                } else if (e.key === "ArrowUp") {
                  e.preventDefault();
                  setSelIndex((i) => Math.max(i - 1, 0));
                } else if (e.key === "Enter") {
                  e.preventDefault();
                  const entry = filtered[selIndex];
                  if (entry) {
                    void useTabs.getState().open(entry.nodeRef);
                    setQuickOpen(false);
                  }
                }
              }}
            />
            <div className="col" style={{ maxHeight: 320, overflow: "auto" }}>
              {filtered.map((entry, i) => (
                <div
                  key={entry.nodeRef}
                  className={`tree-row ${i === selIndex ? "selected" : ""}`}
                  onClick={() => {
                    void useTabs.getState().open(entry.nodeRef);
                    setQuickOpen(false);
                  }}
                >
                  <MethodLabel method={entry.method} />
                  <span className="tree-name">{entry.path}</span>
                </div>
              ))}
              {!filtered.length && <div className="faint pad">No matching requests.</div>}
            </div>
          </div>
        </Modal>
      )}
    </>
  );
}
