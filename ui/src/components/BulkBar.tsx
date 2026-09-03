import { useState } from "react";
import { api } from "../api";
import type { BulkOutcome, ResourceDescriptor, Row, TargetRef } from "../types";

/** Kinds `kubectl rollout restart` understands — the ones with a pod template. */
const RESTARTABLE = new Set(["Deployment", "StatefulSet", "DaemonSet"]);

interface Props {
  cluster: string;
  resource: ResourceDescriptor;
  /** Rows currently selected. */
  selected: Row[];
  /** Every row the filter leaves visible, for the "all" actions. */
  visible: Row[];
  onClear: () => void;
  /** The watch delivers deletions on its own; this is for anything it cannot see. */
  onDone: () => void;
}

/**
 * Act on many rows at once.
 *
 * Deleting a set is the one operation here that cannot be undone, so it is
 * confirmed by typing the size of the set — the same deliberate act the
 * single-object dialog asks for, scaled to what is actually at stake. The
 * dialog lists every name so the number can be checked against the objects.
 */
export function BulkBar({ cluster, resource, selected, visible, onClear, onDone }: Props) {
  const [busy, setBusy] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [outcomes, setOutcomes] = useState<BulkOutcome[] | null>(null);
  const [note, setNote] = useState<string | null>(null);

  const targets = (rows: Row[]): TargetRef[] =>
    rows.map((row) => ({
      resource: resource.key,
      namespace: row.namespace,
      name: row.name,
    }));

  const count = selected.length;
  const canRestart = RESTARTABLE.has(resource.kind);

  const download = async (rows: Row[], label: string) => {
    setBusy(true);
    setError(null);
    setOutcomes(null);
    try {
      const result = await api.exportObjects(cluster, targets(rows));
      if (result.exported === 0) {
        setError("Nothing could be read.");
        setOutcomes(result.failed);
        return;
      }
      const blob = new Blob([result.yaml], { type: "application/yaml" });
      const url = URL.createObjectURL(blob);
      const anchor = document.createElement("a");
      anchor.href = url;
      anchor.download = `${resource.plural}-${label}.yaml`;
      anchor.click();
      URL.revokeObjectURL(url);

      setNote(
        `${result.exported} document(s) written` +
          (result.truncated ? ", capped at the export limit" : "") +
          (result.failed.length > 0 ? `, ${result.failed.length} could not be read` : ""),
      );
      if (result.failed.length > 0) setOutcomes(result.failed);
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const restart = async () => {
    setBusy(true);
    setError(null);
    setOutcomes(null);
    try {
      const results = await api.restartWorkloads(cluster, targets(selected));
      report(results, "restarted");
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const remove = async (confirmation: string) => {
    setBusy(true);
    setError(null);
    try {
      const results = await api.deleteObjects(cluster, targets(selected), confirmation);
      setConfirming(false);
      report(results, "deleted");
      onClear();
      onDone();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const report = (results: BulkOutcome[], verb: string) => {
    const failed = results.filter((entry) => !entry.ok);
    setNote(`${results.length - failed.length} of ${results.length} ${verb}`);
    // Only the failures are worth listing; a wall of successes hides them.
    setOutcomes(failed.length > 0 ? failed : null);
  };

  return (
    <div className="bulk">
      <div className="bulk__row">
        {count === 0 ? (
          <span className="muted">
            Select rows to delete, restart or export them.
          </span>
        ) : (
          <strong>{count} selected</strong>
        )}

        {count > 0 && (
          <>
            <button
              className="button button--danger"
              onClick={() => setConfirming(true)}
              disabled={busy || !resource.deletable}
              title={resource.deletable ? undefined : `This cluster does not allow deleting ${resource.kind}`}
            >
              Delete
            </button>
            {canRestart && (
              <button className="button" onClick={() => void restart()} disabled={busy}>
                Restart
              </button>
            )}
            <button
              className="button"
              onClick={() => void download(selected, "selected")}
              disabled={busy}
            >
              Download YAML
            </button>
            <button className="button button--ghost" onClick={onClear} disabled={busy}>
              Clear
            </button>
          </>
        )}

        <button
          className="button button--ghost bulk__all"
          onClick={() => void download(visible, "all")}
          disabled={busy || visible.length === 0}
          title="Every row the current filter leaves visible"
        >
          Download all ({visible.length})
        </button>

        {note && <span className="muted">{note}</span>}
        {error && <span className="error">{error}</span>}
      </div>

      {outcomes && outcomes.length > 0 && (
        <ul className="bulk__failures">
          {outcomes.map((entry) => (
            <li key={`${entry.namespace}-${entry.name}`}>
              <strong>{entry.name}</strong>
              <span className="muted"> {entry.namespace ?? ""}</span> — {entry.error}
            </li>
          ))}
        </ul>
      )}

      {confirming && (
        <ConfirmBulkDelete
          rows={selected}
          kind={resource.kind}
          busy={busy}
          onCancel={() => setConfirming(false)}
          onConfirm={(typed) => void remove(typed)}
        />
      )}
    </div>
  );
}

function ConfirmBulkDelete({
  rows,
  kind,
  busy,
  onCancel,
  onConfirm,
}: {
  rows: Row[];
  kind: string;
  busy: boolean;
  onCancel: () => void;
  onConfirm: (typed: string) => void;
}) {
  const [typed, setTyped] = useState("");
  const expected = String(rows.length);

  return (
    <div className="modal" role="dialog" aria-label="Confirm bulk delete">
      <div className="modal__card modal__card--wide">
        <h3>
          Delete {rows.length} {kind}
          {rows.length === 1 ? "" : "s"}?
        </h3>
        <p className="muted">
          This cannot be undone. Objects a controller owns will be recreated; objects nothing owns
          will not.
        </p>

        {/* The list is the point: the number is only meaningful against it. */}
        <ul className="bulk__list">
          {rows.map((row) => (
            <li key={row.uid}>
              {row.name}
              <span className="muted"> {row.namespace ?? ""}</span>
            </li>
          ))}
        </ul>

        <div className="field">
          <label className="field__label" htmlFor="bulk-confirm">
            Type <code>{expected}</code> to confirm
          </label>
          <input
            id="bulk-confirm"
            value={typed}
            onChange={(e) => setTyped(e.target.value)}
            autoFocus
          />
        </div>

        <div className="modal__actions">
          <button className="button" onClick={onCancel}>
            Cancel
          </button>
          <button
            className="button button--danger"
            disabled={busy || typed !== expected}
            onClick={() => onConfirm(typed)}
          >
            Delete
          </button>
        </div>
      </div>
    </div>
  );
}
