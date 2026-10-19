import { useEffect, useState } from "react";
import { getPluginApi, postPluginApi, usePluginI18n } from "../host/runtime";
import { useScreenReady } from "../host/ready";

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

function shortKey(value: string | undefined | null, size = 12): string {
  if (!value) return "";
  return value.length > size * 2 ? `${value.slice(0, size)}…${value.slice(-4)}` : value;
}

function copy(text: string) {
  void navigator.clipboard?.writeText(text).catch(() => {});
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

/** Proves the node key in the browser's profile: Ghostly drafts, the node signs. */
function ProveKey({ didKey, onClose }: { didKey: string; onClose: () => void }) {
  const { t } = usePluginI18n();
  const [statement, setStatement] = useState("");
  const [signature, setSignature] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const sign = async () => {
    setBusy(true);
    setError(null);
    setSignature(null);
    try {
      const answer = await postPluginApi<{ signature: string }>("sign", { statement: statement.trim() });
      setSignature(answer.signature);
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setBusy(false);
    }
  };
  return (
    <section className="pb-panel" aria-label={t("prove.title")}>
      <div className="pb-panel-head">
        <h2>{t("prove.title")}</h2>
        <button className="pb-btn pb-btn-outline pb-btn-small" type="button" onClick={onClose}>{t("close")}</button>
      </div>
      <ol className="pb-howto">
        <li>{t("prove.step1")}</li>
        <li>
          {t("prove.step2")}
          <span className="pb-copyline">
            <code>{didKey}</code>
            <button className="pb-btn pb-btn-outline pb-btn-small" type="button" onClick={() => copy(didKey)}>{t("copy")}</button>
          </span>
        </li>
        <li>{t("prove.step3")}</li>
      </ol>
      <textarea
        className="pb-input"
        rows={3}
        value={statement}
        placeholder={t("prove.placeholder")}
        onChange={(event) => setStatement(event.target.value)}
      />
      <div className="pb-btn-row">
        <button className="pb-btn pb-btn-accent" type="button" disabled={busy || !statement.trim()} onClick={() => void sign()}>
          {busy ? t("prove.signing") : t("prove.sign")}
        </button>
      </div>
      {signature && (
        <p className="pb-copyline">
          <span>{t("prove.step4")}</span>
          <code>{shortKey(signature, 16)}</code>
          <button className="pb-btn pb-btn-accent pb-btn-small" type="button" onClick={() => copy(signature)}>{t("copy")}</button>
        </p>
      )}
      {error && <p className="pb-error" role="alert">{error}</p>}
    </section>
  );
}

export function Ghostly() {
  const { t } = usePluginI18n();
  const { state, failed } = useNodeState();
  const [address, setAddress] = useState(WEB_BASE);
  const [proving, setProving] = useState(false);
  const installed = state !== null && state.version !== null && state.stage !== "installing";
  useScreenReady(state !== null || failed);

  const members = state?.group?.members ?? [];
  const nodes = members.filter((member) => member.node?.node_id).length;
  const memberReady = state?.stage === "ready" && state.group?.status === "active";
  const link = state?.group?.link ?? null;
  const didKey = state?.node.did_key ?? null;

  return (
    <main className="pb-page pb-ghostly-page">
      <header className="pb-bar">
        <p className="pb-eyebrow"><span className="pb-tick" aria-hidden="true" />{t("eyebrow", { version: state?.version ?? "" })}</p>
        <ul className="pb-chips">
          <li className={`pb-chip${memberReady ? " pb-chip-ok" : ""}`} title={state?.error ?? undefined}>
            {t("chip.member")} <strong>{memberReady ? t("chip.inGroup") : t(`stage.${state?.stage ?? "waiting"}`)}</strong>
          </li>
          {state?.group && (
            <li className="pb-chip">{t("chip.members")} <strong>{t("chip.nodes", { nodes, total: members.length })}</strong></li>
          )}
          {state?.identity?.proof && (
            <li className="pb-chip pb-chip-ok" title={state.identity.did_key}>{t("chip.identity")} <strong>{shortKey(state.identity.did_key)}</strong></li>
          )}
        </ul>
        <div className="pb-bar-actions">
          {installed && link && (
            <button className="pb-btn pb-btn-accent pb-btn-small" type="button" onClick={() => setAddress(joinAddress(link))}>
              {t("join")}
            </button>
          )}
          {installed && didKey && (
            <button className="pb-btn pb-btn-outline pb-btn-small" type="button" onClick={() => setProving((open) => !open)}>
              {t("prove.open")}
            </button>
          )}
        </div>
      </header>

      {proving && didKey && <ProveKey didKey={didKey} onClose={() => setProving(false)} />}

      {installed ? (
        <iframe
          key={address}
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
