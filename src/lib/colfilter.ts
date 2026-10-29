import type { Cell } from "./types";

/**
 * Per-column filters in the results grid. A filter is one expression:
 *
 *   text       contains (case-insensitive)      !text      does not contain
 *   =text      equals                           !=text     not equal (also <>)
 *   >10  >=10  <10  <=10                        numbers, dates or text
 *   ^text      starts with                      text$      ends with
 *   /regex/    regular expression               null / !null
 *   a | b      any of several expressions
 */
export const FILTER_HELP =
  "text · !text · =exact · !=value · > >= < <= · ^starts · ends$ · /regex/ · null · !null · a | b";

export type CellTest = (v: Cell) => boolean;

function text(v: Cell): string {
  if (v === null || v === undefined) return "";
  return typeof v === "object" ? JSON.stringify(v) : String(v);
}

function num(s: string): number | null {
  const t = s.trim();
  if (t === "" || isNaN(Number(t))) return null;
  return Number(t);
}

/** Numbers compare as numbers, everything else (ISO dates included) as text. */
function cmp(v: Cell, operand: string): number | null {
  if (v === null || v === undefined) return null;
  const a = typeof v === "number" ? v : num(text(v));
  const b = num(operand);
  if (a !== null && b !== null) return a - b;
  return text(v).localeCompare(operand, undefined, { numeric: true, sensitivity: "base" });
}

function compileOne(expr: string): CellTest | null {
  const e = expr.trim();
  if (!e) return null;
  const lower = e.toLowerCase();
  if (lower === "null" || lower === "is null") return (v) => v === null || v === undefined;
  if (lower === "!null" || lower === "not null" || lower === "is not null") return (v) => v !== null && v !== undefined;

  const regex = /^\/(.+)\/([a-z]*)$/.exec(e);
  if (regex) {
    try {
      const re = new RegExp(regex[1], (regex[2] || "i").replace("g", ""));
      return (v) => v !== null && v !== undefined && re.test(text(v));
    } catch {
      return () => false;
    }
  }

  const op = /^(>=|<=|!=|<>|>|<|=)\s*(.*)$/.exec(e);
  if (op) {
    const [, o, raw] = op;
    const operand = raw.trim();
    const eq = (v: Cell) => {
      if (v === null || v === undefined) return false;
      const n = num(operand);
      if (n !== null && (typeof v === "number" || num(text(v)) !== null)) return Number(typeof v === "number" ? v : text(v)) === n;
      return text(v).toLowerCase() === operand.toLowerCase();
    };
    switch (o) {
      case "=":
        return eq;
      case "!=":
      case "<>":
        return (v) => !eq(v);
      default:
        return (v) => {
          const c = cmp(v, operand);
          if (c === null) return false;
          return o === ">" ? c > 0 : o === ">=" ? c >= 0 : o === "<" ? c < 0 : c <= 0;
        };
    }
  }

  if (e.startsWith("!") && e.length > 1) {
    const needle = lower.slice(1);
    return (v) => !text(v).toLowerCase().includes(needle);
  }
  const starts = e.startsWith("^");
  const ends = e.endsWith("$") && e.length > 1;
  if (starts || ends) {
    const needle = lower.slice(starts ? 1 : 0, ends ? -1 : undefined);
    return (v) => {
      if (v === null || v === undefined) return false;
      const t = text(v).toLowerCase();
      return starts && ends ? t === needle : starts ? t.startsWith(needle) : t.endsWith(needle);
    };
  }
  return (v) => v !== null && v !== undefined && text(v).toLowerCase().includes(lower);
}

/** `null` when the expression is empty (no filter). */
export function compileFilter(expr: string): CellTest | null {
  const parts = expr
    .split(/\s+\|\s+/)
    .map(compileOne)
    .filter((t): t is CellTest => t !== null);
  if (parts.length === 0) return null;
  return parts.length === 1 ? parts[0] : (v) => parts.some((t) => t(v));
}
