import { json } from "@codemirror/lang-json";
import { syntaxHighlighting } from "@codemirror/language";
import { EditorView } from "@codemirror/view";
import CodeMirror from "@uiw/react-codemirror";
import clsx from "clsx";
import {
  AlertTriangle,
  Braces,
  CheckCircle2,
  Clock,
  Copy,
  Download,
  Eye,
  FileSpreadsheet,
  ListFilter,
  Lock,
  Pencil,
  Plus,
  RotateCcw,
  Save,
  Trash2,
  PanelRightClose,
  PanelRightOpen,
  Rows3,
  Search,
  Sparkles,
  Table as TableIcon,
  X,
} from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import { api, errorText, isTauri } from "../lib/api";
import { cellValue, editState, emptyEdits, pendingCount, withDeleted } from "../lib/edits";
import type { Cell, EditInfo, ExecResponse, PendingEdits, ResultSet } from "../lib/types";
import { highlight, theme as editorTheme } from "./Editor";
import { cellText, DataGrid, type GridEditing, type Selection } from "./DataGrid";
import { Kbd, showContextMenu, Spinner } from "./ui";

function toObjects(set: ResultSet): Record<string, Cell>[] {
  return set.rows.map((r) => {
    const o: Record<string, Cell> = {};
    set.columns.forEach((c, i) => {
      if (!(set.documents && r[i] === null)) o[c.name] = r[i];
    });
    return o;
  });
}

function toCsv(set: ResultSet): string {
  const esc = (s: string) => (/[",\n\r]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s);
  const lines = [set.columns.map((c) => esc(c.name)).join(",")];
  for (const r of set.rows) lines.push(r.map((v) => (v === null ? "" : esc(cellText(v)))).join(","));
  return lines.join("\n");
}

function toMarkdown(set: ResultSet): string {
  const esc = (s: string) => s.replace(/\|/g, "\\|").replace(/\n/g, " ");
  const head = `| ${set.columns.map((c) => esc(c.name)).join(" | ")} |`;
  const sep = `| ${set.columns.map(() => "---").join(" | ")} |`;
  const rows = set.rows.map((r) => `| ${r.map((v) => esc(cellText(v))).join(" | ")} |`);
  return [head, sep, ...rows].join("\n");
}

export async function saveText(contents: string, name: string, ext: string) {
  if (isTauri) {
    const { save } = await import("@tauri-apps/plugin-dialog");
    const path = await save({ defaultPath: `${name}.${ext}`, filters: [{ name: ext.toUpperCase(), extensions: [ext] }] });
    if (!path) return;
    await api.writeTextFile(path, contents);
    toast.success("Exported", { description: path });
  } else {
    const a = document.createElement("a");
    a.href = URL.createObjectURL(new Blob([contents], { type: "text/plain" }));
    a.download = `${name}.${ext}`;
    a.click();
    URL.revokeObjectURL(a.href);
  }
}

async function saveXlsx(set: ResultSet, name: string) {
  const sheet = { name, columns: set.columns, rows: set.rows };
  if (!isTauri) {
    toast.info("Excel export needs the desktop app");
    return;
  }
  const { save } = await import("@tauri-apps/plugin-dialog");
  const path = await save({ defaultPath: `${name}.xlsx`, filters: [{ name: "Excel workbook", extensions: ["xlsx"] }] });
  if (!path) return;
  const id = toast.loading("Writing Excel file…");
  try {
    await api.writeXlsx(path.toLowerCase().endsWith(".xlsx") ? path : `${path}.xlsx`, [sheet]);
    toast.success("Exported", { id, description: path });
  } catch (e) {
    toast.error("Export failed", { id, description: errorText(e) });
  }
}

const NO_EDITS = emptyEdits();

/** Editing context for a result opened from a table. */
export interface ResultEditing {
  /** table / collection name shown in the toolbar */
  target: string;
  info?: EditInfo | null;
  edits?: PendingEdits;
  readOnly: boolean;
  saving: boolean;
  onChange: (edits: PendingEdits) => void;
  onSave: (dryRun?: boolean) => Promise<{ statements: string[] } | null>;
  onDiscard: () => void;
}

const jsonExt = [json(), syntaxHighlighting(highlight), editorTheme, EditorView.lineWrapping, EditorView.editable.of(false)];

export function ResultsPanel({
  result,
  error,
  running,
  activeSet,
  onActiveSet,
  title,
  editing,
}: {
  result?: ExecResponse;
  error?: string;
  running: boolean;
  activeSet: number;
  onActiveSet: (i: number) => void;
  title: string;
  editing?: ResultEditing;
}) {
  const [view, setView] = useState<"grid" | "json">("grid");
  const [filter, setFilter] = useState("");
  // kept across re-runs so a refreshed table stays filtered; only columns present apply
  const [columnFilters, setColumnFilters] = useState<Record<string, string>>({});
  const [showColumnFilters, setShowColumnFilters] = useState(false);
  const [shownRows, setShownRows] = useState<number | null>(null);
  const [selection, setSelection] = useState<Selection | null>(null);
  const [inspector, setInspector] = useState(true);
  const [preview, setPreview] = useState<string[] | null>(null);
  const set = result?.sets[activeSet] ?? result?.sets[0];
  // only the first result of a table tab is the table's rows
  const state = useMemo(
    () => (set && editing && activeSet === 0 && !set.documents === !editing.info?.documents ? editState(set, editing.info, editing.readOnly) : null),
    [set, editing?.info, editing?.readOnly, activeSet],
  );
  const edits = editing?.edits ?? NO_EDITS;
  const pending = pendingCount(editing?.edits);
  const gridEditing: GridEditing | null =
    state?.editable && editing?.info ? { info: editing.info, edits, colEditable: state.colEditable, onChange: editing.onChange } : null;

  useEffect(() => {
    if (!pending || !editing) return;
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key === "s") {
        e.preventDefault();
        void editing.onSave();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [pending, editing]);

  useEffect(() => {
    setSelection(null);
    setFilter("");
    if (set && !set.documents) setView("grid");
  }, [set]);

  const jsonText = useMemo(() => (set && view === "json" ? JSON.stringify(toObjects(set), null, 2) : ""), [set, view]);

  if (running && !result) {
    return (
      <div className="flex h-full flex-col">
        <div className="progress-bar h-0.5 bg-accent-soft" />
        <div className="grid flex-1 place-items-center text-muted">
          <div className="flex items-center gap-2.5 text-[13px]">
            <Spinner className="h-4 w-4 text-accent" /> Executing query…
          </div>
        </div>
      </div>
    );
  }

  if (error) {
    return (
      <div className="h-full overflow-auto p-4">
        {running && <div className="progress-bar -mx-4 -mt-4 mb-4 h-0.5 bg-accent-soft" />}
        <div className="animate-pop rounded-xl border border-danger/25 bg-danger/[0.06] p-4">
          <div className="mb-2 flex items-center gap-2 text-[13px] font-semibold text-danger">
            <AlertTriangle className="h-4 w-4" /> Query failed
            <button
              onClick={() => navigator.clipboard.writeText(error)}
              className="ml-auto flex items-center gap-1 rounded-md px-2 py-1 text-[11.5px] font-normal text-muted hover:bg-hover hover:text-fg"
            >
              <Copy className="h-3 w-3" /> Copy
            </button>
          </div>
          <pre className="selectable font-mono text-[12px] leading-relaxed whitespace-pre-wrap text-fg/90">{error}</pre>
        </div>
      </div>
    );
  }

  if (!result || !set) {
    return (
      <div className="grid h-full place-items-center">
        <div className="text-center">
          <div className="mx-auto mb-3 grid h-11 w-11 place-items-center rounded-2xl bg-accent-soft">
            <Sparkles className="h-5 w-5 text-accent" />
          </div>
          <p className="text-[13px] text-muted">Run a query to see results</p>
          <p className="mt-1.5 text-[12px] text-faint">
            <Kbd>Ctrl</Kbd> + <Kbd>Enter</Kbd> runs the selection or the whole editor
          </p>
        </div>
      </div>
    );
  }

  const hasGrid = set.columns.length > 0;
  const activeColumnFilters = set.columns.filter((c) => columnFilters[c.name]?.trim()).length;
  const exportMenu = (e: React.MouseEvent) =>
    showContextMenu(e, [
      { label: "Export CSV", icon: <Download className="h-3.5 w-3.5" />, onSelect: () => saveText(toCsv(set), title, "csv").catch((x) => toast.error(errorText(x))) },
      { label: "Export JSON", icon: <Braces className="h-3.5 w-3.5" />, onSelect: () => saveText(JSON.stringify(toObjects(set), null, 2), title, "json").catch((x) => toast.error(errorText(x))) },
      { label: "Export Excel (.xlsx)", icon: <FileSpreadsheet className="h-3.5 w-3.5" />, onSelect: () => void saveXlsx(set, title) },
      "separator",
      { label: "Copy as CSV", icon: <Copy className="h-3.5 w-3.5" />, onSelect: () => navigator.clipboard.writeText(toCsv(set)).then(() => toast.success("Copied CSV")) },
      { label: "Copy as JSON", icon: <Copy className="h-3.5 w-3.5" />, onSelect: () => navigator.clipboard.writeText(JSON.stringify(toObjects(set), null, 2)).then(() => toast.success("Copied JSON")) },
      { label: "Copy as Markdown", icon: <Copy className="h-3.5 w-3.5" />, onSelect: () => navigator.clipboard.writeText(toMarkdown(set)).then(() => toast.success("Copied Markdown")) },
    ]);

  return (
    <div className="flex h-full flex-col">
      {running ? <div className="progress-bar h-0.5 bg-accent-soft" /> : <div className="h-0.5" />}
      <div className="flex h-10 shrink-0 items-center gap-1 border-b border-line px-2">
        <div className="flex min-w-0 items-center gap-0.5 overflow-x-auto">
          {result.sets.map((s, i) => (
            <button
              key={i}
              onClick={() => onActiveSet(i)}
              className={clsx(
                "flex shrink-0 items-center gap-1.5 rounded-md px-2.5 py-1 text-[12px] transition-colors",
                i === activeSet ? "bg-accent-soft text-fg" : "text-muted hover:bg-hover hover:text-fg",
              )}
            >
              {s.columns.length ? <Rows3 className="h-3.5 w-3.5" /> : <CheckCircle2 className="h-3.5 w-3.5 text-success" />}
              {result.sets.length > 1 ? `Result ${i + 1}` : "Result"}
              <span className="text-faint">{s.columns.length ? s.rows.length.toLocaleString() : "ok"}</span>
            </button>
          ))}
        </div>
        <div className="flex-1" />
        {hasGrid && editing && state && (
          <div className="mr-1 flex items-center gap-1">
            <span
              title={state.reason ?? (state.editable ? "Double-click a cell to edit · Enter / Tab to move · Del sets NULL · Ctrl+S saves" : undefined)}
              className={clsx(
                "flex items-center gap-1 rounded-md px-1.5 py-0.5 text-[11.5px]",
                state.editable ? "text-muted" : "text-faint",
              )}
            >
              {state.editable ? <Pencil className="h-3 w-3 text-accent" /> : <Lock className="h-3 w-3" />}
              <span className="max-w-[140px] truncate">{editing.target}</span>
            </span>
            {state.editable && (
              <>
                <button
                  title="Add row"
                  onClick={() => editing.onChange({ ...edits, inserts: [...edits.inserts, {}] })}
                  className="flex items-center gap-1 rounded-md px-1.5 py-1 text-[12px] text-muted hover:bg-hover hover:text-fg"
                >
                  <Plus className="h-3.5 w-3.5" /> Row
                </button>
                <button
                  title="Delete selected row"
                  disabled={!selection || !editing.info}
                  onClick={() => selection && editing.info && editing.onChange(withDeleted(edits, set, editing.info, [selection.row], true))}
                  className="grid h-7 w-7 place-items-center rounded-md text-muted hover:bg-hover hover:text-danger disabled:opacity-40"
                >
                  <Trash2 className="h-3.5 w-3.5" />
                </button>
              </>
            )}
            {pending > 0 && (
              <div className="ml-1 flex items-center gap-1 rounded-lg border border-warn/40 bg-warn/[0.08] py-0.5 pr-0.5 pl-2">
                <span className="text-[11.5px] font-medium whitespace-nowrap text-warn">
                  {pending} change{pending > 1 ? "s" : ""}
                </span>
                <button
                  title="Preview the statements"
                  onClick={async () => {
                    try {
                      const r = await editing.onSave(true);
                      if (r) setPreview(r.statements);
                    } catch (e) {
                      toast.error("Cannot preview", { description: errorText(e) });
                    }
                  }}
                  className="grid h-6 w-6 place-items-center rounded text-muted hover:bg-hover hover:text-fg"
                >
                  <Eye className="h-3.5 w-3.5" />
                </button>
                <button title="Discard all changes" onClick={editing.onDiscard} className="grid h-6 w-6 place-items-center rounded text-muted hover:bg-hover hover:text-fg">
                  <RotateCcw className="h-3.5 w-3.5" />
                </button>
                <button
                  onClick={() => void editing.onSave()}
                  disabled={editing.saving}
                  title="Save (Ctrl+S)"
                  className="btn-primary flex h-6 items-center gap-1 rounded-md px-2 text-[11.5px] font-semibold text-white disabled:opacity-60"
                >
                  {editing.saving ? <Spinner className="h-3 w-3" /> : <Save className="h-3 w-3" />} Save
                </button>
              </div>
            )}
          </div>
        )}
        {hasGrid && (
          <>
            <div className="flex items-center gap-1.5 rounded-md border border-line bg-panel-2 px-2 py-1 focus-within:border-accent/60">
              <Search className="h-3 w-3 text-faint" />
              <input
                value={filter}
                onChange={(e) => setFilter(e.target.value)}
                placeholder="Filter rows"
                className="w-32 bg-transparent text-[12px] outline-none placeholder:text-faint"
              />
              {filter && <X className="h-3 w-3 text-faint hover:text-fg" onClick={() => setFilter("")} />}
            </div>
            <button
              title="Filter by column"
              onClick={() => {
                if (showColumnFilters) setColumnFilters({});
                setShowColumnFilters((v) => !v);
                setView("grid");
              }}
              className={clsx(
                "relative grid h-7 w-7 place-items-center rounded-md hover:bg-hover",
                showColumnFilters || activeColumnFilters ? "text-accent" : "text-muted",
              )}
            >
              <ListFilter className="h-3.5 w-3.5" />
              {activeColumnFilters > 0 && (
                <span className="absolute -top-0.5 -right-0.5 grid h-3.5 min-w-3.5 place-items-center rounded-full bg-accent px-0.5 text-[9px] font-semibold text-white">
                  {activeColumnFilters}
                </span>
              )}
            </button>
            <div className="ml-1 flex rounded-md border border-line bg-panel-2 p-0.5">
              <button
                onClick={() => setView("grid")}
                className={clsx("flex items-center gap-1 rounded px-2 py-0.5 text-[11.5px]", view === "grid" ? "bg-elev text-fg shadow-sm" : "text-muted")}
              >
                <TableIcon className="h-3 w-3" /> Grid
              </button>
              <button
                onClick={() => setView("json")}
                className={clsx("flex items-center gap-1 rounded px-2 py-0.5 text-[11.5px]", view === "json" ? "bg-elev text-fg shadow-sm" : "text-muted")}
              >
                <Braces className="h-3 w-3" /> JSON
              </button>
            </div>
            <button onClick={exportMenu} className="ml-1 flex items-center gap-1.5 rounded-md px-2 py-1 text-[12px] text-muted hover:bg-hover hover:text-fg">
              <Download className="h-3.5 w-3.5" /> Export
            </button>
            <button
              title="Value inspector"
              onClick={() => setInspector((v) => !v)}
              className={clsx("grid h-7 w-7 place-items-center rounded-md hover:bg-hover", inspector ? "text-accent" : "text-muted")}
            >
              {inspector ? <PanelRightClose className="h-3.5 w-3.5" /> : <PanelRightOpen className="h-3.5 w-3.5" />}
            </button>
          </>
        )}
      </div>

      <div className="flex min-h-0 flex-1">
        <div className="min-w-0 flex-1">
          {!hasGrid ? (
            <div className="grid h-full place-items-center">
              <div className="animate-pop text-center">
                <CheckCircle2 className="mx-auto mb-2 h-9 w-9 text-success" />
                <p className="text-[14px] font-medium">{set.message ?? "Query executed successfully"}</p>
                {set.affected != null && (
                  <p className="mt-1 text-[12.5px] text-muted">
                    {set.affected.toLocaleString()} row{set.affected === 1 ? "" : "s"} affected
                  </p>
                )}
              </div>
            </div>
          ) : view === "grid" ? (
            <DataGrid
              set={set}
              filter={filter}
              columnFilters={columnFilters}
              onColumnFilters={setColumnFilters}
              showColumnFilters={showColumnFilters}
              onShownRows={setShownRows}
              onSelect={setSelection}
              editing={gridEditing}
            />
          ) : (
            <CodeMirror value={jsonText} extensions={jsonExt} theme="none" height="100%" className="h-full overflow-auto text-[12px]" basicSetup={{ foldGutter: true, lineNumbers: true }} />
          )}
        </div>
        {hasGrid && inspector && view === "grid" && (
          <div className="flex w-[300px] shrink-0 flex-col border-l border-line bg-panel">
            <div className="flex h-9 items-center gap-2 border-b border-line px-3 text-[11px] font-semibold tracking-[0.08em] text-faint uppercase">
              Inspector
              {selection && (
                <button
                  onClick={() => {
                    const v = selection.value;
                    navigator.clipboard.writeText(typeof v === "object" && v !== null ? JSON.stringify(v, null, 2) : v === null ? "" : String(v));
                    toast.success("Copied value");
                  }}
                  className="ml-auto flex items-center gap-1 rounded px-1.5 py-0.5 text-[11px] font-normal tracking-normal text-muted normal-case hover:bg-hover hover:text-fg"
                >
                  <Copy className="h-3 w-3" /> Copy
                </button>
              )}
            </div>
            {selection ? (
              <div className="min-h-0 flex-1 overflow-auto p-3">
                <div className="mb-1 text-[11px] text-faint">
                  {selection.row < set.rows.length ? `row ${selection.row + 1}` : "new row"} · <span className="text-muted">{selection.column}</span>
                </div>
                <pre className="selectable font-mono text-[12px] leading-relaxed break-all whitespace-pre-wrap text-fg">
                  {selection.value === null
                    ? "NULL"
                    : typeof selection.value === "object"
                      ? JSON.stringify(selection.value, null, 2)
                      : String(selection.value)}
                </pre>
                <div className="mt-4 border-t border-line pt-3">
                  <div className="mb-2 text-[11px] text-faint">Row</div>
                  {set.columns.map((c, i) => (
                    <div key={i} className="flex gap-2 py-0.5 text-[11.5px]">
                      <span className="w-24 shrink-0 truncate text-muted">{c.name}</span>
                      <span className="selectable truncate font-mono text-fg/90">
                        {cellText(cellValue(set, edits, selection.row, i) ?? null)}
                      </span>
                    </div>
                  ))}
                </div>
              </div>
            ) : (
              <div className="p-4 text-[12px] text-faint">Select a cell to inspect its value. Use arrow keys to move, Ctrl+C to copy.</div>
            )}
          </div>
        )}
      </div>

      <div className="flex h-7 shrink-0 items-center gap-3 border-t border-line px-3 text-[11.5px] text-faint">
        {hasGrid && (
          <span>
            {shownRows !== null && shownRows !== set.rows.length && (
              <>
                <span className="text-accent">{shownRows.toLocaleString()}</span> of{" "}
              </>
            )}
            <span className="text-muted">{set.rows.length.toLocaleString()}</span> rows · {set.columns.length} columns
          </span>
        )}
        {set.truncated && (
          <span className="rounded bg-warn/12 px-1.5 py-px text-warn">limited — raise the row limit or add LIMIT/paging</span>
        )}
        {set.message && hasGrid && <span className="truncate">{set.message}</span>}
        {pending > 0 && <span className="text-warn">unsaved changes – Ctrl+S to save</span>}
        <span className="ml-auto flex items-center gap-1">
          <Clock className="h-3 w-3" /> {result.elapsedMs.toLocaleString()} ms
        </span>
      </div>
      {preview && <StatementsPreview statements={preview} onClose={() => setPreview(null)} />}
    </div>
  );
}

function StatementsPreview({ statements, onClose }: { statements: string[]; onClose: () => void }) {
  const text = statements.map((s) => (s.trimEnd().endsWith(";") ? s : `${s};`)).join("\n\n");
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
  return (
    <div className="animate-overlay fixed inset-0 z-50 grid place-items-center bg-black/50 p-6" onMouseDown={onClose}>
      <div onMouseDown={(e) => e.stopPropagation()} className="animate-pop flex max-h-[80vh] w-[min(760px,100%)] flex-col rounded-2xl border border-line bg-panel shadow-pop">
        <div className="flex items-center gap-2 border-b border-line px-4 py-3">
          <Eye className="h-4 w-4 text-accent" />
          <span className="text-[14px] font-semibold">Statements to run</span>
          <span className="text-[12px] text-faint">· {statements.length}</span>
          <button
            onClick={() => navigator.clipboard.writeText(text).then(() => toast.success("Copied"))}
            className="ml-auto flex items-center gap-1 rounded-md px-2 py-1 text-[12px] text-muted hover:bg-hover hover:text-fg"
          >
            <Copy className="h-3.5 w-3.5" /> Copy
          </button>
          <button onClick={onClose} className="grid h-7 w-7 place-items-center rounded-md text-muted hover:bg-hover hover:text-fg">
            <X className="h-4 w-4" />
          </button>
        </div>
        <pre className="selectable min-h-0 flex-1 overflow-auto p-4 font-mono text-[12px] leading-relaxed whitespace-pre-wrap">{text}</pre>
      </div>
    </div>
  );
}
