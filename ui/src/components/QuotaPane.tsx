import { useEffect, useState } from "react";
import { api } from "../api";
import { bytes, cores, count } from "../format";
import { Gauge } from "./Gauge";
import type { NamespaceQuotaInfo, ResourceGauge } from "../types";

interface Props {
  cluster: string;
  namespace: string;
}

/** Cores for CPU, bytes for memory, the bare number otherwise. `null` for
 * anything unparseable rather than guessing. */
function parseQuantity(value: string): number | null {
  const match = /^(-?[0-9.]+)([a-zA-Z]*)$/.exec(value.trim());
  if (!match) return null;
  const number = Number(match[1]);
  if (Number.isNaN(number)) return null;
  const multiplier: Record<string, number> = {
    "": 1,
    m: 1e-3,
    n: 1e-9,
    u: 1e-6,
    k: 1e3,
    M: 1e6,
    G: 1e9,
    T: 1e12,
    Ki: 1024,
    Mi: 1024 ** 2,
    Gi: 1024 ** 3,
    Ti: 1024 ** 4,
  };
  const factor = multiplier[match[2] ?? ""];
  return factor === undefined ? null : number * factor;
}

/** First present key from a priority list, e.g. prefer bare "cpu" over "requests.cpu". */
function firstKey(hard: Record<string, string>, keys: string[]): string | null {
  return keys.find((key) => key in hard) ?? null;
}

function gaugeFor(
  hard: Record<string, string>,
  used: Record<string, string>,
  keys: string[],
): ResourceGauge | null {
  const key = firstKey(hard, keys);
  if (!key) return null;
  const rawHard = hard[key];
  if (rawHard === undefined) return null;
  const hardValue = parseQuantity(rawHard);
  if (hardValue === null) return null;
  const rawUsed = used[key];
  const usedValue = rawUsed !== undefined ? parseQuantity(rawUsed) : null;
  return {
    usage: usedValue ?? 0,
    usageAvailable: usedValue !== null,
    requests: 0,
    limits: 0,
    allocatable: hardValue,
    capacity: hardValue,
  };
}

/** A namespace's own `ResourceQuota`/`LimitRange` objects — declared hard
 * limits and current usage, distinct from the metrics-sampled Heatmap. */
export function QuotaPane({ cluster, namespace }: Props) {
  const [info, setInfo] = useState<NamespaceQuotaInfo | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setInfo(null);
    setError(null);
    api
      .namespaceQuota(cluster, namespace)
      .then((result) => {
        if (!cancelled) setInfo(result);
      })
      .catch((err) => {
        if (!cancelled) setError(String(err));
      });
    return () => {
      cancelled = true;
    };
  }, [cluster, namespace]);

  if (error) return <p className="error drawer__body">{error}</p>;
  if (!info) return <p className="muted drawer__body">Loading…</p>;

  if (info.quotas.length === 0 && info.limitRanges.length === 0) {
    return (
      <p className="muted drawer__body">
        No ResourceQuota or LimitRange set in this namespace.
      </p>
    );
  }

  return (
    <div className="drawer__body">
      {info.quotas.map((quota) => {
        const cpuGauge = gaugeFor(quota.hard, quota.used, ["cpu", "requests.cpu", "limits.cpu"]);
        const memoryGauge = gaugeFor(quota.hard, quota.used, [
          "memory",
          "requests.memory",
          "limits.memory",
        ]);
        const podsGauge = gaugeFor(quota.hard, quota.used, ["pods"]);
        const gauged = new Set(
          [
            cpuGauge && firstKey(quota.hard, ["cpu", "requests.cpu", "limits.cpu"]),
            memoryGauge && firstKey(quota.hard, ["memory", "requests.memory", "limits.memory"]),
            podsGauge && "pods",
          ].filter((k): k is string => Boolean(k)),
        );
        const remaining = Object.keys(quota.hard).filter((key) => !gauged.has(key));

        return (
          <section key={quota.name} className="context__block">
            <h3>{quota.name}</h3>
            <div className="overview__gauges">
              {cpuGauge && <Gauge title="CPU" gauge={cpuGauge} format={cores} />}
              {memoryGauge && <Gauge title="Memory" gauge={memoryGauge} format={bytes} />}
              {podsGauge && <Gauge title="Pods" gauge={podsGauge} format={count} />}
            </div>
            {remaining.length > 0 && (
              <dl className="props">
                {remaining.map((key) => (
                  <div className="props__row" key={key}>
                    <dt>{key}</dt>
                    <dd>
                      {quota.used[key] ?? "0"} / {quota.hard[key]}
                    </dd>
                  </div>
                ))}
              </dl>
            )}
          </section>
        );
      })}

      {info.limitRanges.length > 0 && (
        <section className="context__block">
          <h3>Limit range defaults</h3>
          <table className="settings__audit">
            <thead>
              <tr>
                <th>Applies to</th>
                <th>Default limit</th>
                <th>Default request</th>
                <th>Max</th>
                <th>Min</th>
              </tr>
            </thead>
            <tbody>
              {info.limitRanges.map((item, index) => (
                <tr key={`${item.kind}-${index}`}>
                  <td>{item.kind}</td>
                  <td>{Object.entries(item.default).map(([k, v]) => `${k}: ${v}`).join(", ") || "—"}</td>
                  <td>
                    {Object.entries(item.defaultRequest)
                      .map(([k, v]) => `${k}: ${v}`)
                      .join(", ") || "—"}
                  </td>
                  <td>{Object.entries(item.max).map(([k, v]) => `${k}: ${v}`).join(", ") || "—"}</td>
                  <td>{Object.entries(item.min).map(([k, v]) => `${k}: ${v}`).join(", ") || "—"}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </section>
      )}
    </div>
  );
}
