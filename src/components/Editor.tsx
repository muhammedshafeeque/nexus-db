import { javascript } from "@codemirror/lang-javascript";
import { MSSQL, MySQL, PostgreSQL, SQLDialect, SQLite, StandardSQL, sql } from "@codemirror/lang-sql";
import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { Prec, type Extension } from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";
import { tags as t } from "@lezer/highlight";
import CodeMirror from "@uiw/react-codemirror";
import { useMemo, useRef } from "react";
import { ENGINE_META } from "../lib/engines";
import type { Engine } from "../lib/types";

const AQL = SQLDialect.define({
  keywords:
    "for in filter return sort limit let collect into with insert update replace remove upsert options aggregate keep count graph shortest_path outbound inbound any all none prune search distinct asc desc like not and or true false null at least",
  builtin:
    "length count sum min max average avg concat contains document collections keys values merge unset has first last unique sorted append push date_now date_iso8601 to_string to_number is_null lower upper substring starts_with",
});

const REDIS = SQLDialect.define({
  keywords:
    "get set del exists expire ttl keys scan type hget hset hgetall hdel hkeys hvals lpush rpush lpop rpop lrange llen sadd srem smembers sscan zadd zrem zrange zrevrange zscore zcard incr decr incrby mget mset info dbsize ping config flushdb flushall xadd xrange xlen object memory usage persist rename unlink append strlen setex json.get json.set publish client slowlog withscores count match",
});

export const highlight = HighlightStyle.define([
  { tag: [t.keyword, t.operatorKeyword, t.modifier], color: "var(--accent)", fontWeight: "600" },
  { tag: [t.string, t.special(t.string)], color: "var(--c-str)" },
  { tag: [t.number, t.bool, t.null], color: "var(--c-num)" },
  { tag: [t.comment, t.lineComment, t.blockComment], color: "var(--faint)", fontStyle: "italic" },
  { tag: [t.function(t.variableName), t.standard(t.name), t.typeName], color: "var(--accent-2)" },
  { tag: [t.propertyName, t.attributeName], color: "var(--c-date)" },
  { tag: [t.operator, t.punctuation, t.bracket], color: "var(--muted)" },
  { tag: [t.special(t.name), t.variableName], color: "var(--fg)" },
]);

export const theme = EditorView.theme({
  "&": { backgroundColor: "transparent", color: "var(--fg)" },
  ".cm-content": { caretColor: "var(--accent)", padding: "12px 0" },
  ".cm-cursor": { borderLeftColor: "var(--accent)", borderLeftWidth: "2px" },
  ".cm-gutters": { backgroundColor: "transparent", color: "var(--faint)", border: "none", paddingLeft: "6px" },
  ".cm-activeLineGutter": { backgroundColor: "transparent", color: "var(--fg)" },
  ".cm-activeLine": { backgroundColor: "var(--hover)" },
  "&.cm-focused .cm-selectionBackground, .cm-selectionBackground, ::selection": {
    backgroundColor: "color-mix(in oklab, var(--accent) 28%, transparent) !important",
  },
  ".cm-matchingBracket": { backgroundColor: "var(--accent-soft)", outline: "1px solid var(--accent)" },
  ".cm-tooltip": { backgroundColor: "var(--elev)", color: "var(--fg)" },
  ".cm-tooltip-autocomplete > ul > li[aria-selected]": { backgroundColor: "var(--accent-soft)", color: "var(--fg)" },
  ".cm-completionDetail": { color: "var(--faint)" },
  ".cm-placeholder": { color: "var(--faint)" },
});

function language(engine: Engine, schema: Record<string, string[]>): Extension {
  switch (ENGINE_META[engine].language) {
    case "mongo":
    case "es":
      return javascript();
    case "aql":
      return sql({ dialect: AQL });
    case "redis":
      return sql({ dialect: REDIS });
    default: {
      const dialect =
        engine === "postgres" ? PostgreSQL : engine === "mysql" ? MySQL : engine === "mssql" ? MSSQL : engine === "sqlite" ? SQLite : StandardSQL;
      return sql({ dialect, schema, upperCaseKeywords: true });
    }
  }
}

export function Editor({
  value,
  onChange,
  engine,
  schema,
  onRun,
}: {
  value: string;
  onChange: (v: string) => void;
  engine: Engine;
  schema: Record<string, string[]>;
  onRun: (selection: string | null) => void;
}) {
  const runRef = useRef(onRun);
  runRef.current = onRun;

  const extensions = useMemo(
    () => [
      language(engine, schema),
      syntaxHighlighting(highlight),
      theme,
      EditorView.lineWrapping,
      Prec.highest(
        keymap.of([
          {
            key: "Mod-Enter",
            run: (view) => {
              const sel = view.state.sliceDoc(view.state.selection.main.from, view.state.selection.main.to);
              runRef.current(sel.trim() ? sel : null);
              return true;
            },
          },
        ]),
      ),
    ],
    [engine, schema],
  );

  return (
    <CodeMirror
      value={value}
      onChange={onChange}
      extensions={extensions}
      theme="none"
      height="100%"
      className="h-full"
      placeholder="Write a query…  (Ctrl/⌘ + Enter to run)"
      basicSetup={{ highlightActiveLine: true, foldGutter: false, autocompletion: true, bracketMatching: true }}
    />
  );
}
