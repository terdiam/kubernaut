import { useEffect, useState } from "react";
import { api } from "../api";
import { bytes, cores } from "../format";
import type { Recommendation, SizingReport } from "../types";

interface Props {
  cluster: string;
  namespace: string;
  resource: string;
  name: string;
}

const CONFIDENCE_LABEL: Record<Recommendation["confidence"], string> = {
  reasonable: "Reasonable",
  indicative: "Indicative",
  insufficient: "Not enough data",
};

/**
 * Request and limit suggestions from observed usage.
 *
 * Observation starts when this panel opens, so the first look shows almost
 * nothing. That is stated rather than hidden: a number derived from ninety
 * seconds of traffic looks exactly like one derived from a week, and only one
 * of them is worth acting on.
 */
export function SizingPane({ cluster, namespace, resource, name }: Props) {
  const [report, setReport] = useState<SizingReport | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;

    const refresh = () =>
      void api
        .workloadSizing(cluster, namespace, resource, name)
        .then((result) => !cancelled && setReport(result))
        .catch((err) => !cancelled && setError(String(err)));

    refresh();
    // Matches the sampling interval; each tick adds one observation.
    const id = window.setInterval(refresh, 15_000);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, [cluster, namespace, resource, name]);

  if (error) return <p className="error drawer__body">{error}</p>;
  if (!report) return <p className="muted drawer__body">Starting observation…</p>;
  if (report.note) return <p className="muted drawer__body">{report.note}</p>;

  return (
    <div className="sizing">
      {report.recommendations.map((recommendation) => (
        <section key={recommendation.container} className="sizing__block">
          <header className="sizing__head">
            <h4>{recommendation.container}</h4>
            <span className={`chip chip--${confidenceTone(recommendation.confidence)}`}>
              {CONFIDENCE_LABEL[recommendation.confidence]}
            </span>
          </header>

          {recommendation.confidence !== "insufficient" && (
            <table className="sizing__table">
              <thead>
                <tr>
                  <th />
                  <th>Observed p95</th>
                  <th>Observed peak</th>
                  <th>Current</th>
                  <th>Suggested</th>
                </tr>
              </thead>
              <tbody>
                <tr>
                  <th>CPU request</th>
                  <td>{cores(recommendation.cpuP95)}</td>
                  <td>{cores(recommendation.cpuMax)}</td>
                  <td>{value(recommendation.currentCpuRequest, cores)}</td>
                  <td className="sizing__suggested">
                    {cores(recommendation.recommendedCpuRequest)}
                  </td>
                </tr>
                <tr>
                  <th>CPU limit</th>
                  <td colSpan={2} className="muted">
                    —
                  </td>
                  <td>{value(recommendation.currentCpuLimit, cores)}</td>
                  <td className="muted">not suggested</td>
                </tr>
                <tr>
                  <th>Memory request</th>
                  <td>{bytes(recommendation.memoryP95)}</td>
                  <td>{bytes(recommendation.memoryMax)}</td>
                  <td>{value(recommendation.currentMemoryRequest, bytes)}</td>
                  <td className="sizing__suggested">
                    {bytes(recommendation.recommendedMemoryRequest)}
                  </td>
                </tr>
                <tr>
                  <th>Memory limit</th>
                  <td className="muted">—</td>
                  <td>{bytes(recommendation.memoryMax)}</td>
                  <td>{value(recommendation.currentMemoryLimit, bytes)}</td>
                  <td className="sizing__suggested">
                    {bytes(recommendation.recommendedMemoryLimit)}
                  </td>
                </tr>
              </tbody>
            </table>
          )}

          <ul className="sizing__notes">
            {recommendation.notes.map((note) => (
              <li key={note}>{note}</li>
            ))}
          </ul>

          {recommendation.confidence !== "insufficient" && (
            <details className="sizing__yaml">
              <summary>YAML to paste into the pod template</summary>
              <pre>{snippet(recommendation)}</pre>
            </details>
          )}
        </section>
      ))}

      <p className="muted sizing__footer">
        {report.pods} pod{report.pods === 1 ? "" : "s"} observed. Samples accumulate while this
        panel is open and are kept for six hours.
      </p>
    </div>
  );
}

function confidenceTone(confidence: Recommendation["confidence"]): string {
  return confidence === "reasonable" ? "ok" : confidence === "indicative" ? "warn" : "error";
}

function value(amount: number, format: (value: number) => string): string {
  return amount > 0 ? format(amount) : "not set";
}

/** Millicores and mebibytes, the units people actually write in manifests. */
function snippet(recommendation: Recommendation): string {
  const milli = (value: number) => `${Math.round(value * 1000)}m`;
  const mib = (value: number) => `${Math.round(value / 1024 / 1024)}Mi`;

  return [
    `# container: ${recommendation.container}`,
    "resources:",
    "  requests:",
    `    cpu: ${milli(recommendation.recommendedCpuRequest)}`,
    `    memory: ${mib(recommendation.recommendedMemoryRequest)}`,
    "  limits:",
    `    memory: ${mib(recommendation.recommendedMemoryLimit)}`,
    "    # no cpu limit: see the notes above",
  ].join("\n");
}
