import { useVirtualizer } from "@tanstack/react-virtual";
import clsx from "clsx";
import { ArrowDown, ArrowUp, Ban, Copy, CopyPlus, KeyRound, Pencil, Plus, RotateCcw, Trash2, Undo2 } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { cellValue, editText, parseInput, withCell, withDeleted, withReverted } from "../lib/edits";
import type { Cell, EditInfo, PendingEdits, ResultSet } from "../lib/types";
import { showContextMenu, type MenuEntry } from "./ui";

export function cellText(v: Cell): string {
  if (v === null || v === undefined) return "NULL";
  if (typeof v === "object") return JSON.stringify(v);
  return String(v);
}

const NUMERIC = /^(int|int2|int4|int8|integer|bigint|smallint|tinyint|float|float4|float8|double|real|numeric|decimal|money|u?int\d*|float\d*|decimal.*|intn|floatn|decimaln|numericn)/i;
const ROW_H = 30;
const NUM_W = 52;

function guessWidth(name: string, rows: Cell[][], col: number) {
  let max = name.length + 3;
  for (let i = 0; i < Math.min(rows.length, 60); i++) {
    const v = rows[i][col];
    const len = typeof v === "object" && v !== null ? Math.min(40, JSON.stringify(v).length + 4) : cellText(v).length;
    if (len > max) max = len;
  }
  return Math.max(84, Math.min(380, max * 7.4 + 26));
}

function compare(a: Cell, b: Cell): number {
  if (a === b) return 0;
  if (a === null || a === undefined) return 1;
  if (b === null || b === undefined) return -1;
  if (typeof a === "number" && typeof b === "number") return a - b;
  const na = Number(a);
  const nb = Number(b);
  if (typeof a === "string" && typeof b === "string" && a.trim() !== "" && !isNaN(na) && !isNaN(nb)) return na - nb;
  return cellText(a).localeCompare(cellText(b), undefined, { numeric: true });
}

export interface Selection {
  /** index into `set.rows`; values ≥ rows.length are new (unsaved) rows */
  row: number;
  col: number;
  value: Cell;
  column: string;
}

/** Inline editing of a table opened from the tree. */
export interface GridEditing {
  info: EditInfo;
  edits: PendingEdits;
  colEditable: boolean[];
  onChange: (edits: PendingEdits) => void;
}

export function DataGrid({
  set,
  filter,
  onSelect,
  editing,
}: {
  set: ResultSet;
  filter: string;
  onSelect?: (s: Selection | null) => void;
  editing?: GridEditing | null;
}) {
  const [widths, setWidths] = useState<number[]>([]);
  const [sort, setSort] = useState<{ col: number; dir: 1 | -1 } | null>(null);
  const [sel, setSel] = useState<{ r: number; c: number } | null>(null);
  const [editor, setEditor] = useState<{ r: number; c: number; text: string } | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const edits = editing?.edits;
  const base = set.rows.length;
  const inserted = edits?.inserts.length ?? 0;

  useEffect(() => {
    setWidths(set.columns.map((c, i) => guessWidth(c.name, set.rows, i)));
    setSort(null);
    setSel(null);
    setEditor(null);
  }, [set]);

  const order = useMemo(() => {
    let idx = set.rows.map((_, i) => i);
    const f = filter.trim().toLowerCase();
    if (f) idx = idx.filter((i) => set.rows[i].some((v) => cellText(v).toLowerCase().includes(f)));
    if (sort) idx.sort((a, b) => compare(set.rows[a][sort.col], set.rows[b][sort.col]) * sort.dir);
    // new rows always stay at the bottom
    for (let i = 0; i < inserted; i++) idx.push(base + i);
    return idx;
  }, [set, filter, sort, inserted, base]);

  const value = (row: number, col: number): Cell | undefined => cellValue(set, edits, row, col);
  const isDeleted = (row: number) => !!edits && row < base && row in edits.deletes;
  const isChanged = (row: number, col: number) => !!edits && row < base && set.columns[col].name in (edits.updates[row]?.values ?? {});
  const canEdit = (row: number, col: number) => !!editing && editing.colEditable[col] && !isDeleted(row);

  const startEdit = (r: number, c: number, text?: string) => {
    const row = order[r];
    if (row === undefined || !canEdit(row, c)) return;
    setEditor({ r, c, text: text ?? editText(value(row, c)) });
  };

  const commit = (move?: [number, number]) => {
    if (!editor || !editing) return;
    const row = order[editor.r];
    const v = parseInput(editor.text, value(row, editor.c), editing.info.documents);
    if (row !== undefined && JSON.stringify(v) !== JSON.stringify(value(row, editor.c) ?? null)) {
      editing.onChange(withCell(editing.edits, set, editing.info, row, editor.c, v));
    }
    setEditor(null);
    scrollRef.current?.focus();
    if (move) {
      const r = Math.max(0, Math.min(order.length - 1, editor.r + move[0]));
      const c = Math.max(0, Math.min(set.columns.length - 1, editor.c + move[1]));
      select(r, c);
    }
  };

  const setNull = (row: number, col: number) => {
    if (editing && canEdit(row, col)) editing.onChange(withCell(editing.edits, set, editing.info, row, col, null));
  };

  const rowMenu = (e: React.MouseEvent, r: number, c: number) => {
    select(r, c);
    const row = order[r];
    const copy: MenuEntry = {
      label: "Copy value",
      icon: <Copy className="h-3.5 w-3.5" />,
      shortcut: "Ctrl C",
      onSelect: () => {
        const v = value(row, c);
        void navigator.clipboard.writeText(typeof v === "object" && v !== null ? JSON.stringify(v, null, 2) : v == null ? "" : String(v));
      },
    };
    if (!editing) return showContextMenu(e, [copy]);
    const { info } = editing;
    const deleted = isDeleted(row);
    const touched = deleted || row >= base || !!edits?.updates[row];
    showContextMenu(e, [
      ...(canEdit(row, c)
        ? ([
            { label: "Edit cell", icon: <Pencil className="h-3.5 w-3.5" />, shortcut: "Enter", onSelect: () => startEdit(r, c) },
            { label: info.documents ? "Set to null" : "Set to NULL", icon: <Ban className="h-3.5 w-3.5" />, shortcut: "Del", onSelect: () => setNull(row, c) },
          ] as MenuEntry[])
        : []),
      copy,
      "separator",
      { label: "Add row", icon: <Plus className="h-3.5 w-3.5" />, onSelect: () => addRow() },
      {
        label: "Duplicate row",
        icon: <CopyPlus className="h-3.5 w-3.5" />,
        onSelect: () => {
          const values: Record<string, Cell> = {};
          // keys stay empty so the database assigns new ones
          set.columns.forEach((col, i) => {
            if (editing.colEditable[i] && !info.keyColumns.includes(col.name)) values[col.name] = value(row, i) ?? null;
          });
          addRow(values);
        },
      },
      deleted
        ? { label: "Restore row", icon: <Undo2 className="h-3.5 w-3.5" />, onSelect: () => editing.onChange(withDeleted(editing.edits, set, info, [row], false)) }
        : { label: row >= base ? "Remove new row" : "Delete row", icon: <Trash2 className="h-3.5 w-3.5" />, danger: true, onSelect: () => editing.onChange(withDeleted(editing.edits, set, info, [row], true)) },
      ...(touched && row < base
        ? ([{ label: "Revert row", icon: <RotateCcw className="h-3.5 w-3.5" />, onSelect: () => editing.onChange(withReverted(editing.edits, set, row)) }] as MenuEntry[])
        : []),
    ]);
  };

  const addRow = (values: Record<string, Cell> = {}) => {
    if (!editing) return;
    editing.onChange({ ...editing.edits, inserts: [...editing.edits.inserts, values] });
    // the new row lands at the bottom
    requestAnimationFrame(() => {
      const r = order.length;
      const c = Math.max(0, editing.colEditable.findIndex(Boolean));
      select(r, c);
      virt.scrollToIndex(r);
    });
  };

  const virt = useVirtualizer({
    count: order.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_H,
    overscan: 24,
  });

  const numeric = useMemo(
    () =>
      set.columns.map((c, i) => {
        if (NUMERIC.test(c.typeName)) return true;
        if (c.typeName) return false;
        const sample = set.rows.slice(0, 20).map((r) => r[i]).filter((v) => v !== null);
        return sample.length > 0 && sample.every((v) => typeof v === "number");
      }),
    [set],
  );

  const select = (r: number, c: number) => {
    setSel({ r, c });
    const row = order[r];
    onSelect?.(row !== undefined && set.columns[c] ? { row, col: c, value: value(row, c) ?? null, column: set.columns[c].name } : null);
  };

  // keep the inspector in sync when the selected cell is edited
  useEffect(() => {
    if (sel) select(sel.r, sel.c);
  }, [edits]);

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (!sel || editor) return;
    const row = order[sel.r];
    if (editing && row !== undefined) {
      if (e.key === "Enter" || e.key === "F2") {
        e.preventDefault();
        return startEdit(sel.r, sel.c);
      }
      if (e.key === "Delete" || e.key === "Backspace") {
        e.preventDefault();
        return setNull(row, sel.c);
      }
      if (e.key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey && canEdit(row, sel.c)) {
        e.preventDefault();
        return startEdit(sel.r, sel.c, e.key);
      }
    }
    const moves: Record<string, [number, number]> = { ArrowUp: [-1, 0], ArrowDown: [1, 0], ArrowLeft: [0, -1], ArrowRight: [0, 1] };
    if (moves[e.key]) {
      e.preventDefault();
      const [dr, dc] = moves[e.key];
      const r = Math.max(0, Math.min(order.length - 1, sel.r + dr));
      const c = Math.max(0, Math.min(set.columns.length - 1, sel.c + dc));
      select(r, c);
      virt.scrollToIndex(r);
    } else if ((e.ctrlKey || e.metaKey) && e.key === "c") {
      e.preventDefault();
      const v = value(order[sel.r], sel.c);
      navigator.clipboard.writeText(typeof v === "object" && v !== null ? JSON.stringify(v, null, 2) : v === null ? "" : String(v));
    }
  };

  const startResize = (e: React.MouseEvent, col: number) => {
    e.preventDefault();
    e.stopPropagation();
    const startX = e.clientX;
    const startW = widths[col];
    const move = (ev: MouseEvent) =>
      setWidths((w) => w.map((x, i) => (i === col ? Math.max(48, startW + ev.clientX - startX) : x)));
    const up = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      document.body.style.cursor = "";
    };
    document.body.style.cursor = "col-resize";
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  };

  const total = NUM_W + widths.reduce((a, b) => a + b, 0);
  const template = `${NUM_W}px ${widths.map((w) => `${w}px`).join(" ")}`;

  return (
    <div ref={scrollRef} tabIndex={0} onKeyDown={onKeyDown} className="relative h-full overflow-auto outline-none">
      <div style={{ width: Math.max(total, 0), minWidth: "100%" }}>
        {/* header */}
        <div
          className="sticky top-0 z-10 grid border-b border-line bg-panel-2/95 backdrop-blur"
          style={{ gridTemplateColumns: template, height: 34 }}
        >
          <div className="sticky left-0 z-10 border-r border-line bg-panel-2" />
          {set.columns.map((c, i) => (
            <div
              key={i}
              onClick={() =>
                setSort((s) => (s?.col === i ? (s.dir === 1 ? { col: i, dir: -1 } : null) : { col: i, dir: 1 }))
              }
              className="group relative flex min-w-0 items-center gap-1.5 border-r border-line px-2.5 hover:bg-hover"
              title={c.typeName ? `${c.name} · ${c.typeName}` : c.name}
            >
              {(editing ? editing.info.keyColumns.includes(c.name) : c.name === "_id" || c.name === "id" || c.name === "_key") ? (
                <KeyRound className="h-3 w-3 shrink-0 text-warn" />
              ) : null}
              <span className="truncate text-[12px] font-semibold text-fg">{c.name}</span>
              {c.typeName && <span className="truncate font-mono text-[10px] text-faint">{c.typeName}</span>}
              {sort?.col === i &&
                (sort.dir === 1 ? (
                  <ArrowUp className="ml-auto h-3 w-3 shrink-0 text-accent" />
                ) : (
                  <ArrowDown className="ml-auto h-3 w-3 shrink-0 text-accent" />
                ))}
              <div
                onMouseDown={(e) => startResize(e, i)}
                onClick={(e) => e.stopPropagation()}
                className="absolute top-0 -right-[3px] z-10 h-full w-[6px] cursor-col-resize opacity-0 group-hover:opacity-100"
              >
                <div className="mx-auto h-full w-[2px] bg-accent" />
              </div>
            </div>
          ))}
        </div>
        {/* body */}
        <div className="relative" style={{ height: virt.getTotalSize() }}>
          {virt.getVirtualItems().map((vr) => {
            const row = order[vr.index];
            const isNew = row >= base;
            const deleted = isDeleted(row);
            return (
              <div
                key={vr.key}
                className={clsx(
                  "absolute left-0 grid w-full",
                  isNew ? "bg-success/[0.07]" : deleted ? "bg-danger/[0.08]" : vr.index % 2 === 1 && "bg-[var(--grid-alt)]",
                )}
                style={{ transform: `translateY(${vr.start}px)`, height: ROW_H, gridTemplateColumns: template }}
              >
                <div
                  className={clsx(
                    "sticky left-0 z-[1] flex items-center justify-end border-r border-b border-line bg-panel pr-2.5 font-mono text-[10.5px]",
                    isNew ? "text-success" : deleted ? "text-danger" : "text-faint",
                  )}
                  title={isNew ? "New row – not saved yet" : deleted ? "Will be deleted on save" : undefined}
                >
                  {isNew ? "+" : deleted ? "−" : row + 1}
                </div>
                {set.columns.map((_, ci) => {
                  const active = sel?.r === vr.index && sel?.c === ci;
                  const changed = isChanged(row, ci);
                  const v = value(row, ci);
                  return (
                    <div
                      key={ci}
                      onMouseDown={() => select(vr.index, ci)}
                      onDoubleClick={() => startEdit(vr.index, ci)}
                      onContextMenu={(e) => rowMenu(e, vr.index, ci)}
                      title={editing && !editing.colEditable[ci] ? "Read-only column" : undefined}
                      className={clsx(
                        "relative flex min-w-0 items-center border-r border-b border-line px-2.5 text-[12.5px]",
                        numeric[ci] && "justify-end",
                        deleted && "text-faint line-through",
                        changed && "bg-warn/15",
                        active ? "bg-accent-soft ring-1 ring-accent ring-inset" : sel?.r === vr.index && !changed && "bg-hover",
                      )}
                    >
                      {editor && editor.r === vr.index && editor.c === ci ? (
                        <input
                          autoFocus
                          value={editor.text}
                          onChange={(e) => setEditor({ ...editor, text: e.target.value })}
                          onBlur={() => commit()}
                          onKeyDown={(e) => {
                            e.stopPropagation();
                            if (e.key === "Enter") {
                              e.preventDefault();
                              commit([1, 0]);
                            } else if (e.key === "Tab") {
                              e.preventDefault();
                              commit([0, e.shiftKey ? -1 : 1]);
                            } else if (e.key === "Escape") {
                              e.preventDefault();
                              setEditor(null);
                              scrollRef.current?.focus();
                            }
                          }}
                          className="absolute inset-0 z-[2] w-full bg-elev px-2.5 font-mono text-[12px] text-fg outline-none ring-2 ring-accent ring-inset"
                        />
                      ) : isNew && v === undefined ? (
                        <span className="font-mono text-[11px] text-faint/70 italic">default</span>
                      ) : (
                        <CellView value={v ?? null} numeric={numeric[ci]} />
                      )}
                      {changed && <span className="absolute top-0.5 right-0.5 h-1.5 w-1.5 rounded-full bg-warn" />}
                    </div>
                  );
                })}
              </div>
            );
          })}
        </div>
        {order.length === inserted && set.rows.length > 0 && filter && (
          <div className="p-6 text-center text-[12.5px] text-faint">No rows match “{filter}”</div>
        )}
      </div>
    </div>
  );
}

const OID = /^ObjectId\('([0-9a-f]{24})'\)$/;
const DATE = /^\d{4}-\d{2}-\d{2}([ T]\d{2}:\d{2}(:\d{2}(\.\d+)?)?(Z|[+-]\d{2}:?\d{2})?)?$/;

function CellView({ value, numeric }: { value: Cell; numeric: boolean }) {
  if (value === null || value === undefined) return <span className="font-mono text-[11px] text-faint/80 italic">NULL</span>;
  if (typeof value === "boolean")
    return (
      <span
        className={clsx(
          "rounded px-1.5 py-px font-mono text-[10.5px] font-medium",
          value ? "bg-success/12 text-success" : "bg-danger/10 text-danger",
        )}
      >
        {String(value)}
      </span>
    );
  if (typeof value === "number" || numeric)
    return <span className="truncate font-mono text-[12px] text-[var(--c-num)]">{String(value)}</span>;
  if (typeof value === "object") {
    const isArr = Array.isArray(value);
    const n = isArr ? value.length : Object.keys(value).length;
    return (
      <span className="flex min-w-0 items-center gap-1.5">
        <span className="shrink-0 rounded bg-accent-soft px-1 font-mono text-[10px] text-accent">
          {isArr ? `[${n}]` : `{${n}}`}
        </span>
        <span className="truncate font-mono text-[11.5px] text-muted">{JSON.stringify(value)}</span>
      </span>
    );
  }
  const m = OID.exec(value);
  if (m)
    return (
      <span className="truncate font-mono text-[11.5px]">
        <span className="text-faint">ObjectId(</span>
        <span className="text-[var(--c-oid)]">{m[1]}</span>
        <span className="text-faint">)</span>
      </span>
    );
  if (DATE.test(value)) return <span className="truncate font-mono text-[11.5px] text-[var(--c-date)]">{value}</span>;
  return <span className="truncate">{value.length > 300 ? value.slice(0, 300) + "…" : value}</span>;
}
