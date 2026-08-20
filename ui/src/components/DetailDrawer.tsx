import { Suspense, lazy, useEffect, useMemo, useState } from "react";
import { api } from "../api";
import { useStore } from "../store";
import { formSections } from "../formSpec";
import { hasBinaryData, secretFromForm, secretToForm } from "../secret";
import { ActionsMenu } from "./ActionsMenu";
import { DiagnosePane } from "./DiagnosePane";
import { FormEditor } from "./FormEditor";
import { ForwardDialog } from "./ForwardsPanel";
import { LogsPane } from "./LogsPane";
import { ObjectContext } from "./ObjectContext";
import { ObjectMetricsPane } from "./ObjectMetricsPane";
import { SizingPane } from "./SizingPane";
import type { LogTarget, MetricTarget, StepAction } from "../types";

// xterm and Monaco are the two heavy dependencies; neither is needed until the
// user actually opens those tabs.
const TerminalPane = lazy(() =>
  import("./TerminalPane").then((m) => ({ default: m.TerminalPane })),
);
const YamlEditor = lazy(() => import("./YamlEditor").then((m) => ({ default: m.YamlEditor })));

type Tab = "overview" | "diagnose" | "form" | "yaml" | "metrics" | "sizing" | "logs" | "terminal";

/** Kinds with a pod template, where sizing advice applies. */
const SIZEABLE = new Set(["Deployment", "StatefulSet", "DaemonSet", "ReplicaSet", "Job"]);

/** Kinds that own pods, so their logs can be tailed across all replicas. */
const POD_OWNERS = new Set([
  "Deployment",
  "StatefulSet",
  "DaemonSet",
  "ReplicaSet",
  "Job",
  "Service",
]);

export function DetailDrawer() {
  const selected = useStore((s) => s.selected);
  const resource = useStore((s) => s.resource);
  const cluster = useStore((s) => s.activeCluster);
  const select = useStore((s) => s.select);

  const [tab, setTab] = useState<Tab>("overview");
  const [yaml, setYaml] = useState<string | null>(null);
  const [json, setJson] = useState<Record<string, unknown> | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [reload, setReload] = useState(0);
  const [forwarding, setForwarding] = useState(false);
  // Set when a diagnosis step opens the Logs tab on one container. The counter
  // forces a remount so a second step with a different preset actually applies.
  const [logsPreset, setLogsPreset] = useState<{
    container: string | null;
    previous: boolean;
    seq: number;
  } | null>(null);

  const isPod = resource?.kind === "Pod";
  const isSecret = resource?.kind === "Secret" && resource.group === "";
  const canTailLogs = isPod || (resource ? POD_OWNERS.has(resource.kind) : false);
  const hasForm = resource ? formSections(resource.group, resource.kind) !== null : false;

  useEffect(() => {
    setTab("overview");
    setLogsPreset(null);
  }, [selected?.uid]);

  // Esc closes the drawer, but must not fire while a dialog is open or while
  // typing — Esc inside Monaco and inside the palette means something else.
  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      if (document.querySelector(".modal")) return;
      const target = event.target as HTMLElement | null;
      if (target?.closest("input, textarea, .monaco-editor, .xterm")) return;
      select(null);
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [select]);

  useEffect(() => {
    if (!selected || !resource || !cluster) {
      setYaml(null);
      setJson(null);
      return;
    }
    let cancelled = false;
    setLoading(true);
    setError(null);
    api
      .getObject(cluster, resource.key, selected.namespace, selected.name)
      .then((payload) => {
        if (cancelled) return;
        setYaml(payload.yaml);
        setJson(payload.json as Record<string, unknown>);
      })
      .catch((err) => !cancelled && setError(String(err)))
      .finally(() => !cancelled && setLoading(false));
    return () => {
      cancelled = true;
    };
  }, [selected?.uid, resource?.key, cluster, reload]);

  const formInitial = useMemo(() => {
    if (!json) return null;
    return isSecret ? secretToForm(json) : json;
  }, [json, isSecret]);

  // Anything whose usage can be attributed: a pod, the node it runs on, a
  // namespace, or a workload's pods in aggregate.
  const metricTarget = useMemo<MetricTarget | null>(() => {
    if (!selected || !resource) return null;
    if (isPod && selected.namespace) {
      return { kind: "pod", namespace: selected.namespace, name: selected.name };
    }
    if (resource.kind === "Node") return { kind: "node", name: selected.name };
    if (resource.kind === "Namespace") return { kind: "namespace", name: selected.name };
    if (POD_OWNERS.has(resource.kind) && selected.namespace) {
      return {
        kind: "workload",
        namespace: selected.namespace,
        resource: resource.key,
        name: selected.name,
      };
    }
    return null;
  }, [selected?.uid, resource?.key, isPod]);

  const logTarget = useMemo<LogTarget | null>(() => {
    if (!selected || !resource || !selected.namespace) return null;
    return isPod
      ? { kind: "pod", namespace: selected.namespace, name: selected.name }
      : {
          kind: "workload",
          namespace: selected.namespace,
          resource: resource.key,
          name: selected.name,
        };
  }, [selected?.uid, resource?.key, isPod]);

  if (!selected || !resource || !cluster) return null;

  const wide = tab !== "overview";
  const tabs: { id: Tab; label: string; enabled: boolean }[] = [
    { id: "overview", label: "Overview", enabled: true },
    {
      id: "diagnose",
      label: "Diagnose",
      // Pods, and the workloads whose pods carry the failure.
      enabled: (isPod || POD_OWNERS.has(resource.kind)) && Boolean(selected.namespace),
    },
    { id: "form", label: "Form", enabled: hasForm && resource.editable },
    { id: "yaml", label: "YAML", enabled: true },
    { id: "metrics", label: "Metrics", enabled: metricTarget !== null },
    {
      id: "sizing",
      label: "Sizing",
      enabled: SIZEABLE.has(resource.kind) && Boolean(selected.namespace),
    },
    { id: "logs", label: "Logs", enabled: canTailLogs && Boolean(selected.namespace) },
    {
      id: "terminal",
      label: "Terminal",
      // Pods get a shell; nodes get a node shell; everything else still gets
      // the local kubectl shell pinned to this cluster.
      enabled: true,
    },
  ];

  /** Carry out a step a finding suggested, inside the drawer. */
  const runDiagnosisStep = (action: StepAction) => {
    switch (action.kind) {
      case "logs":
        setLogsPreset({
          container: action.container,
          previous: action.previous,
          seq: (logsPreset?.seq ?? 0) + 1,
        });
        setTab("logs");
        break;
      case "terminal":
        setTab("terminal");
        break;
      case "edit":
        setTab(hasForm && resource.editable ? "form" : "yaml");
        break;
      // `open` navigates to another object and is handled by the pane itself.
      case "open":
        break;
    }
  };

  return (
    <aside className={`drawer${wide ? " drawer--wide" : ""}`}>
      <header className="drawer__head">
        <div>
          <h2>{selected.name}</h2>
          <p className="muted">
            {resource.kind}
            {selected.namespace ? ` · ${selected.namespace}` : ""} · {resource.apiVersion}
          </p>
        </div>
        <button
          className="drawer__close"
          onClick={() => select(null)}
          title="Close (Esc)"
          aria-label="Close details"
        >
          ✕
        </button>
      </header>

      <nav className="drawer__tabs">
        {tabs
          .filter((entry) => entry.enabled)
          .map((entry) => (
            <button
              key={entry.id}
              className={`tab${tab === entry.id ? " tab--active" : ""}`}
              onClick={() => setTab(entry.id)}
            >
              {entry.label}
            </button>
          ))}
      </nav>

      {error && <p className="error drawer__body">{error}</p>}
      {loading && !yaml && <p className="muted drawer__body">Loading…</p>}

      {tab === "overview" && (
        <div className="drawer__body drawer__overview">
          <ActionsMenu
            cluster={cluster}
            resource={resource}
            row={selected}
            onForward={() => setForwarding(true)}
            onDone={() => setReload((n) => n + 1)}
          />

          <dl className="facts">
            <dt>Status</dt>
            <dd className={`health-text health-text--${selected.health}`}>{selected.health}</dd>
            <dt>Created</dt>
            <dd>{selected.created ?? "—"}</dd>
            <dt>Resource version</dt>
            <dd>{selected.resourceVersion ?? "—"}</dd>
            {selected.terminating && (
              <>
                <dt>Terminating</dt>
                <dd className="warning-text">
                  A deletion is in progress; a stuck finalizer keeps the object alive.
                </dd>
              </>
            )}
          </dl>

          {(isPod || POD_OWNERS.has(resource.kind)) &&
            selected.namespace &&
            selected.health !== "ok" && (
              <p className="dx-prompt">
                {selected.health === "pending"
                  ? "This is not running yet."
                  : "This is not healthy."}{" "}
                <button className="dx-prompt__link" onClick={() => setTab("diagnose")}>
                  Diagnose it
                </button>{" "}
                to see what the cluster says and what to do next.
              </p>
            )}

          {isSecret && json && hasBinaryData(json) && (
            <p className="warning">
              This Secret holds non-text values. They are left untouched by the form and can be
              edited in the YAML tab.
            </p>
          )}

          <ObjectContext
            cluster={cluster}
            resource={resource.key}
            kind={resource.kind}
            namespace={selected.namespace}
            name={selected.name}
            object={json}
            revision={reload}
          />
        </div>
      )}

      {tab === "diagnose" && selected.namespace && (
        <DiagnosePane
          cluster={cluster}
          resource={resource.key}
          namespace={selected.namespace}
          name={selected.name}
          revision={reload}
          onAction={runDiagnosisStep}
        />
      )}

      {tab === "form" && formInitial && (
        <FormEditor
          cluster={cluster}
          resource={resource.key}
          group={resource.group}
          kind={resource.kind}
          namespace={selected.namespace}
          name={selected.name}
          initial={formInitial}
          serialize={isSecret ? secretFromForm : undefined}
          onApplied={() => setReload((n) => n + 1)}
        />
      )}

      {tab === "yaml" && yaml && (
        <Suspense fallback={<p className="muted drawer__body">Loading editor…</p>}>
          <YamlEditor
            cluster={cluster}
            resource={resource.key}
            namespace={selected.namespace}
            name={selected.name}
            initial={yaml}
            onApplied={() => setReload((n) => n + 1)}
          />
        </Suspense>
      )}

      {tab === "metrics" && metricTarget && (
        <ObjectMetricsPane cluster={cluster} target={metricTarget} />
      )}

      {tab === "sizing" && selected.namespace && (
        <SizingPane
          cluster={cluster}
          namespace={selected.namespace}
          resource={resource.key}
          name={selected.name}
        />
      )}

      {tab === "logs" && logTarget && selected.namespace && (
        <LogsPane
          key={logsPreset?.seq ?? 0}
          cluster={cluster}
          target={logTarget}
          pod={isPod ? selected.name : null}
          namespace={selected.namespace}
          initialContainer={logsPreset?.container ?? null}
          initialPrevious={logsPreset?.previous ?? false}
        />
      )}

      {tab === "terminal" && (
        <Suspense fallback={<p className="muted drawer__body">Loading terminal…</p>}>
          <TerminalPane
            cluster={cluster}
            namespace={selected.namespace ?? undefined}
            pod={isPod ? selected.name : undefined}
            node={resource.kind === "Node" ? selected.name : undefined}
            modes={
              isPod && selected.namespace
                ? ["podExec", "ephemeral", "localShell"]
                : resource.kind === "Node"
                  ? ["nodeShell", "localShell"]
                  : ["localShell"]
            }
          />
        </Suspense>
      )}

      {forwarding && selected.namespace && (
        <ForwardDialog
          cluster={cluster}
          resource={resource.key}
          namespace={selected.namespace}
          name={selected.name}
          onClose={() => setForwarding(false)}
          onStarted={() => useStore.getState().showForwards()}
        />
      )}
    </aside>
  );
}
