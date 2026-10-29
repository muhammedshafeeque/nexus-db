import clsx from "clsx";
import { AlertTriangle, ArchiveRestore, CheckCircle2, DatabaseBackup, FolderOpen, Plus, Save, Wrench, X, XCircle } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { api, errorText, isTauri } from "../lib/api";
import { ENGINE_META, flavorOf } from "../lib/engines";
import type { ConnectionConfig, DumpFormat, DumpReport, Engine, RestoreReport, ToolStatus } from "../lib/types";
import { newConnection, nodeKey, useStore, type DumpMode } from "../store";
import { Spinner, Toggle } from "./ui";

const inputCls =
  "h-8 w-full rounded-lg border border-line bg-panel-2 px-2.5 text-[12.5px] text-fg outline-none transition-colors placeholder:text-faint focus:border-accent/70";

/** Engines with dump / restore support and the formats each one writes. */
const FORMATS: Partial<Record<Engine, { id: DumpFormat; label: string; ext: string; hint: string }[]>> = {
  postgres: [
    { id: "native", label: "Custom archive", ext: "dump", hint: "pg_dump -Fc – compressed, restored with pg_restore" },
    { id: "sql", label: "Plain SQL", ext: "sql", hint: "Readable script, restored with psql" },
  ],
  mysql: [{ id: "sql", label: "SQL script", ext: "sql", hint: "mysqldump – tables, data, routines and triggers" }],
  mongodb: [{ id: "native", label: "Archive (gzip)", ext: "archive.gz", hint: "mongodump --archive --gzip" }],
  sqlite: [
    { id: "native", label: "Database copy", ext: "sqlite", hint: "A consistent copy of the file (VACUUM INTO) – no tools needed" },
    { id: "sql", label: "SQL script", ext: "sql", hint: "sqlite3 .dump" },
  ],
};

const CLEANABLE = new Set<Engine>(["postgres", "mongodb"]);
const TOOL_DIR_KEY = "nexus.toolDir";

function readToolDir(): string {
  try {
    return localStorage.getItem(TOOL_DIR_KEY) ?? "";
  } catch {
    return "";
  }
}

function stamp() {
  const d = new Date();
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}${p(d.getMonth() + 1)}${p(d.getDate())}-${p(d.getHours())}${p(d.getMinutes())}`;
}

function bytes(n: number) {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024;
    i++;
  }
  return `${n.toFixed(i ? 1 : 0)} ${units[i]}`;
}

function Label({ text, children }: { text: string; children: React.ReactNode }) {
  return (
    <label className="block">
      <span className="mb-1 block text-[11.5px] text-muted">{text}</span>
      {children}
    </label>
  );
}

function Segmented<T extends string>({ value, options, onChange }: { value: T; options: [T, string][]; onChange: (v: T) => void }) {
  return (
    <div className="flex rounded-lg border border-line bg-panel-2 p-0.5">
      {options.map(([v, label]) => (
        <button key={v} onClick={() => onChange(v)} className={clsx("flex-1 rounded-md px-2 py-1 text-[12px]", value === v ? "bg-elev text-fg shadow-sm" : "text-muted")}>
          {label}
        </button>
      ))}
    </div>
  );
}

export function DumpDialog() {
  const { open, mode, connId, database } = useStore((s) => s.dumpDialog);
  if (!open) return null;
  return <DumpWizard initialMode={mode} initialConn={connId} initialDb={database} />;
}

function DumpWizard({ initialMode, initialConn, initialDb }: { initialMode: DumpMode; initialConn?: string; initialDb?: string }) {
  const connections = useStore((s) => s.connections);
  const nodes = useStore((s) => s.nodes);
  const { closeDump, loadChildren, openDialog } = useStore.getState();
  const [mode, setMode] = useState<DumpMode>(initialMode);
  const supported = connections.filter((c) => FORMATS[c.engine] && (mode === "dump" || !c.readOnly));
  const [connId, setConnId] = useState(initialConn ?? "");
  const conn: ConnectionConfig | undefined = supported.find((c) => c.id === connId) ?? supported[0];
  const engine = conn?.engine;
  const formats = engine ? (FORMATS[engine] ?? []) : [];
  const hasDatabases = !!engine && ENGINE_META[engine].hasDatabases;
  const isSqlite = engine === "sqlite";

  // dump
  const [db, setDb] = useState(initialDb ?? "");
  const [format, setFormat] = useState<DumpFormat>("native");
  const [schemaOnly, setSchemaOnly] = useState(false);
  const [outPath, setOutPath] = useState("");
  // restore
  const [inPath, setInPath] = useState("");
  const [target, setTarget] = useState<"new" | "existing">(initialMode === "restore" && initialDb ? "existing" : "new");
  const [targetDb, setTargetDb] = useState(initialMode === "restore" ? (initialDb ?? "") : "");
  const [clean, setClean] = useState(false);

  const [toolDir, setToolDir] = useState(readToolDir);
  const [tools, setTools] = useState<ToolStatus[] | null>(null);
  const [phase, setPhase] = useState<"setup" | "running" | "done">("setup");
  const [log, setLog] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [dumpReport, setDumpReport] = useState<DumpReport | null>(null);
  const [restoreReport, setRestoreReport] = useState<RestoreReport | null>(null);
  const logRef = useRef<HTMLDivElement>(null);

  const fmt = formats.find((f) => f.id === format) ?? formats[0];
  const databases = conn ? (nodes[nodeKey(conn.id, [])] ?? []).filter((n) => n.kind === "database").map((n) => n.name) : [];

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && phase !== "running" && closeDump();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [phase]);

  useEffect(() => {
    if (conn && hasDatabases) void loadChildren(conn.id, []);
    if (conn && !formats.some((f) => f.id === format)) setFormat(formats[0].id);
  }, [conn?.id]);

  useEffect(() => {
    if (!conn) return;
    let stale = false;
    const t = setTimeout(() => {
      api
        .dumpTools(conn.id, toolDir)
        .then((t) => !stale && setTools(t))
        .catch(() => !stale && setTools([]));
    }, 250);
    return () => {
      stale = true;
      clearTimeout(t);
    };
  }, [conn?.id, toolDir]);

  useEffect(() => {
    logRef.current?.scrollTo({ top: logRef.current.scrollHeight });
  }, [log]);

  const dumpDb = db || conn?.database || "";
  // tools this run needs (SQLite copies and SQL restores need none)
  const needed = !engine
    ? []
    : mode === "dump"
      ? { postgres: ["pg_dump"], mysql: ["mysqldump"], mongodb: ["mongodump"], sqlite: format === "sql" || schemaOnly ? ["sqlite3"] : [] }[engine as string] ?? []
      : { postgres: inPath.toLowerCase().endsWith(".sql") ? ["psql"] : ["pg_restore"], mysql: ["mysql"], mongodb: ["mongorestore"], sqlite: [] }[engine as string] ?? [];
  const missing = needed.filter((n) => tools && !tools.find((t) => t.name === n)?.path);

  const problems: string[] = [];
  if (!conn) problems.push("Add a PostgreSQL, MySQL, MongoDB or SQLite connection first.");
  else if (mode === "dump") {
    if (hasDatabases && !dumpDb) problems.push("Choose the database to dump.");
    if (!outPath.trim()) problems.push("Choose where to save the dump.");
  } else {
    if (!inPath.trim()) problems.push("Choose the dump file to restore.");
    if (isSqlite && target === "existing") {
      if (!conn.filePath) problems.push("This connection has no database file.");
    } else if (!targetDb.trim()) problems.push(isSqlite ? "Choose the new database file." : "Enter the target database.");
    else if (target === "new" && !isSqlite && databases.includes(targetDb.trim())) problems.push(`“${targetDb.trim()}” already exists – pick another name or restore into the existing database.`);
  }
  if (missing.length) problems.push(`${missing.join(", ")} not found – install the client tools or set their folder below.`);

  const browseOut = async () => {
    const name = `${dumpDb || conn?.name || "database"}-${stamp()}.${fmt?.ext ?? "dump"}`;
    if (!isTauri) return setOutPath(`/demo/${name}`);
    const { save } = await import("@tauri-apps/plugin-dialog");
    const p = await save({ defaultPath: name, filters: [{ name: fmt?.label ?? "Dump", extensions: [fmt?.ext.split(".").pop() ?? "dump"] }] });
    if (p) setOutPath(p);
  };

  const browseIn = async () => {
    if (!isTauri) return setInPath("/demo/shop-20260101-1200.dump");
    const { open } = await import("@tauri-apps/plugin-dialog");
    const p = await open({
      multiple: false,
      filters: [
        { name: "Dumps", extensions: ["dump", "backup", "sql", "gz", "archive", "sqlite", "db", "sqlite3"] },
        { name: "All files", extensions: ["*"] },
      ],
    });
    if (typeof p === "string") setInPath(p);
  };

  const browseSqliteTarget = async () => {
    if (!isTauri) return setTargetDb("/demo/restored.sqlite");
    const { save } = await import("@tauri-apps/plugin-dialog");
    const p = await save({ defaultPath: "restored.sqlite", filters: [{ name: "SQLite database", extensions: ["sqlite", "db", "sqlite3"] }] });
    if (p) setTargetDb(p);
  };

  const browseToolDir = async () => {
    if (!isTauri) return;
    const { open } = await import("@tauri-apps/plugin-dialog");
    const p = await open({ directory: true, multiple: false });
    if (typeof p === "string") updateToolDir(p);
  };

  const updateToolDir = (v: string) => {
    setToolDir(v);
    try {
      localStorage.setItem(TOOL_DIR_KEY, v);
    } catch {
      /* storage unavailable */
    }
  };

  const run = async () => {
    if (!conn || problems.length) return;
    setPhase("running");
    setError(null);
    setLog([]);
    let unlisten: (() => void) | undefined;
    if (isTauri) {
      const { listen } = await import("@tauri-apps/api/event");
      unlisten = await listen<string>("dump-log", (e) => setLog((l) => (l.length > 2000 ? [...l.slice(-1500), e.payload] : [...l, e.payload])));
    }
    try {
      if (mode === "dump") {
        const r = await api.dumpRun(conn.id, { database: dumpDb, path: outPath.trim(), format: fmt.id, schemaOnly, toolDir });
        setDumpReport(r);
        if (!isTauri) setLog(r.log);
      } else {
        const sqliteExisting = isSqlite && target === "existing";
        const r = await api.restoreRun(conn.id, {
          path: inPath.trim(),
          database: sqliteExisting ? conn.filePath : targetDb.trim(),
          create: target === "new",
          clean: clean && target === "existing" && CLEANABLE.has(conn.engine),
          toolDir,
        });
        setRestoreReport(r);
        if (!isTauri) setLog(r.log);
        if (!isSqlite) void loadChildren(conn.id, [], true);
      }
      setPhase("done");
    } catch (e) {
      setError(errorText(e));
      setPhase("done");
    } finally {
      unlisten?.();
    }
  };

  const addSqliteConnection = (file: string) => {
    closeDump();
    const name = file.split(/[\\/]/).pop() ?? file;
    openDialog({ ...newConnection("sqlite"), name, filePath: file });
  };

  const title = mode === "dump" ? "Create dump" : "Restore dump";

  return (
    <div className="animate-overlay fixed inset-0 z-50 grid place-items-center bg-black/55 p-5 backdrop-blur-[3px]" onMouseDown={() => phase !== "running" && closeDump()}>
      <div onMouseDown={(e) => e.stopPropagation()} className="animate-pop flex max-h-[94vh] w-[min(720px,100%)] flex-col overflow-hidden rounded-2xl border border-line bg-panel shadow-pop">
        <div className="flex items-center gap-3 border-b border-line px-5 py-3.5">
          <div className="grid h-9 w-9 place-items-center rounded-xl bg-accent/15 text-accent">
            {mode === "dump" ? <DatabaseBackup className="h-4.5 w-4.5" /> : <ArchiveRestore className="h-4.5 w-4.5" />}
          </div>
          <div className="min-w-0">
            <h2 className="text-[15px] font-semibold">{title}</h2>
            <p className="truncate text-[12px] text-muted">
              {mode === "dump" ? "Back up a database to a file" : "Load a dump into a new or an existing database"} · PostgreSQL, MySQL, MongoDB, SQLite
            </p>
          </div>
          <button onClick={closeDump} disabled={phase === "running"} className="ml-auto grid h-8 w-8 place-items-center rounded-lg text-muted hover:bg-hover hover:text-fg disabled:opacity-40">
            <X className="h-4 w-4" />
          </button>
        </div>

        <div className="min-h-0 flex-1 space-y-4 overflow-auto p-5">
          {phase === "setup" ? (
            <>
              <Segmented<DumpMode>
                value={mode}
                onChange={(m) => {
                  setMode(m);
                  setError(null);
                }}
                options={[
                  ["dump", "Create dump"],
                  ["restore", "Restore dump"],
                ]}
              />
              <div className="grid grid-cols-2 gap-3">
                <Label text="Connection">
                  <select className={clsx(inputCls, "cursor-pointer")} value={conn?.id ?? ""} onChange={(e) => setConnId(e.target.value)}>
                    {supported.map((c) => (
                      <option key={c.id} value={c.id}>
                        {c.name || flavorOf(c.engine, c.flavor).name}
                      </option>
                    ))}
                  </select>
                </Label>
                {mode === "dump" && hasDatabases && (
                  <Label text="Database">
                    <select className={clsx(inputCls, "cursor-pointer")} value={dumpDb} onChange={(e) => setDb(e.target.value)}>
                      <option value="">Choose…</option>
                      {[...new Set([...(dumpDb ? [dumpDb] : []), ...databases])].map((d) => (
                        <option key={d}>{d}</option>
                      ))}
                    </select>
                  </Label>
                )}
              </div>

              {mode === "dump" ? (
                <>
                  {formats.length > 1 && (
                    <Label text="Format">
                      <Segmented<DumpFormat> value={fmt?.id ?? "native"} onChange={setFormat} options={formats.map((f) => [f.id, f.label])} />
                    </Label>
                  )}
                  {fmt && <p className="-mt-2 text-[11.5px] text-faint">{fmt.hint}</p>}
                  {engine !== "mongodb" && <Toggle checked={schemaOnly} onChange={setSchemaOnly} label="Structure only" hint="Tables, indexes and views without the rows" />}
                  {isSqlite && schemaOnly && format === "native" && <p className="-mt-2 text-[11.5px] text-faint">Structure-only dumps are written as SQL.</p>}
                  <Label text="Save to">
                    <div className="flex gap-2">
                      <input className={clsx(inputCls, "font-mono text-[11.5px]")} value={outPath} placeholder={`/path/to/backup.${fmt?.ext ?? "dump"}`} onChange={(e) => setOutPath(e.target.value)} />
                      <button onClick={browseOut} className="flex h-8 shrink-0 items-center gap-1.5 rounded-lg border border-line bg-panel-2 px-2.5 text-[12px] hover:bg-hover">
                        <Save className="h-3.5 w-3.5" /> Save as…
                      </button>
                    </div>
                  </Label>
                </>
              ) : (
                <>
                  <Label text="Dump file">
                    <div className="flex gap-2">
                      <input className={clsx(inputCls, "font-mono text-[11.5px]")} value={inPath} placeholder="/path/to/backup.dump" onChange={(e) => setInPath(e.target.value)} />
                      <button onClick={browseIn} className="flex h-8 shrink-0 items-center gap-1.5 rounded-lg border border-line bg-panel-2 px-2.5 text-[12px] hover:bg-hover">
                        <FolderOpen className="h-3.5 w-3.5" /> Browse
                      </button>
                    </div>
                  </Label>
                  <Label text="Restore into">
                    <Segmented
                      value={target}
                      onChange={(t) => {
                        setTarget(t);
                        setTargetDb("");
                      }}
                      options={[
                        ["new", isSqlite ? "New database file" : "New database"],
                        ["existing", isSqlite ? "This connection's file" : "Existing database"],
                      ]}
                    />
                  </Label>
                  {isSqlite ? (
                    target === "new" ? (
                      <Label text="New database file">
                        <div className="flex gap-2">
                          <input className={clsx(inputCls, "font-mono text-[11.5px]")} value={targetDb} placeholder="/path/to/restored.sqlite" onChange={(e) => setTargetDb(e.target.value)} />
                          <button onClick={browseSqliteTarget} className="flex h-8 shrink-0 items-center gap-1.5 rounded-lg border border-line bg-panel-2 px-2.5 text-[12px] hover:bg-hover">
                            <Save className="h-3.5 w-3.5" /> Choose…
                          </button>
                        </div>
                      </Label>
                    ) : (
                      <p className="text-[11.5px] text-faint">
                        SQL scripts run against <span className="font-mono">{conn?.filePath}</span>. A database copy can only be restored to a new file.
                      </p>
                    )
                  ) : target === "new" ? (
                    <Label text="New database name">
                      <input className={inputCls} value={targetDb} placeholder="e.g. shop_restored" onChange={(e) => setTargetDb(e.target.value.trim())} />
                    </Label>
                  ) : (
                    <Label text="Database">
                      <select className={clsx(inputCls, "cursor-pointer")} value={targetDb} onChange={(e) => setTargetDb(e.target.value)}>
                        <option value="">Choose…</option>
                        {databases.map((d) => (
                          <option key={d}>{d}</option>
                        ))}
                      </select>
                    </Label>
                  )}
                  {target === "existing" && engine && CLEANABLE.has(engine) && (
                    <Toggle
                      checked={clean}
                      onChange={setClean}
                      label={engine === "mongodb" ? "Drop collections first" : "Drop objects first"}
                      hint={engine === "mongodb" ? "Collections in the dump are replaced, not merged" : "Tables and other objects in the archive are dropped and recreated (custom archives only)"}
                    />
                  )}
                  {target === "existing" && clean && (
                    <div className="flex items-start gap-2 rounded-lg border border-warn/30 bg-warn/[0.06] p-2 text-[12px] text-warn">
                      <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
                      Data in {targetDb || "the database"} that is also in the dump will be replaced.
                    </div>
                  )}
                  {engine === "postgres" && <p className="text-[11.5px] text-faint">Owners and privileges are not restored, so the dump loads even when its roles don't exist here.</p>}
                </>
              )}

              <div className="rounded-xl border border-line bg-panel-2/40 p-3">
                <div className="mb-2 flex items-center gap-2 text-[11px] font-semibold tracking-[0.08em] text-faint uppercase">
                  <Wrench className="h-3.5 w-3.5" /> Tools
                </div>
                {tools === null ? (
                  <div className="flex items-center gap-2 text-[12px] text-muted">
                    <Spinner className="h-3.5 w-3.5" /> Looking for client tools…
                  </div>
                ) : (
                  <div className="space-y-1">
                    {tools.map((t) => (
                      <div key={t.name} className="flex items-center gap-2 text-[12px]">
                        {t.path ? <CheckCircle2 className="h-3.5 w-3.5 shrink-0 text-success" /> : <XCircle className={clsx("h-3.5 w-3.5 shrink-0", needed.includes(t.name) ? "text-danger" : "text-faint")} />}
                        <span className="w-24 shrink-0 font-mono">{t.name}</span>
                        <span className="min-w-0 truncate text-faint" title={t.path ?? ""}>
                          {t.path ? (t.version ?? t.path) : needed.includes(t.name) ? "not found – needed" : "not found"}
                        </span>
                      </div>
                    ))}
                  </div>
                )}
                <div className="mt-2.5 flex gap-2">
                  <input className={clsx(inputCls, "font-mono text-[11.5px]")} value={toolDir} placeholder="Tools folder (optional, if not on PATH)" onChange={(e) => updateToolDir(e.target.value)} />
                  {isTauri && (
                    <button onClick={browseToolDir} className="flex h-8 shrink-0 items-center gap-1.5 rounded-lg border border-line bg-panel-2 px-2.5 text-[12px] hover:bg-hover">
                      <FolderOpen className="h-3.5 w-3.5" /> Browse
                    </button>
                  )}
                </div>
              </div>
              {error && <div className="selectable rounded-lg border border-danger/30 bg-danger/[0.06] p-2 font-mono text-[12px] break-words text-danger">{error}</div>}
            </>
          ) : (
            <>
              {phase === "done" && (
                <div className="flex items-center gap-3">
                  {error ? (
                    <XCircle className="h-9 w-9 shrink-0 text-danger" />
                  ) : restoreReport?.warnings ? (
                    <AlertTriangle className="h-9 w-9 shrink-0 text-warn" />
                  ) : (
                    <CheckCircle2 className="h-9 w-9 shrink-0 text-success" />
                  )}
                  <div className="min-w-0">
                    <div className="text-[16px] font-semibold">
                      {error
                        ? error === "Cancelled"
                          ? `${mode === "dump" ? "Dump" : "Restore"} cancelled`
                          : `${mode === "dump" ? "Dump" : "Restore"} failed`
                        : mode === "dump"
                          ? "Dump finished"
                          : restoreReport?.warnings
                            ? `Restore finished with ${restoreReport.warnings} error${restoreReport.warnings > 1 ? "s" : ""} skipped`
                            : "Restore finished"}
                    </div>
                    <div className="selectable truncate text-[12.5px] text-muted">
                      {error && error !== "Cancelled"
                        ? error
                        : dumpReport
                          ? `${bytes(dumpReport.bytes)} · ${(dumpReport.elapsedMs / 1000).toFixed(1)} s · ${dumpReport.path}`
                          : restoreReport
                            ? `${restoreReport.database} · ${(restoreReport.elapsedMs / 1000).toFixed(1)} s`
                            : ""}
                    </div>
                  </div>
                </div>
              )}
              {phase === "running" && (
                <div className="flex items-center gap-2 text-[12.5px] text-muted">
                  <Spinner className="h-4 w-4 text-accent" />
                  {mode === "dump" ? `Dumping ${dumpDb || conn?.name}…` : `Restoring into ${target === "existing" && isSqlite ? conn?.filePath : targetDb}…`}
                </div>
              )}
              <div ref={logRef} className="selectable h-[340px] overflow-auto rounded-xl border border-line bg-panel-2/60 p-3 font-mono text-[11.5px] leading-relaxed">
                {log.length === 0 ? (
                  <span className="text-faint">{phase === "running" ? "Waiting for output…" : "No output."}</span>
                ) : (
                  log.map((l, i) => (
                    <div key={i} className={clsx("break-all whitespace-pre-wrap", l.startsWith("$ ") ? "text-accent" : /error|fatal/i.test(l) ? "text-danger" : /warning/i.test(l) ? "text-warn" : "text-muted")}>
                      {l}
                    </div>
                  ))
                )}
              </div>
            </>
          )}
        </div>

        <div className="flex items-center gap-3 border-t border-line bg-panel-2/40 px-5 py-3">
          {phase === "running" ? (
            <>
              <div className="flex-1" />
              <button onClick={() => void api.dumpCancel()} className="h-9 rounded-lg border border-line-strong px-3.5 text-[12.5px] hover:bg-hover">
                Cancel
              </button>
            </>
          ) : phase === "done" ? (
            <>
              <button onClick={() => setPhase("setup")} className="h-9 rounded-lg px-3.5 text-[12.5px] text-muted hover:bg-hover hover:text-fg">
                Back
              </button>
              <div className="flex-1" />
              {!error && mode === "dump" && dumpReport && (
                <button
                  onClick={() => {
                    setMode("restore");
                    setInPath(dumpReport.path);
                    setTarget("new");
                    setTargetDb("");
                    setPhase("setup");
                  }}
                  className="flex h-9 items-center gap-2 rounded-lg border border-line-strong px-3.5 text-[12.5px] hover:bg-hover"
                >
                  <ArchiveRestore className="h-3.5 w-3.5" /> Restore to a new database…
                </button>
              )}
              {!error && isSqlite && restoreReport && target === "new" && (
                <button onClick={() => addSqliteConnection(restoreReport.database)} className="flex h-9 items-center gap-2 rounded-lg border border-line-strong px-3.5 text-[12.5px] hover:bg-hover">
                  <Plus className="h-3.5 w-3.5" /> Add as connection
                </button>
              )}
              <button onClick={closeDump} className="btn-primary h-9 rounded-lg px-4 text-[12.5px] font-semibold text-white">
                Done
              </button>
            </>
          ) : (
            <>
              <div className="min-w-0 flex-1 truncate text-[12px]">
                {problems.length ? <span className="text-faint">{problems[0]}</span> : <span className="text-muted">{mode === "dump" ? `${fmt?.label} of ${dumpDb || conn?.name}` : `Restore into ${target === "new" ? "new" : "existing"} ${isSqlite ? "file" : "database"}`}</span>}
              </div>
              <button onClick={closeDump} className="h-9 rounded-lg px-3.5 text-[12.5px] text-muted hover:bg-hover hover:text-fg">
                Cancel
              </button>
              <button
                onClick={() => {
                  if (problems.length) toast.error(problems[0]);
                  else void run();
                }}
                className={clsx("btn-primary flex h-9 items-center gap-2 rounded-lg px-4 text-[12.5px] font-semibold text-white", problems.length > 0 && "opacity-50")}
              >
                {mode === "dump" ? <DatabaseBackup className="h-3.5 w-3.5" /> : <ArchiveRestore className="h-3.5 w-3.5" />}
                {mode === "dump" ? "Create dump" : "Restore"}
              </button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
