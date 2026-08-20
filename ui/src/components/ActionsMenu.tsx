import { useState } from "react";
import { api } from "../api";
import type { ResourceDescriptor, Row } from "../types";

interface Props {
  cluster: string;
  resource: ResourceDescriptor;
  row: Row;
  onForward: () => void;
  onDone: () => void;
}

type Pending =
  | { kind: "scale"; current: number }
  | { kind: "delete" }
  | { kind: "drain" }
  | null;

const SCALABLE = new Set(["Deployment", "StatefulSet", "ReplicaSet"]);

/** Row actions. Everything destructive routes through a typed confirmation. */
export function ActionsMenu({ cluster, resource, row, onForward, onDone }: Props) {
  const [pending, setPending] = useState<Pending>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<string | null>(null);

  const target = {
    resource: resource.key,
    namespace: row.namespace,
    name: row.name,
  };

  const run = async (action: () => Promise<unknown>, done: string) => {
    setBusy(true);
    setError(null);
    try {
      await action();
      setStatus(done);
      setPending(null);
      onDone();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const isNode = resource.kind === "Node";
  const isPod = resource.kind === "Pod";
  const scalable = SCALABLE.has(resource.kind);

  return (
    <div className="actions">
      <div className="actions__row">
        {scalable && (
          <button
            className="button"
            onClick={() =>
              void api
                .currentScale(cluster, target)
                .then((current) => setPending({ kind: "scale", current }))
                .catch((err) => setError(String(err)))
            }
          >
            Scale
          </button>
        )}
        {scalable && (
          <button
            className="button"
            title="Rolls pods by touching the pod template, exactly like kubectl rollout restart"
            onClick={() => void run(() => api.restartWorkload(cluster, target), "Restart triggered.")}
          >
            Restart
          </button>
        )}
        {(isPod || resource.kind === "Service" || scalable) && (
          <button className="button" onClick={onForward}>
            Port forward
          </button>
        )}
        {isNode && (
          <>
            <button
              className="button"
              onClick={() =>
                void run(() => api.setNodeCordoned(cluster, row.name, true), "Node cordoned.")
              }
            >
              Cordon
            </button>
            <button
              className="button"
              onClick={() =>
                void run(() => api.setNodeCordoned(cluster, row.name, false), "Node uncordoned.")
              }
            >
              Uncordon
            </button>
            <button className="button button--danger" onClick={() => setPending({ kind: "drain" })}>
              Drain
            </button>
          </>
        )}
        {resource.deletable && (
          <button className="button button--danger" onClick={() => setPending({ kind: "delete" })}>
            Delete
          </button>
        )}
      </div>

      {status && <p className="muted">{status}</p>}
      {error && <p className="error">{error}</p>}

      {pending?.kind === "scale" && (
        <ScaleDialog
          current={pending.current}
          name={row.name}
          busy={busy}
          onCancel={() => setPending(null)}
          onConfirm={(replicas) =>
            void run(
              () => api.scaleWorkload(cluster, target, replicas),
              `Scaled to ${replicas}.`,
            )
          }
        />
      )}

      {pending?.kind === "delete" && (
        <ConfirmDialog
          title={`Delete ${resource.kind}`}
          name={row.name}
          detail={`${cluster} · ${row.namespace ?? "cluster-scoped"}`}
          verb="Delete"
          busy={busy}
          onCancel={() => setPending(null)}
          onConfirm={(confirmation) =>
            void run(
              () =>
                api.deleteObject(cluster, {
                  ...target,
                  confirmation,
                  propagation: "background",
                  gracePeriodSeconds: null,
                }),
              "Deleted.",
            )
          }
        />
      )}

      {pending?.kind === "drain" && (
        <ConfirmDialog
          title="Drain node"
          name={row.name}
          detail={`${cluster} — evicts pods and cordons the node. DaemonSet and static pods stay.`}
          verb="Drain"
          busy={busy}
          onCancel={() => setPending(null)}
          onConfirm={(confirmation) =>
            void run(async () => {
              const report = await api.drainNode(cluster, row.name, {
                confirmation,
                deleteStandalonePods: false,
                dryRun: false,
              });
              setStatus(
                `Evicted ${report.evicted.length}, skipped ${report.skipped.length}, blocked ${report.blocked.length}.`,
              );
              return report;
            }, "Drain finished.")
          }
        />
      )}
    </div>
  );
}

function ScaleDialog({
  current,
  name,
  busy,
  onCancel,
  onConfirm,
}: {
  current: number;
  name: string;
  busy: boolean;
  onCancel: () => void;
  onConfirm: (replicas: number) => void;
}) {
  const [replicas, setReplicas] = useState(current);
  return (
    <div className="modal" role="dialog">
      <div className="modal__card">
        <h3>Scale {name}</h3>
        <div className="field">
          <label className="field__label">Replicas (currently {current})</label>
          <input
            type="number"
            min={0}
            value={replicas}
            onChange={(e) => setReplicas(Number(e.target.value))}
          />
        </div>
        {replicas === 0 && (
          <p className="warning">Scaling to zero stops all traffic to this workload.</p>
        )}
        <div className="modal__actions">
          <button className="button" onClick={onCancel}>
            Cancel
          </button>
          <button
            className="button button--primary"
            disabled={busy}
            onClick={() => onConfirm(replicas)}
          >
            Scale
          </button>
        </div>
      </div>
    </div>
  );
}

function ConfirmDialog({
  title,
  name,
  detail,
  verb,
  busy,
  onCancel,
  onConfirm,
}: {
  title: string;
  name: string;
  detail: string;
  verb: string;
  busy: boolean;
  onCancel: () => void;
  onConfirm: (confirmation: string) => void;
}) {
  const [typed, setTyped] = useState("");
  return (
    <div className="modal" role="dialog">
      <div className="modal__card">
        <h3>{title}</h3>
        <p className="muted">{detail}</p>
        <div className="field">
          <label className="field__label">
            Type <code>{name}</code> to confirm
          </label>
          <input value={typed} onChange={(e) => setTyped(e.target.value)} autoFocus />
        </div>
        <div className="modal__actions">
          <button className="button" onClick={onCancel}>
            Cancel
          </button>
          <button
            className="button button--danger"
            disabled={busy || typed !== name}
            onClick={() => onConfirm(typed)}
          >
            {verb}
          </button>
        </div>
      </div>
    </div>
  );
}
