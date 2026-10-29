import clsx from "clsx";
import { ArrowLeft, CheckCircle2, Eye, EyeOff, FolderOpen, Link2, LogOut, Search, ShieldCheck, SlidersHorizontal, X, XCircle, Zap } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import { api, errorText, isTauri } from "../lib/api";
import {
  AUTH_METHODS,
  AZURE_CLOUDS,
  CONNECTION_COLORS,
  defaultEntraScope,
  EMPTY_ENTRA,
  ENGINE_META,
  entraSupport,
  FLAVORS,
  flavorOf,
  type Flavor,
} from "../lib/engines";
import type { ConnectionConfig, EntraOptions } from "../lib/types";
import { newConnection, useStore } from "../store";
import { EngineBadge, Spinner, Toggle } from "./ui";

const CATEGORIES: Flavor["category"][] = ["Relational", "Document", "Graph", "Key-value", "Analytics", "Search"];

export function ConnectionDialog() {
  const { open, editing } = useStore((s) => s.dialog);
  const closeDialog = useStore((s) => s.closeDialog);
  const [draft, setDraft] = useState<ConnectionConfig | null>(null);

  useEffect(() => {
    if (open) setDraft(editing);
  }, [open, editing]);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && closeDialog();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, closeDialog]);

  if (!open) return null;
  return (
    <div className="animate-overlay fixed inset-0 z-50 grid place-items-center bg-black/55 p-6 backdrop-blur-[3px]" onMouseDown={closeDialog}>
      <div
        onMouseDown={(e) => e.stopPropagation()}
        className="animate-pop flex max-h-[min(760px,92vh)] w-[min(820px,100%)] flex-col overflow-hidden rounded-2xl border border-line bg-panel shadow-pop"
      >
        {draft ? (
          <ConnectionForm draft={draft} setDraft={setDraft} onBack={editing?.id ? undefined : () => setDraft(null)} />
        ) : (
          <EnginePicker onPick={(f) => setDraft(newConnection(f.id))} onClose={closeDialog} />
        )}
      </div>
    </div>
  );
}

function EnginePicker({ onPick, onClose }: { onPick: (f: Flavor) => void; onClose: () => void }) {
  const [q, setQ] = useState("");
  const list = FLAVORS.filter((f) => !q || f.name.toLowerCase().includes(q.toLowerCase()) || f.category.toLowerCase().includes(q.toLowerCase()));
  return (
    <>
      <div className="flex items-center gap-3 border-b border-line px-5 py-4">
        <div>
          <h2 className="text-[16px] font-semibold">Connect to a database</h2>
          <p className="text-[12.5px] text-muted">Pick an engine — wire-compatible databases share a driver.</p>
        </div>
        <div className="ml-auto flex items-center gap-2 rounded-lg border border-line bg-panel-2 px-2.5 py-1.5 focus-within:border-accent/60">
          <Search className="h-3.5 w-3.5 text-faint" />
          <input autoFocus value={q} onChange={(e) => setQ(e.target.value)} placeholder="Search engines" className="w-40 bg-transparent text-[12.5px] outline-none placeholder:text-faint" />
        </div>
        <button onClick={onClose} className="grid h-8 w-8 place-items-center rounded-lg text-muted hover:bg-hover hover:text-fg">
          <X className="h-4 w-4" />
        </button>
      </div>
      <div className="overflow-auto p-5">
        {CATEGORIES.map((cat) => {
          const items = list.filter((f) => f.category === cat);
          if (!items.length) return null;
          return (
            <div key={cat} className="mb-5 last:mb-0">
              <div className="mb-2 text-[11px] font-semibold tracking-[0.08em] text-faint uppercase">{cat}</div>
              <div className="grid grid-cols-3 gap-2">
                {items.map((f) => (
                  <button
                    key={f.id}
                    onClick={() => onPick(f)}
                    className="group flex items-center gap-3 rounded-xl border border-line bg-panel-2 p-3 text-left transition-all hover:-translate-y-px hover:border-[color:var(--hover-c)] hover:shadow-[0_8px_30px_-12px_var(--hover-c)]"
                    style={{ ["--hover-c" as string]: f.color }}
                  >
                    <EngineBadge engine={f.engine} flavor={f.id} size={34} />
                    <div className="min-w-0">
                      <div className="text-[13px] font-medium">{f.name}</div>
                      <div className="line-clamp-2 text-[11px] leading-snug text-faint">{f.blurb}</div>
                    </div>
                  </button>
                ))}
              </div>
            </div>
          );
        })}
      </div>
    </>
  );
}

function Field({ label, children, className }: { label: string; children: React.ReactNode; className?: string }) {
  return (
    <label className={clsx("block", className)}>
      <span className="mb-1 block text-[11.5px] font-medium text-muted">{label}</span>
      {children}
    </label>
  );
}

const inputCls =
  "h-9 w-full rounded-lg border border-line bg-panel-2 px-3 text-[13px] text-fg outline-none transition-colors placeholder:text-faint focus:border-accent/70 focus:ring-3 focus:ring-accent/15";

function ConnectionForm({
  draft,
  setDraft,
  onBack,
}: {
  draft: ConnectionConfig;
  setDraft: (c: ConnectionConfig) => void;
  onBack?: () => void;
}) {
  const connections = useStore((s) => s.connections);
  const { saveConnection, closeDialog, toggleNode, expanded } = useStore.getState();
  const f = flavorOf(draft.engine, draft.flavor);
  const meta = ENGINE_META[draft.engine];
  const [mode, setMode] = useState<"params" | "url">(draft.url ? "url" : "params");
  const [test, setTest] = useState<{ state: "idle" | "running" | "ok" | "fail"; text?: string }>({ state: "idle" });
  const [saving, setSaving] = useState(false);
  const groups = useMemo(() => [...new Set(connections.map((c) => c.group).filter(Boolean))], [connections]);

  const set = (patch: Partial<ConnectionConfig>) => {
    setDraft({ ...draft, ...patch });
    setTest({ state: "idle" });
  };

  const prepared = (): ConnectionConfig => ({
    ...draft,
    name: draft.name.trim() || defaultName(draft),
    url: mode === "url" ? draft.url : "",
  });

  const runTest = async () => {
    setTest({ state: "running" });
    try {
      const r = await api.testConnection(prepared());
      setTest({ state: "ok", text: `${r.version} · ${r.elapsedMs} ms` });
    } catch (e) {
      setTest({ state: "fail", text: errorText(e) });
    }
  };

  const save = async (connect: boolean) => {
    setSaving(true);
    try {
      const saved = await saveConnection(prepared());
      closeDialog();
      toast.success(`Saved ${saved.name}`);
      if (connect && !expanded[saved.id]) void toggleNode(saved.id, []);
    } catch (e) {
      toast.error("Could not save", { description: errorText(e) });
    } finally {
      setSaving(false);
    }
  };

  const browse = async () => {
    if (!isTauri) return;
    const { open } = await import("@tauri-apps/plugin-dialog");
    const path = await open({ multiple: false, filters: [{ name: "SQLite", extensions: ["db", "sqlite", "sqlite3", "db3", "*"] }] });
    if (typeof path === "string") set({ filePath: path, name: draft.name || path.split(/[\\/]/).pop() || "" });
  };

  return (
    <>
      <div className="relative flex items-center gap-3 overflow-hidden border-b border-line px-5 py-4">
        <div className="pointer-events-none absolute inset-0 opacity-[0.12]" style={{ background: `radial-gradient(500px 120px at 0% 0%, ${f.color}, transparent)` }} />
        {onBack && (
          <button onClick={onBack} className="relative grid h-8 w-8 place-items-center rounded-lg text-muted hover:bg-hover hover:text-fg">
            <ArrowLeft className="h-4 w-4" />
          </button>
        )}
        <EngineBadge engine={draft.engine} flavor={draft.flavor} size={36} />
        <div className="relative">
          <h2 className="text-[16px] font-semibold">{draft.id ? "Edit connection" : `New ${f.name} connection`}</h2>
          <p className="text-[12.5px] text-muted">{f.blurb}</p>
        </div>
        {!meta.usesFile && (
          <div className="relative ml-auto flex rounded-lg border border-line bg-panel-2 p-0.5">
            {(["params", "url"] as const).map((m) => (
              <button
                key={m}
                onClick={() => setMode(m)}
                className={clsx("flex items-center gap-1.5 rounded-md px-2.5 py-1 text-[12px]", mode === m ? "bg-elev text-fg shadow-sm" : "text-muted")}
              >
                {m === "params" ? <SlidersHorizontal className="h-3.5 w-3.5" /> : <Link2 className="h-3.5 w-3.5" />}
                {m === "params" ? "Parameters" : "Connection string"}
              </button>
            ))}
          </div>
        )}
      </div>

      <div className="grid grid-cols-[1fr_240px] overflow-auto">
        <div className="space-y-4 p-5">
          <div className="grid grid-cols-2 gap-3">
            <Field label="Name">
              <input autoFocus className={inputCls} value={draft.name} placeholder={defaultName(draft)} onChange={(e) => set({ name: e.target.value })} />
            </Field>
            <Field label="Group">
              <input className={inputCls} list="conn-groups" value={draft.group} placeholder="e.g. Production" onChange={(e) => set({ group: e.target.value })} />
              <datalist id="conn-groups">
                {groups.map((g) => (
                  <option key={g} value={g} />
                ))}
              </datalist>
            </Field>
          </div>

          {meta.usesFile ? (
            <Field label="Database file">
              <div className="flex gap-2">
                <input className={clsx(inputCls, "font-mono text-[12px]")} value={draft.filePath} placeholder="/path/to/database.sqlite" onChange={(e) => set({ filePath: e.target.value })} />
                <button onClick={browse} className="flex h-9 shrink-0 items-center gap-1.5 rounded-lg border border-line bg-panel-2 px-3 text-[12.5px] hover:bg-hover">
                  <FolderOpen className="h-3.5 w-3.5" /> Browse
                </button>
              </div>
              <p className="mt-1 text-[11.5px] text-faint">The file is created if it does not exist.</p>
            </Field>
          ) : mode === "url" ? (
            <Field label="Connection string / URL">
              <textarea
                className={clsx(inputCls, "h-24 resize-none py-2 font-mono text-[12px]")}
                value={draft.url}
                placeholder={meta.urlPlaceholder}
                onChange={(e) => set({ url: e.target.value })}
              />
            </Field>
          ) : null}

          {!meta.usesFile && mode === "url" && <AuthFields draft={draft} set={set} urlMode />}

          {meta.usesFile || mode === "url" ? null : (
            <>
              <div className="grid grid-cols-[1fr_110px] gap-3">
                <Field label="Host">
                  <input className={inputCls} value={draft.host} placeholder="localhost" onChange={(e) => set({ host: e.target.value })} />
                </Field>
                <Field label="Port">
                  <input
                    className={clsx(inputCls, "font-mono")}
                    value={draft.port ?? ""}
                    placeholder={String(f.port ?? "")}
                    onChange={(e) => set({ port: e.target.value ? Number(e.target.value.replace(/\D/g, "")) || null : null })}
                  />
                </Field>
              </div>
              <AuthFields draft={draft} set={set} />
            </>
          )}

          {!meta.usesFile && draft.engine !== "elasticsearch" && (
            <Field label={draft.engine === "redis" ? "Database index" : "Default database"}>
              <input
                className={inputCls}
                value={draft.database}
                placeholder={draft.engine === "redis" ? "0" : f.defaults?.database ?? "optional"}
                onChange={(e) => set({ database: e.target.value })}
              />
            </Field>
          )}

          {test.state !== "idle" && (
            <div
              className={clsx(
                "animate-pop flex items-start gap-2.5 rounded-xl border p-3 text-[12.5px]",
                test.state === "running" && "border-line bg-panel-2 text-muted",
                test.state === "ok" && "border-success/30 bg-success/[0.07] text-success",
                test.state === "fail" && "border-danger/30 bg-danger/[0.07] text-danger",
              )}
            >
              {test.state === "running" ? <Spinner className="mt-0.5 h-4 w-4" /> : test.state === "ok" ? <CheckCircle2 className="mt-0.5 h-4 w-4 shrink-0" /> : <XCircle className="mt-0.5 h-4 w-4 shrink-0" />}
              <div className="min-w-0">
                <div className="font-medium">{test.state === "running" ? "Testing connection…" : test.state === "ok" ? "Connected successfully" : "Connection failed"}</div>
                {test.text && <div className="selectable mt-0.5 font-mono text-[11.5px] break-words whitespace-pre-wrap opacity-90">{test.text}</div>}
              </div>
            </div>
          )}
        </div>

        <div className="space-y-1 border-l border-line bg-panel-2/50 p-4">
          <div className="mb-1 text-[11px] font-semibold tracking-[0.08em] text-faint uppercase">Options</div>
          {!meta.usesFile && mode === "params" && (
            <>
              <Toggle checked={draft.ssl} onChange={(ssl) => set({ ssl })} label="Use SSL / TLS" hint="Encrypt the connection" />
              <Toggle checked={draft.trustCert} onChange={(trustCert) => set({ trustCert })} label="Trust server certificate" hint="Allow self-signed certs" />
            </>
          )}
          <Toggle checked={draft.readOnly} onChange={(readOnly) => set({ readOnly })} label="Read-only" hint="Block statements that modify data" />
          <div className="pt-4">
            <div className="mb-2 text-[11px] font-semibold tracking-[0.08em] text-faint uppercase">Color</div>
            <div className="flex flex-wrap gap-2">
              {CONNECTION_COLORS.map((c) => (
                <button
                  key={c || "none"}
                  onClick={() => set({ color: c })}
                  className={clsx(
                    "h-6 w-6 rounded-full border-2 transition-transform hover:scale-110",
                    draft.color === c ? "border-fg" : "border-transparent",
                    !c && "bg-[repeating-linear-gradient(45deg,var(--line-strong)_0_3px,transparent_3px_6px)]",
                  )}
                  style={c ? { background: c } : undefined}
                  title={c || "none"}
                />
              ))}
            </div>
          </div>
        </div>
      </div>

      <div className="flex items-center gap-2 border-t border-line bg-panel-2/40 px-5 py-3">
        <button
          onClick={runTest}
          disabled={test.state === "running"}
          className="flex h-9 items-center gap-2 rounded-lg border border-line-strong px-3.5 text-[12.5px] font-medium hover:bg-hover disabled:opacity-60"
        >
          <Zap className="h-3.5 w-3.5 text-warn" /> Test connection
        </button>
        <div className="flex-1" />
        <button onClick={closeDialog} className="h-9 rounded-lg px-3.5 text-[12.5px] text-muted hover:bg-hover hover:text-fg">
          Cancel
        </button>
        <button onClick={() => save(false)} disabled={saving} className="h-9 rounded-lg border border-line-strong px-3.5 text-[12.5px] font-medium hover:bg-hover">
          Save
        </button>
        <button onClick={() => save(true)} disabled={saving} className="btn-primary flex h-9 items-center gap-2 rounded-lg px-4 text-[12.5px] font-semibold text-white">
          {saving && <Spinner />} Save & connect
        </button>
      </div>
    </>
  );
}

function defaultName(c: ConnectionConfig) {
  const f = flavorOf(c.engine, c.flavor);
  if (c.engine === "sqlite") return c.filePath.split(/[\\/]/).pop() || f.name;
  return `${f.name} · ${c.host || "localhost"}`;
}

function SecretInput({ value, onChange, placeholder = "••••••••" }: { value: string; onChange: (v: string) => void; placeholder?: string }) {
  const [show, setShow] = useState(false);
  return (
    <div className="relative">
      <input className={clsx(inputCls, "pr-9")} type={show ? "text" : "password"} value={value} placeholder={placeholder} onChange={(e) => onChange(e.target.value)} />
      <button onClick={() => setShow((v) => !v)} className="absolute top-1/2 right-2 -translate-y-1/2 text-faint hover:text-fg">
        {show ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
      </button>
    </div>
  );
}

/** User / password, or one of the Microsoft Entra ID methods. */
function AuthFields({ draft, set, urlMode }: { draft: ConnectionConfig; set: (patch: Partial<ConnectionConfig>) => void; urlMode?: boolean }) {
  const support = entraSupport(draft.engine);
  const auth = draft.auth ?? "password";
  const entra: EntraOptions = { ...EMPTY_ENTRA, ...draft.entra };
  const setEntra = (patch: Partial<EntraOptions>) => set({ entra: { ...entra, ...patch } });
  const method = AUTH_METHODS.find((m) => m.id === auth) ?? AUTH_METHODS[0];
  const userFlow = auth === "entraInteractive" || auth === "entraDeviceCode";
  const servicePrincipal = auth === "entraClientSecret" || auth === "entraCertificate";
  const builtInScope = defaultEntraScope(draft.engine, entra.cloud);
  const [signedOut, setSignedOut] = useState(false);

  const browseCert = async () => {
    if (!isTauri) return;
    const { open } = await import("@tauri-apps/plugin-dialog");
    const path = await open({ multiple: false, filters: [{ name: "PEM certificate + key", extensions: ["pem", "crt", "cer", "key", "*"] }] });
    if (typeof path === "string") setEntra({ certificatePath: path });
  };

  const signOut = async () => {
    try {
      await api.entraSignOut(draft);
      setSignedOut(true);
      toast.success("Signed out", { description: "The next connection will ask you to sign in again." });
    } catch (e) {
      toast.error("Could not sign out", { description: errorText(e) });
    }
  };

  const passwordFields = !urlMode && (
    <div className="grid grid-cols-2 gap-3">
      <Field label="User">
        <input className={inputCls} value={draft.user} placeholder={draft.engine === "redis" ? "(default)" : "username"} onChange={(e) => set({ user: e.target.value })} />
      </Field>
      <Field label="Password">
        <SecretInput value={draft.password} onChange={(password) => set({ password })} />
      </Field>
    </div>
  );

  if (!support) return passwordFields || null;

  // which identity the database sees: Postgres/MySQL roles, Redis object ID, Mongo principal;
  // for other engines a UPN only serves as the account hint of user flows
  const engineUser = ["postgres", "mysql", "mongodb", "redis"].includes(draft.engine);
  const showUser = !urlMode && (engineUser || userFlow);

  return (
    <div className="space-y-3">
      <Field label="Authentication">
        <select
          className={clsx(inputCls, "cursor-pointer")}
          value={auth}
          onChange={(e) => {
            setSignedOut(false);
            const next = e.target.value as ConnectionConfig["auth"];
            // "postgres" / "sa" / "root" defaults are password users, not Entra identities
            const defaultUser = flavorOf(draft.engine, draft.flavor).defaults?.user;
            const user = next !== "password" && auth === "password" && draft.user === defaultUser ? "" : draft.user;
            set({ auth: next, entra, user });
          }}
        >
          {AUTH_METHODS.map((m) => (
            <option key={m.id} value={m.id}>
              {m.label}
            </option>
          ))}
        </select>
        <p className="mt-1 text-[11.5px] text-faint">{method.hint}</p>
      </Field>

      {auth === "password" ? (
        passwordFields
      ) : (
        <div className="space-y-3 rounded-xl border border-accent/25 bg-accent/[0.04] p-3">
          <div className="flex items-start gap-2 text-[11.5px] text-muted">
            <ShieldCheck className="mt-px h-3.5 w-3.5 shrink-0 text-accent" />
            <span>{support.note}</span>
          </div>

          {showUser && (
            <Field label={engineUser ? support.userLabel : "Account"}>
              <input
                className={inputCls}
                value={draft.user}
                placeholder={engineUser ? support.userPlaceholder : "name@contoso.com (optional)"}
                onChange={(e) => set({ user: e.target.value })}
              />
            </Field>
          )}

          {auth === "entraAccessToken" && (
            <Field label="Access token">
              <textarea
                className={clsx(inputCls, "h-20 resize-none py-2 font-mono text-[11px] break-all")}
                value={draft.password}
                placeholder={`az account get-access-token --scope ${builtInScope ?? "<scope>"} --query accessToken -o tsv`}
                onChange={(e) => set({ password: e.target.value.trim() })}
              />
            </Field>
          )}

          {(userFlow || servicePrincipal || auth === "entraAzureCli") && (
            <div className="grid grid-cols-2 gap-3">
              <Field label={servicePrincipal ? "Tenant ID (required)" : "Tenant"}>
                <input className={inputCls} value={entra.tenantId} placeholder={servicePrincipal ? "contoso.onmicrosoft.com or GUID" : "organizations"} onChange={(e) => setEntra({ tenantId: e.target.value })} />
              </Field>
              {auth !== "entraAzureCli" && (
                <Field label={servicePrincipal ? "Client ID (required)" : "Client ID"}>
                  <input className={clsx(inputCls, "font-mono text-[12px]")} value={entra.clientId} placeholder={servicePrincipal ? "application (client) ID" : "Azure CLI (default)"} onChange={(e) => setEntra({ clientId: e.target.value })} />
                </Field>
              )}
            </div>
          )}

          {auth === "entraClientSecret" && (
            <Field label="Client secret">
              <SecretInput value={entra.clientSecret} onChange={(clientSecret) => setEntra({ clientSecret })} placeholder="secret value" />
            </Field>
          )}

          {auth === "entraCertificate" && (
            <Field label="Certificate (PEM with private key)">
              <div className="flex gap-2">
                <input className={clsx(inputCls, "font-mono text-[12px]")} value={entra.certificatePath} placeholder="/path/to/app.pem" onChange={(e) => setEntra({ certificatePath: e.target.value })} />
                <button onClick={browseCert} className="flex h-9 shrink-0 items-center gap-1.5 rounded-lg border border-line bg-panel-2 px-3 text-[12.5px] hover:bg-hover">
                  <FolderOpen className="h-3.5 w-3.5" /> Browse
                </button>
              </div>
            </Field>
          )}

          {auth === "entraManagedIdentity" && (
            <Field label="User-assigned identity client ID">
              <input className={clsx(inputCls, "font-mono text-[12px]")} value={entra.identityClientId} placeholder="system-assigned" onChange={(e) => setEntra({ identityClientId: e.target.value })} />
            </Field>
          )}

          <div className="grid grid-cols-[170px_1fr] gap-3">
            <Field label="Cloud">
              <select className={clsx(inputCls, "cursor-pointer")} value={entra.cloud || "public"} onChange={(e) => setEntra({ cloud: e.target.value })}>
                {AZURE_CLOUDS.map((c) => (
                  <option key={c.id} value={c.id}>
                    {c.name}
                  </option>
                ))}
              </select>
            </Field>
            <Field label={builtInScope ? "Scope" : "Scope (required)"}>
              <input
                className={clsx(inputCls, "font-mono text-[12px]", !builtInScope && !entra.scope.trim() && "border-warn/60")}
                value={entra.scope}
                placeholder={builtInScope ?? "api://<app-id>/.default"}
                onChange={(e) => setEntra({ scope: e.target.value })}
              />
            </Field>
          </div>

          {userFlow && isTauri && (
            <button
              onClick={signOut}
              disabled={signedOut}
              className="flex items-center gap-1.5 text-[12px] text-muted hover:text-fg disabled:opacity-50"
            >
              <LogOut className="h-3.5 w-3.5" /> {signedOut ? "Signed out" : "Sign out / use another account"}
            </button>
          )}
        </div>
      )}
    </div>
  );
}
