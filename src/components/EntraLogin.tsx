import { Copy, ExternalLink, KeyRound, X } from "lucide-react";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { api, errorText, isTauri } from "../lib/api";
import type { EntraLoginEvent } from "../lib/types";
import { Spinner } from "./ui";

/** Shows a pending Microsoft Entra ID sign-in (browser or device code) above everything else. */
export function EntraLogin() {
  const [event, setEvent] = useState<EntraLoginEvent | null>(null);

  useEffect(() => {
    if (!isTauri) return;
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void import("@tauri-apps/api/event").then(({ listen }) =>
      listen<EntraLoginEvent>("entra-login", (e) => setEvent(e.payload.state === "done" ? null : e.payload)).then((u) => {
        if (cancelled) u();
        else unlisten = u;
      }),
    );
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  if (!event || event.state === "done") return null;

  const copy = async (text: string, what: string) => {
    try {
      await navigator.clipboard.writeText(text);
      toast.success(`${what} copied`);
    } catch (e) {
      toast.error("Could not copy", { description: errorText(e) });
    }
  };
  const open = (url: string) => api.openUrl(url).catch((e) => toast.error("Could not open the browser", { description: errorText(e) }));

  return (
    <div className="animate-overlay fixed inset-0 z-[60] grid place-items-center bg-black/45 p-6 backdrop-blur-[2px]">
      <div className="animate-pop w-[min(460px,100%)] rounded-2xl border border-line bg-panel p-5 shadow-pop">
        <div className="mb-3 flex items-center gap-3">
          <div className="grid h-9 w-9 place-items-center rounded-xl bg-accent/15 text-accent">
            <KeyRound className="h-4.5 w-4.5" />
          </div>
          <div className="min-w-0 flex-1">
            <h2 className="text-[15px] font-semibold">Sign in with Microsoft</h2>
            <p className="text-[12px] text-muted">Microsoft Entra ID</p>
          </div>
          <button onClick={() => void api.entraCancel()} title="Cancel sign-in" className="grid h-8 w-8 place-items-center rounded-lg text-muted hover:bg-hover hover:text-fg">
            <X className="h-4 w-4" />
          </button>
        </div>

        {event.state === "browser" ? (
          <>
            <p className="text-[13px] leading-relaxed text-muted">
              Finish signing in in the browser window that just opened. Nexus DB continues automatically afterwards.
            </p>
            <div className="mt-4 flex flex-wrap gap-2">
              <button onClick={() => open(event.url)} className="flex h-8 items-center gap-1.5 rounded-lg border border-line-strong px-3 text-[12.5px] hover:bg-hover">
                <ExternalLink className="h-3.5 w-3.5" /> Open browser again
              </button>
              <button onClick={() => copy(event.url, "Sign-in link")} className="flex h-8 items-center gap-1.5 rounded-lg border border-line px-3 text-[12.5px] text-muted hover:bg-hover hover:text-fg">
                <Copy className="h-3.5 w-3.5" /> Copy link
              </button>
            </div>
          </>
        ) : (
          <>
            <p className="text-[13px] leading-relaxed text-muted">
              Open <span className="selectable font-mono text-fg">{event.url}</span> on any device and enter this code:
            </p>
            <button
              onClick={() => copy(event.code, "Code")}
              title="Copy code"
              className="selectable mt-3 flex w-full items-center justify-center gap-3 rounded-xl border border-line-strong bg-panel-2 py-3 font-mono text-[26px] font-semibold tracking-[0.18em] hover:bg-hover"
            >
              {event.code}
              <Copy className="h-4 w-4 text-faint" />
            </button>
            <div className="mt-4 flex gap-2">
              <button
                onClick={() => {
                  void copy(event.code, "Code");
                  void open(event.url);
                }}
                className="btn-primary flex h-8 items-center gap-1.5 rounded-lg px-3 text-[12.5px] font-semibold text-white"
              >
                <ExternalLink className="h-3.5 w-3.5" /> Copy code & open
              </button>
            </div>
          </>
        )}

        <div className="mt-4 flex items-center gap-2 border-t border-line pt-3 text-[12px] text-faint">
          <Spinner className="h-3.5 w-3.5" /> Waiting for sign-in…
          <button onClick={() => void api.entraCancel()} className="ml-auto text-muted hover:text-fg">
            Cancel
          </button>
        </div>
      </div>
    </div>
  );
}
