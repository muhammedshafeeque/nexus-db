import clsx from "clsx";
import {
  Braces,
  Columns3,
  Database,
  Eye,
  FileKey2,
  FolderTree,
  KeyRound,
  ListTree,
  Network,
  Table2,
  Hash,
  Layers,
} from "lucide-react";
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { flavorOf } from "../lib/engines";
import type { Engine } from "../lib/types";

export function EngineBadge({ engine, flavor, size = 22 }: { engine: Engine; flavor?: string | null; size?: number }) {
  const f = flavorOf(engine, flavor);
  return (
    <div
      className="grid shrink-0 place-items-center rounded-[7px] font-semibold tracking-tight"
      style={{
        width: size,
        height: size,
        fontSize: size * 0.42,
        color: f.ink ?? "#fff",
        background: `linear-gradient(140deg, color-mix(in oklab, ${f.color} 100%, white 18%), ${f.color} 55%, color-mix(in oklab, ${f.color} 75%, black))`,
        boxShadow: `inset 0 1px 0 rgba(255,255,255,.25), 0 2px 8px -2px ${f.color}88`,
      }}
      title={f.name}
    >
      {f.short}
    </div>
  );
}

export function Kbd({ children }: { children: ReactNode }) {
  return (
    <kbd className="rounded-[5px] border border-line-strong bg-panel-2 px-1.5 py-px font-mono text-[10.5px] text-muted">
      {children}
    </kbd>
  );
}

export function IconButton({
  children,
  title,
  onClick,
  active,
  className,
  disabled,
}: {
  children: ReactNode;
  title?: string;
  onClick?: (e: React.MouseEvent) => void;
  active?: boolean;
  className?: string;
  disabled?: boolean;
}) {
  return (
    <button
      title={title}
      disabled={disabled}
      onClick={onClick}
      className={clsx(
        "grid h-7 w-7 shrink-0 place-items-center rounded-md text-muted transition-colors hover:bg-hover hover:text-fg disabled:opacity-40 disabled:hover:bg-transparent",
        active && "bg-accent-soft text-accent",
        className,
      )}
    >
      {children}
    </button>
  );
}

export function Spinner({ className }: { className?: string }) {
  return (
    <svg className={clsx("animate-spin", className ?? "h-3.5 w-3.5")} viewBox="0 0 24 24" fill="none">
      <circle cx="12" cy="12" r="9" stroke="currentColor" strokeOpacity=".2" strokeWidth="3" />
      <path d="M21 12a9 9 0 0 0-9-9" stroke="currentColor" strokeWidth="3" strokeLinecap="round" />
    </svg>
  );
}

const KIND_ICONS: Record<string, { icon: typeof Database; color: string }> = {
  database: { icon: Database, color: "#a78bfa" },
  schema: { icon: FolderTree, color: "#60a5fa" },
  table: { icon: Table2, color: "#22d3ee" },
  view: { icon: Eye, color: "#f472b6" },
  collection: { icon: Layers, color: "#34d399" },
  edge: { icon: Network, color: "#fbbf24" },
  column: { icon: Columns3, color: "var(--faint)" },
  pk: { icon: KeyRound, color: "#fbbf24" },
  index: { icon: ListTree, color: "var(--faint)" },
  key: { icon: FileKey2, color: "#f87171" },
  folder: { icon: Braces, color: "var(--muted)" },
};

export function NodeIcon({ kind, className }: { kind: string; className?: string }) {
  const k = KIND_ICONS[kind] ?? { icon: Hash, color: "var(--muted)" };
  const Icon = k.icon;
  return <Icon className={clsx("shrink-0", className ?? "h-3.5 w-3.5")} style={{ color: k.color }} strokeWidth={2} />;
}

export function Toggle({ checked, onChange, label, hint }: { checked: boolean; onChange: (v: boolean) => void; label: string; hint?: string }) {
  return (
    <label className="flex cursor-pointer items-start gap-3 rounded-lg px-1 py-1.5">
      <button
        type="button"
        role="switch"
        aria-checked={checked}
        onClick={() => onChange(!checked)}
        className={clsx(
          "relative mt-0.5 h-[18px] w-8 shrink-0 rounded-full transition-colors",
          checked ? "bg-accent" : "bg-line-strong",
        )}
      >
        <span
          className={clsx(
            "absolute top-0.5 left-0.5 h-3.5 w-3.5 rounded-full bg-white shadow transition-transform",
            checked && "translate-x-3.5",
          )}
        />
      </button>
      <span className="leading-tight">
        <span className="block text-[12.5px] text-fg">{label}</span>
        {hint && <span className="text-[11.5px] text-faint">{hint}</span>}
      </span>
    </label>
  );
}

// ---------------------------------------------------------------- context menu

export interface MenuItem {
  label: string;
  icon?: ReactNode;
  shortcut?: string;
  danger?: boolean;
  onSelect: () => void;
}

export type MenuEntry = MenuItem | "separator";

interface MenuState {
  x: number;
  y: number;
  items: MenuEntry[];
}

let openMenu: ((m: MenuState | null) => void) | null = null;

export function showContextMenu(e: React.MouseEvent, items: MenuEntry[]) {
  e.preventDefault();
  e.stopPropagation();
  openMenu?.({ x: e.clientX, y: e.clientY, items });
}

export function ContextMenuHost() {
  const [menu, setMenu] = useState<MenuState | null>(null);
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ x: 0, y: 0 });

  useEffect(() => {
    openMenu = setMenu;
    return () => {
      openMenu = null;
    };
  }, []);

  useLayoutEffect(() => {
    if (!menu || !ref.current) return;
    const r = ref.current.getBoundingClientRect();
    setPos({
      x: Math.min(menu.x, window.innerWidth - r.width - 8),
      y: Math.min(menu.y, window.innerHeight - r.height - 8),
    });
  }, [menu]);

  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(null);
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && close();
    window.addEventListener("mousedown", close);
    window.addEventListener("blur", close);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("blur", close);
      window.removeEventListener("keydown", onKey);
    };
  }, [menu]);

  if (!menu) return null;
  return createPortal(
    <div
      ref={ref}
      className="animate-pop fixed z-[100] min-w-[200px] rounded-xl border border-line bg-elev p-1 shadow-pop backdrop-blur"
      style={{ left: pos.x, top: pos.y }}
      onMouseDown={(e) => e.stopPropagation()}
      onContextMenu={(e) => e.preventDefault()}
    >
      {menu.items.map((item, i) =>
        item === "separator" ? (
          <div key={i} className="my-1 h-px bg-line" />
        ) : (
          <button
            key={i}
            onClick={() => {
              setMenu(null);
              item.onSelect();
            }}
            className={clsx(
              "flex w-full items-center gap-2.5 rounded-lg px-2.5 py-1.5 text-left text-[12.5px] transition-colors",
              item.danger ? "text-danger hover:bg-danger/10" : "text-fg hover:bg-accent-soft",
            )}
          >
            <span className="grid w-4 place-items-center text-muted">{item.icon}</span>
            <span className="flex-1">{item.label}</span>
            {item.shortcut && <span className="text-[11px] text-faint">{item.shortcut}</span>}
          </button>
        ),
      )}
    </div>,
    document.body,
  );
}
