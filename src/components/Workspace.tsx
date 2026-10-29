import clsx from "clsx";
import { BookOpen, Command, FileCode2, Info, Plus, X, Zap } from "lucide-react";
import { FLAVORS, flavorOf } from "../lib/engines";
import { useStore, type Tab } from "../store";
import { DataGrid } from "./DataGrid";
import { DocsView } from "./DocsView";
import { QueryTab } from "./QueryTab";
import { EngineBadge, Kbd, showContextMenu, Spinner } from "./ui";
import { newConnection } from "../store";

export function Workspace() {
  const tabs = useStore((s) => s.tabs);
  const activeTabId = useStore((s) => s.activeTabId);
  const active = tabs.find((t) => t.id === activeTabId);

  return (
    <div className="flex h-full flex-col bg-bg">
      {tabs.length > 0 && <TabBar tabs={tabs} activeId={activeTabId} />}
      <div className="min-h-0 flex-1">
        {active ? (
          // keep each tab's editor state by keying on id
          active.kind === "query" ? (
            <QueryTab key={active.id} tab={active} />
          ) : active.kind === "docs" ? (
            <DocsView key={active.id} tab={active} />
          ) : (
            <StructureView key={active.id} tab={active} />
          )
        ) : (
          <Welcome />
        )}
      </div>
    </div>
  );
}

function TabBar({ tabs, activeId }: { tabs: Tab[]; activeId: string | null }) {
  const connections = useStore((s) => s.connections);
  const { setActive, closeTab, closeOtherTabs, openQueryTab } = useStore.getState();
  const activeConn = tabs.find((t) => t.id === activeId)?.connId;

  return (
    <div className="flex h-10 shrink-0 items-end gap-0.5 overflow-x-auto border-b border-line bg-panel px-1.5 pt-1.5">
      {tabs.map((t) => {
        const conn = connections.find((c) => c.id === t.connId);
        const active = t.id === activeId;
        return (
          <div
            key={t.id}
            onMouseDown={(e) => {
              if (e.button === 1) {
                e.preventDefault();
                closeTab(t.id);
              } else setActive(t.id);
            }}
            onContextMenu={(e) =>
              showContextMenu(e, [
                { label: "Close", shortcut: "Ctrl W", onSelect: () => closeTab(t.id) },
                { label: "Close others", onSelect: () => closeOtherTabs(t.id) },
              ])
            }
            className={clsx(
              "group relative flex h-full max-w-[220px] min-w-[120px] shrink-0 items-center gap-2 rounded-t-lg px-3 text-[12.5px] transition-colors",
              active ? "bg-bg text-fg" : "text-muted hover:bg-hover hover:text-fg",
            )}
          >
            {active && (
              <span
                className="absolute inset-x-2 top-0 h-[2px] rounded-full"
                style={{ background: conn?.color || "linear-gradient(90deg, var(--accent), var(--accent-2))" }}
              />
            )}
            {t.kind === "structure" ? (
              <Info className="h-3.5 w-3.5 shrink-0 text-accent-2" />
            ) : t.kind === "docs" ? (
              <BookOpen className="h-3.5 w-3.5 shrink-0 text-accent" />
            ) : conn ? (
              <EngineBadge engine={conn.engine} flavor={conn.flavor} size={15} />
            ) : (
              <FileCode2 className="h-3.5 w-3.5" />
            )}
            <span className="flex-1 truncate">{t.title}</span>
            {t.running ? (
              <Spinner className="h-3 w-3 text-accent" />
            ) : (
              <button
                onMouseDown={(e) => e.stopPropagation()}
                onClick={() => closeTab(t.id)}
                className={clsx("grid h-4 w-4 place-items-center rounded hover:bg-hover", !active && "opacity-0 group-hover:opacity-100")}
              >
                <X className="h-3 w-3" />
              </button>
            )}
          </div>
        );
      })}
      {activeConn && (
        <button
          onClick={() => openQueryTab(activeConn)}
          title="New query (Ctrl+T)"
          className="mb-1 ml-1 grid h-7 w-7 shrink-0 place-items-center rounded-md text-muted hover:bg-hover hover:text-fg"
        >
          <Plus className="h-4 w-4" />
        </button>
      )}
    </div>
  );
}

function StructureView({ tab }: { tab: Tab }) {
  const conn = useStore((s) => s.connections.find((c) => c.id === tab.connId));
  return (
    <div className="flex h-full flex-col">
      <div className="flex h-14 shrink-0 items-center gap-3 border-b border-line bg-panel px-4">
        {conn && <EngineBadge engine={conn.engine} flavor={conn.flavor} size={28} />}
        <div className="leading-tight">
          <div className="text-[14px] font-semibold">{tab.title}</div>
          <div className="font-mono text-[11px] text-faint">{tab.path?.join(" › ")}</div>
        </div>
        {tab.structure?.message && <span className="ml-auto text-[12px] text-faint">{tab.structure.message}</span>}
      </div>
      <div className="min-h-0 flex-1 bg-panel">
        {tab.running ? (
          <div className="grid h-full place-items-center text-muted">
            <Spinner className="h-5 w-5 text-accent" />
          </div>
        ) : tab.error ? (
          <div className="m-4 rounded-xl border border-danger/25 bg-danger/[0.06] p-4 font-mono text-[12px] text-danger">{tab.error}</div>
        ) : tab.structure ? (
          <DataGrid set={tab.structure} filter="" />
        ) : null}
      </div>
    </div>
  );
}

function Welcome() {
  const connections = useStore((s) => s.connections);
  const live = useStore((s) => s.live);
  const history = useStore((s) => s.history);
  const { openDialog, openQueryTab, setPalette } = useStore.getState();
  const featured = FLAVORS.slice(0, 12);

  return (
    <div className="glow-bg relative h-full overflow-auto">
      <div className="grid-dots pointer-events-none absolute inset-0 opacity-60" />
      <div className="relative mx-auto max-w-[980px] px-10 pt-16 pb-12">
        <div className="mb-10 flex items-center gap-4">
          <img src="/logo.svg" className="h-14 w-14 rounded-2xl shadow-[0_10px_40px_-10px_var(--accent)]" />
          <div>
            <h1 className="text-[30px] leading-tight font-semibold tracking-tight">
              One client for <span className="gradient-text">every database</span>
            </h1>
            <p className="mt-1 text-[14px] text-muted">
              SQL, documents, graphs, key-value and search — explore and query them all from one place.
            </p>
          </div>
        </div>

        <div className="mb-10 flex flex-wrap gap-2.5">
          <button onClick={() => openDialog()} className="btn-primary flex items-center gap-2 rounded-xl px-4 py-2.5 text-[13px] font-semibold text-white">
            <Plus className="h-4 w-4" /> New connection
          </button>
          <button
            onClick={() => setPalette(true)}
            className="flex items-center gap-2 rounded-xl border border-line-strong bg-panel/70 px-4 py-2.5 text-[13px] font-medium backdrop-blur hover:bg-hover"
          >
            <Command className="h-4 w-4 text-muted" /> Command palette <Kbd>Ctrl K</Kbd>
          </button>
        </div>

        {connections.length > 0 && (
          <section className="mb-10">
            <h2 className="mb-3 text-[11px] font-semibold tracking-[0.08em] text-faint uppercase">Jump back in</h2>
            <div className="grid grid-cols-[repeat(auto-fill,minmax(220px,1fr))] gap-2.5">
              {connections.slice(0, 6).map((c) => {
                const f = flavorOf(c.engine, c.flavor);
                return (
                  <button
                    key={c.id}
                    onClick={() => openQueryTab(c.id)}
                    className="group flex items-center gap-3 rounded-xl border border-line bg-panel/80 p-3 text-left backdrop-blur transition-all hover:-translate-y-0.5 hover:border-line-strong hover:shadow-pop"
                  >
                    <EngineBadge engine={c.engine} flavor={c.flavor} size={34} />
                    <div className="min-w-0 flex-1">
                      <div className="truncate text-[13px] font-medium">{c.name || f.name}</div>
                      <div className="truncate text-[11.5px] text-faint">{live[c.id]?.version ?? f.name}</div>
                    </div>
                    <Zap className="h-4 w-4 text-faint opacity-0 transition-opacity group-hover:text-accent group-hover:opacity-100" />
                  </button>
                );
              })}
            </div>
          </section>
        )}

        <section className="mb-10">
          <h2 className="mb-3 text-[11px] font-semibold tracking-[0.08em] text-faint uppercase">Supported engines</h2>
          <div className="grid grid-cols-[repeat(auto-fill,minmax(150px,1fr))] gap-2">
            {featured.map((f) => (
              <button
                key={f.id}
                onClick={() => openDialog(newConnection(f.id))}
                className="flex items-center gap-2.5 rounded-xl border border-line bg-panel/60 px-3 py-2.5 text-left backdrop-blur transition-colors hover:border-line-strong hover:bg-panel"
              >
                <EngineBadge engine={f.engine} flavor={f.id} size={26} />
                <div className="min-w-0">
                  <div className="truncate text-[12.5px] font-medium">{f.name}</div>
                  <div className="text-[10.5px] text-faint">{f.category}</div>
                </div>
              </button>
            ))}
            <button
              onClick={() => openDialog()}
              className="flex items-center justify-center rounded-xl border border-dashed border-line-strong px-3 py-2.5 text-[12px] text-muted hover:text-fg"
            >
              +{FLAVORS.length - featured.length} more
            </button>
          </div>
        </section>

        {history.length > 0 && (
          <section>
            <h2 className="mb-3 text-[11px] font-semibold tracking-[0.08em] text-faint uppercase">Recent queries</h2>
            <div className="overflow-hidden rounded-xl border border-line bg-panel/70 backdrop-blur">
              {history.slice(0, 6).map((h) => {
                const c = connections.find((x) => x.id === h.connId);
                if (!c) return null;
                return (
                  <button
                    key={h.id}
                    onClick={() => openQueryTab(c.id, { query: h.query })}
                    className="flex w-full items-center gap-3 border-b border-line px-3 py-2 text-left last:border-0 hover:bg-hover"
                  >
                    <span className={clsx("h-1.5 w-1.5 shrink-0 rounded-full", h.ok ? "bg-success" : "bg-danger")} />
                    <EngineBadge engine={c.engine} flavor={c.flavor} size={16} />
                    <span className="flex-1 truncate font-mono text-[12px] text-muted">{h.query.replace(/\s+/g, " ")}</span>
                    <span className="shrink-0 text-[11px] text-faint">{h.ms != null ? `${h.ms} ms` : ""}</span>
                  </button>
                );
              })}
            </div>
          </section>
        )}
      </div>
    </div>
  );
}
