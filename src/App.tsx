import clsx from "clsx";
import { Command, Moon, PanelLeft, Plus, Sun } from "lucide-react";
import { useEffect, useState } from "react";
import { Group, Panel, Separator } from "react-resizable-panels";
import { Toaster } from "sonner";
import { CommandPalette } from "./components/CommandPalette";
import { ConnectionDialog } from "./components/ConnectionDialog";
import { DumpDialog } from "./components/DumpDialog";
import { EntraLogin } from "./components/EntraLogin";
import { ImportDialog } from "./components/ImportDialog";
import { Sidebar } from "./components/Sidebar";
import { ContextMenuHost, EngineBadge, IconButton, Kbd } from "./components/ui";
import { Workspace } from "./components/Workspace";
import { isTauri } from "./lib/api";
import { useStore } from "./store";

export default function App() {
  const theme = useStore((s) => s.theme);
  const [sidebar, setSidebar] = useState(true);
  const { loadConnections, setPalette, openDialog } = useStore.getState();

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
  }, [theme]);

  useEffect(() => {
    void loadConnections();
    const onKey = (e: KeyboardEvent) => {
      const mod = e.ctrlKey || e.metaKey;
      if (!mod) return;
      const s = useStore.getState();
      const active = s.tabs.find((t) => t.id === s.activeTabId);
      if (e.key === "k") {
        e.preventDefault();
        setPalette(!s.paletteOpen);
      } else if (e.key === "t") {
        e.preventDefault();
        const connId = active?.connId ?? s.connections[0]?.id;
        if (connId) s.openQueryTab(connId);
        else openDialog();
      } else if (e.key === "w" && active) {
        e.preventDefault();
        s.closeTab(active.id);
      } else if (e.key === "b") {
        e.preventDefault();
        setSidebar((v) => !v);
      } else if (e.key === "Tab" && s.tabs.length > 1) {
        e.preventDefault();
        const i = s.tabs.findIndex((t) => t.id === s.activeTabId);
        const next = (i + (e.shiftKey ? -1 : 1) + s.tabs.length) % s.tabs.length;
        s.setActive(s.tabs[next].id);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <div className="flex h-full flex-col" onContextMenu={(e) => e.preventDefault()}>
      <TopBar sidebar={sidebar} onToggleSidebar={() => setSidebar((v) => !v)} />
      <div className="min-h-0 flex-1">
        <Group orientation="horizontal">
          {sidebar && (
            <>
              <Panel defaultSize="290px" minSize="220px" maxSize="520px" className="border-r border-line">
                <Sidebar />
              </Panel>
              <Separator className="resize-handle w-px" />
            </>
          )}
          <Panel minSize="40">
            <Workspace />
          </Panel>
        </Group>
      </div>
      <StatusBar />
      <ConnectionDialog />
      <ImportDialog />
      <DumpDialog />
      <EntraLogin />
      <CommandPalette />
      <ContextMenuHost />
      <Toaster
        theme={theme}
        position="bottom-right"
        toastOptions={{ className: "!rounded-xl !border !border-line !bg-elev !text-fg !shadow-pop", style: { fontFamily: "var(--font-sans)" } }}
      />
    </div>
  );
}

function TopBar({ sidebar, onToggleSidebar }: { sidebar: boolean; onToggleSidebar: () => void }) {
  const theme = useStore((s) => s.theme);
  const { setPalette, setTheme, openDialog } = useStore.getState();
  return (
    <div className="relative flex h-11 shrink-0 items-center gap-2 border-b border-line bg-panel px-2.5">
      <IconButton title="Toggle sidebar (Ctrl+B)" onClick={onToggleSidebar} active={sidebar}>
        <PanelLeft className="h-4 w-4" />
      </IconButton>
      <div className="flex items-center gap-2 pl-1">
        <img src="/logo.svg" className="h-[22px] w-[22px] rounded-md" />
        <span className="text-[13.5px] font-semibold tracking-tight">
          Nexus<span className="gradient-text">DB</span>
        </span>
        {!isTauri && <span className="rounded-md bg-warn/12 px-1.5 py-0.5 text-[10.5px] font-medium text-warn">browser demo</span>}
      </div>
      <button
        onClick={() => setPalette(true)}
        className="absolute left-1/2 flex h-7 w-[min(440px,40vw)] -translate-x-1/2 items-center gap-2 rounded-lg border border-line bg-panel-2 px-2.5 text-[12.5px] text-faint transition-colors hover:border-line-strong hover:text-muted"
      >
        <Command className="h-3.5 w-3.5" />
        Search tables, collections, actions…
        <span className="ml-auto">
          <Kbd>Ctrl K</Kbd>
        </span>
      </button>
      <div className="ml-auto flex items-center gap-1">
        <button onClick={() => openDialog()} className="flex h-7 items-center gap-1.5 rounded-md px-2.5 text-[12.5px] text-muted hover:bg-hover hover:text-fg">
          <Plus className="h-3.5 w-3.5" /> Connection
        </button>
        <IconButton title="Toggle theme" onClick={() => setTheme(theme === "dark" ? "light" : "dark")}>
          {theme === "dark" ? <Sun className="h-4 w-4" /> : <Moon className="h-4 w-4" />}
        </IconButton>
      </div>
    </div>
  );
}

function StatusBar() {
  const tab = useStore((s) => s.tabs.find((t) => t.id === s.activeTabId));
  const conn = useStore((s) => s.connections.find((c) => c.id === tab?.connId));
  const live = useStore((s) => (tab ? s.live[tab.connId] : undefined));
  const liveCount = useStore((s) => Object.values(s.live).filter((l) => l.status === "connected").length);

  return (
    <div className="flex h-6 shrink-0 items-center gap-3 border-t border-line bg-panel px-3 text-[11px] text-faint">
      <span className="flex items-center gap-1.5">
        <span className={clsx("h-1.5 w-1.5 rounded-full", liveCount ? "bg-success" : "bg-line-strong")} />
        {liveCount} active connection{liveCount === 1 ? "" : "s"}
      </span>
      {conn && (
        <>
          <span className="h-3 w-px bg-line" />
          <span className="flex items-center gap-1.5">
            <EngineBadge engine={conn.engine} flavor={conn.flavor} size={13} />
            <span className="text-muted">{conn.name}</span>
            {live?.version && <span>· {live.version}</span>}
          </span>
          {tab?.database && (
            <span>
              db <span className="text-muted">{tab.database}</span>
              {tab.schema && (
                <>
                  {" "}
                  · schema <span className="text-muted">{tab.schema}</span>
                </>
              )}
            </span>
          )}
        </>
      )}
      <span className="ml-auto">
        <Kbd>Ctrl T</Kbd> new query · <Kbd>Ctrl ↵</Kbd> run
      </span>
    </div>
  );
}
