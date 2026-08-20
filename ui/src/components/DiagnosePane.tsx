import { useEffect, useState } from "react";
import { api } from "../api";
import { useStore } from "../store";
import type { Diagnosis, DiagnosticFinding, DiagnosticStep, DiagnosisReport, StepAction } from "../types";

interface Props {
  cluster: string;
  resource: string;
  namespace: string;
  name: string;
  /** Bumped when the object is reloaded, to re-run the diagnosis. */
  revision: number;
  /**
   * Carry out an action the drawer owns — switching to Logs on a specific
   * container, opening a shell, opening the editor. `open` is handled here
   * because it navigates away from this object entirely.
   */
  onAction: (action: StepAction) => void;
}

/**
 * Why a pod is not running, and what to do next.
 *
 * Every finding quotes the cluster verbatim before it advises anything: the
 * reason string, the exit code, the scheduler's own message. Advice that
 * cannot be checked against the evidence beside it is advice nobody should
 * act on during an incident.
 */
export function DiagnosePane({ cluster, resource, namespace, name, revision, onAction }: Props) {
  const [report, setReport] = useState<DiagnosisReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);

    api
      .diagnose(cluster, resource, namespace, name)
      .then((result) => !cancelled && setReport(result))
      .catch((err) => !cancelled && setError(String(err)))
      .finally(() => !cancelled && setLoading(false));

    return () => {
      cancelled = true;
    };
  }, [cluster, resource, namespace, name, revision]);

  if (loading && !report) return <p className="muted drawer__body">Reading the cluster…</p>;
  if (error) return <p className="error drawer__body">{error}</p>;
  if (!report) return null;

  return (
    <div className="drawer__body dx">
      <header className="dx__head">
        <p className="muted">
          {report.examined} pod{report.examined === 1 ? "" : "s"} examined ·{" "}
          {report.healthy} healthy · {report.pods.length} with findings
          {report.truncated ? " · more pods exist than were examined" : ""}
        </p>
      </header>

      {report.pods.length === 0 ? (
        <p className="dx__clear">
          Nothing to act on. Every pod examined is running with all containers ready.
        </p>
      ) : (
        report.pods.map((diagnosis) => (
          <PodBlock
            key={diagnosis.pod}
            diagnosis={diagnosis}
            single={report.examined === 1}
            onAction={onAction}
          />
        ))
      )}
    </div>
  );
}

function PodBlock({
  diagnosis,
  single,
  onAction,
}: {
  diagnosis: Diagnosis;
  /** A pod opened directly needs no repeat of its own name. */
  single: boolean;
  onAction: (action: StepAction) => void;
}) {
  return (
    <section className="dx__pod">
      {!single && (
        <h3 className="dx__podname">
          {diagnosis.pod}
          <span className="muted"> · {diagnosis.phase}</span>
        </h3>
      )}
      {diagnosis.findings.map((finding, index) => (
        <FindingCard
          key={`${finding.code}-${finding.container ?? ""}-${index}`}
          finding={finding}
          onAction={onAction}
        />
      ))}
    </section>
  );
}

function FindingCard({
  finding,
  onAction,
}: {
  finding: DiagnosticFinding;
  onAction: (action: StepAction) => void;
}) {
  return (
    <article className={`dx-finding dx-finding--${finding.severity}`}>
      <header className="dx-finding__head">
        <span className={`chip chip--${chipTone(finding.severity)}`}>
          {finding.code}
        </span>
        <h4>{finding.title}</h4>
      </header>

      <p className="dx-finding__explain">{finding.explanation}</p>

      {finding.evidence.filter(Boolean).length > 0 && (
        <ul className="dx-finding__evidence">
          {finding.evidence.filter(Boolean).map((line, index) => (
            <li key={index}>{line}</li>
          ))}
        </ul>
      )}

      <ol className="dx-finding__steps">
        {finding.steps.map((step, index) => (
          <StepRow key={index} step={step} onAction={onAction} />
        ))}
      </ol>
    </article>
  );
}

function StepRow({
  step,
  onAction,
}: {
  step: DiagnosticStep;
  onAction: (action: StepAction) => void;
}) {
  const openObject = useStore((s) => s.openObject);
  const [copied, setCopied] = useState(false);

  const copy = async () => {
    if (!step.command) return;
    try {
      await navigator.clipboard.writeText(step.command);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      // Clipboard access can be refused; the command is on screen either way.
    }
  };

  const run = () => {
    const action = step.action;
    if (!action) return;
    if (action.kind === "open") {
      void openObject(action.resource, action.namespace, action.name);
      return;
    }
    onAction(action);
  };

  return (
    <li className="dx-step">
      <p className="dx-step__text">{step.text}</p>
      <div className="dx-step__actions">
        {step.action && (
          <button className="button dx-step__go" onClick={run}>
            {actionLabel(step.action)}
          </button>
        )}
        {step.command && (
          <button
            className="dx-step__command"
            onClick={() => void copy()}
            title="Copy to clipboard"
          >
            <code>{step.command}</code>
            <span className="muted">{copied ? "copied" : "copy"}</span>
          </button>
        )}
      </div>
    </li>
  );
}

/** The chip palette has no `warning` or `info`; map onto what exists. */
function chipTone(severity: string): string {
  if (severity === "error") return "error";
  if (severity === "warning") return "warn";
  return "pending";
}

function actionLabel(action: StepAction): string {
  switch (action.kind) {
    case "logs":
      return action.previous ? "Open previous logs" : "Open logs";
    case "terminal":
      return "Open a shell";
    case "edit":
      return "Edit";
    case "open":
      return `Open ${action.name}`;
  }
}
