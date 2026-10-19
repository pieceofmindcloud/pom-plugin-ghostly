import { useEffect, useRef, useState } from "react";
import { getPluginApi, usePluginI18n, usePomContext } from "../host/runtime";
import { useScreenReady } from "../host/ready";
import { holdPomTheme } from "./ghostlyTheme";

/** Where the POM serves Ghostly's own web app (the plugin's proxy). */
const WEB_BASE = `/api/ui/plugins/${encodeURIComponent(__POM_PLUGIN_CODE__)}/proxy/web/`;
const POLL_MS = 3000;
const INSTALL_STEPS = ["node", "resolve", "download", "build", "install"] as const;

declare const __POM_PLUGIN_CODE__: string;

type Member = { key: string; me: boolean | null; online: boolean | null; node: { node_id?: string } | null };

export type NodeState = {
  stage: "waiting" | "installing" | "starting" | "joining" | "ready" | "error";
  step: string | null;
  error: string | null;
  version: string | null;
  node: { node_id?: string; hostname?: string; did_key?: string; role?: string };
  identity: { did_key?: string; proof?: string; error?: string } | null;
  group: { id: string; link: string | null; name: string | null; status: string | null; members: Member[] } | null;
};

/** Keeps only what the screen can draw; anything else reads as "no state yet". */
export function normalizeState(value: unknown): NodeState | null {
  if (typeof value !== "object" || value === null) return null;
  const state = value as Partial<NodeState>;
  if (typeof state.stage !== "string") return null;
  const group = state.group && typeof state.group === "object" && Array.isArray(state.group.members) ? state.group : null;
  return {
    stage: state.stage,
    step: typeof state.step === "string" ? state.step : null,
    error: typeof state.error === "string" ? state.error : null,
    version: typeof state.version === "string" ? state.version : null,
    node: typeof state.node === "object" && state.node !== null ? state.node : {},
    identity: typeof state.identity === "object" ? state.identity ?? null : null,
    group: group && { ...group, link: typeof group.link === "string" && /^group2\/[\w-]+\/[\w-]+$/.test(group.link) ? group.link : null },
  };
}

/** The web app's address that opens the join screen of a group's link. */
export function joinAddress(link: string): string {
  return `${WEB_BASE}#/join/${link}`;
}

function useNodeState(): { state: NodeState | null; failed: boolean } {
  const [state, setState] = useState<NodeState | null>(null);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    let active = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const load = () => {
      getPluginApi<unknown>("state")
        .then((value) => {
          if (!active) return;
          const next = normalizeState(value);
          setFailed(next === null);
          if (next) setState(next);
        })
        .catch(() => active && setFailed(true))
        .finally(() => {
          if (active) timer = setTimeout(load, POLL_MS);
        });
    };
    load();
    return () => {
      active = false;
      if (timer) clearTimeout(timer);
    };
  }, []);
  return { state, failed };
}

function Setup({ state, failed }: { state: NodeState | null; failed: boolean }) {
  const { t } = usePluginI18n();
  if (!state) {
    return (
      <section className="pb-state-card" role="status">
        <h2>{failed ? t("state.unreachable") : t("state.loading")}</h2>
      </section>
    );
  }
  const current = INSTALL_STEPS.indexOf((state.step ?? "") as (typeof INSTALL_STEPS)[number]);
  return (
    <section className="pb-state-card" role="status" aria-live="polite">
      <h2>{t(`stage.${state.stage}`)}</h2>
      <p>{t(`stage.${state.stage}.body`)}</p>
      {state.stage === "installing" && (
        <ol className="pb-steps">
          {INSTALL_STEPS.map((step, index) => (
            <li key={step} className={index < current ? "pb-step-done" : index === current ? "pb-step-now" : undefined}>
              {t(`install.${step}`)}
            </li>
          ))}
        </ol>
      )}
      {state.error && <p className="pb-error">{state.error}</p>}
    </section>
  );
}

/** Ghostly needs Web Crypto and Web Locks, which the browser gives only to a
 * secure context: plain http to anything but localhost never runs it. */
function Insecure() {
  const { t } = usePluginI18n();
  const secure = `https://${location.host}${location.pathname}${location.search}${location.hash}`;
  return (
    <section className="pb-state-card" role="alert">
      <h2>{t("insecure.title")}</h2>
      <p>{t("insecure.body")}</p>
      <p className="pb-btn-row">
        <a className="pb-btn pb-btn-accent" href={secure}>{t("insecure.open")}</a>
      </p>
    </section>
  );
}

export function Ghostly() {
  const { state, failed } = useNodeState();
  const [address, setAddress] = useState<string | null>(null);
  const joined = useRef(false);
  const frame = useRef<HTMLIFrameElement>(null);
  const { theme } = usePomContext();
  const installed = state !== null && state.version !== null && state.stage !== "installing";
  const link = state?.group?.link ?? null;
  useScreenReady(state !== null || failed);

  // Straight to the network's group: the app opens on its join screen as soon
  // as the node knows the link, once per visit.
  useEffect(() => {
    if (!installed) return;
    if (link && !joined.current) {
      joined.current = true;
      setAddress(joinAddress(link));
    } else if (!link) {
      setAddress((current) => current ?? WEB_BASE);
    }
  }, [installed, link]);

  // Ghostly follows the POM's light or dark theme, in the POM's colours.
  useEffect(() => {
    if (!frame.current) return;
    return holdPomTheme(frame.current, theme);
  }, [theme, address]);

  return (
    <main className="pb-page pb-ghostly-page">
      {installed && !window.isSecureContext ? (
        <Insecure />
      ) : installed && address ? (
        <iframe
          key={address}
          ref={frame}
          className="pb-frame"
          src={address}
          title="Ghostly"
          allow="camera; microphone; clipboard-read; clipboard-write; autoplay; fullscreen"
        />
      ) : (
        <Setup state={state} failed={failed} />
      )}
    </main>
  );
}
