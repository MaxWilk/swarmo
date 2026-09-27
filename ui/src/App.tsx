import { useEffect, useRef, useState } from "react";
import { Icons, Toasts } from "./components/ui";
import { useWorkspace } from "./stores/workspace";
import { useLoad } from "./stores/load";
import { useTabs } from "./stores/tabs";
import { WelcomeView } from "./views/WelcomeView";
import { SettingsView } from "./views/SettingsView";
import { ClientView } from "./views/client/ClientView";
import { LoadView } from "./views/load/LoadView";
import { RunsView } from "./views/runs/RunsView";
import { HistoryView } from "./views/history/HistoryView";

export type Section = "client" | "history" | "load" | "runs" | "settings";

const SECTIONS: { id: Section; label: string; Icon: (p: { className?: string }) => JSX.Element }[] = [
  { id: "client", label: "Client", Icon: Icons.Send },
  { id: "history", label: "History", Icon: Icons.History },
  { id: "load", label: "Load", Icon: Icons.Gauge },
  { id: "runs", label: "Runs", Icon: Icons.Chart },
  { id: "settings", label: "Settings", Icon: Icons.Settings },
];

export function App() {
  const [section, setSection] = useState<Section>("client");
  const { info, booted, boot } = useWorkspace();
  const startListening = useLoad((s) => s.startListening);
  const liveRun = useLoad((s) => s.live);
  const previousWorkspace = useRef<string | null | undefined>(undefined);

  useEffect(() => {
    void boot();
    void startListening();
  }, [boot, startListening]);

  useEffect(() => {
    const path = info?.path ?? null;
    if (
      previousWorkspace.current !== undefined &&
      previousWorkspace.current !== path
    ) {
      useTabs.getState().closeAll();
      useLoad.getState().resetWorkspace();
    }
    previousWorkspace.current = path;
  }, [info?.path]);

  if (!booted) {
    return (
      <div className="empty">
        <div className="spinner" />
      </div>
    );
  }

  return (
    <div className="app">
      <nav className="rail">
        {SECTIONS.map(({ id, label, Icon }) => (
          <button
            key={id}
            className={`rail-btn ${section === id ? "active" : ""}`}
            onClick={() => setSection(id)}
            title={label}
            disabled={!info && id !== "settings"}
          >
            <Icon />
            <span>{label}</span>
            {/* A run keeps going while you work elsewhere; without this the
                only sign of it is on a screen you are not looking at. */}
            {id === "runs" && liveRun && <span className="rail-dot" title="A run is in progress" />}
          </button>
        ))}
        <div className="rail-spacer" />
      </nav>

      {!info && section !== "settings" ? (
        <WelcomeView />
      ) : section === "client" ? (
        <ClientView onNavigate={setSection} />
      ) : section === "history" ? (
        <HistoryView onNavigate={setSection} />
      ) : section === "load" ? (
        <LoadView onNavigate={setSection} />
      ) : section === "runs" ? (
        <RunsView />
      ) : (
        <SettingsView />
      )}

      <Toasts />
    </div>
  );
}
