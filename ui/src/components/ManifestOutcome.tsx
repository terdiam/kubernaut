import { useState } from "react";
import type { DocPlan, DocResult, ManifestPlan } from "../types";

/**
 * How a plan and its results are rendered.
 *
 * Shared by both dialogs: importing a file and creating one object end in the
 * same place — a per-document verdict from the apiserver — and showing them
 * differently would only make the two flows look less related than they are.
 */
const ACTION_TONE: Record<string, string> = {
  create: "ok",
  created: "ok",
  update: "pending",
  configured: "pending",
  unchanged: "unknown",
  conflict: "warn",
  error: "error",
};

export function PlanTable({ plan }: { plan: ManifestPlan }) {
  return (
    <section className="manifest__plan">
      <h4>
        Plan
        <span className="muted"> · nothing has been written yet</span>
      </h4>
      <ul className="manifest__docs">
        {plan.docs.map((doc) => (
          <PlanRow key={doc.index} doc={doc} />
        ))}
      </ul>
    </section>
  );
}

function PlanRow({ doc }: { doc: DocPlan }) {
  const [open, setOpen] = useState(false);

  return (
    <li className={`manifest__doc manifest__doc--${doc.action}`}>
      <div className="manifest__dochead">
        <span className={`chip chip--${ACTION_TONE[doc.action] ?? "unknown"}`}>{doc.action}</span>
        <strong>{doc.kind || "?"}</strong>
        <span>{doc.name || "(unnamed)"}</span>
        <span className="muted">{doc.namespace ?? "cluster-scoped"}</span>
        {doc.unified && (
          <button className="dx-prompt__link" onClick={() => setOpen((value) => !value)}>
            {open ? "hide diff" : "show diff"}
          </button>
        )}
      </div>

      {doc.error && <p className="error manifest__docmsg">{doc.error}</p>}

      {doc.conflicts.length > 0 && (
        <p className="manifest__docmsg">
          Owned by {[...new Set(doc.conflicts.map((c) => c.manager))].join(", ")}:{" "}
          {doc.conflicts.map((c) => c.field).filter(Boolean).join(", ")}. Enable Force to take
          ownership.
        </p>
      )}

      {doc.warnings.map((warning) => (
        <p key={warning} className="manifest__docmsg manifest__docmsg--warn">
          {warning}
        </p>
      ))}

      {open && doc.unified && (
        <pre className="diff__body">
          {doc.unified.split("\n").map((line, index) => (
            <span
              key={index}
              className={
                line.startsWith("+")
                  ? "diff__add"
                  : line.startsWith("-")
                    ? "diff__del"
                    : line.startsWith("@@")
                      ? "diff__hunk"
                      : undefined
              }
            >
              {line}
              {"\n"}
            </span>
          ))}
        </pre>
      )}
    </li>
  );
}

export function ResultTable({ results }: { results: DocResult[] }) {
  const failed = results.filter((entry) => entry.status === "error" || entry.status === "conflict");

  return (
    <section className="manifest__plan">
      <h4>
        Applied
        {failed.length > 0 && (
          <span className="muted"> · {failed.length} of {results.length} did not go through</span>
        )}
      </h4>
      <ul className="manifest__docs">
        {results.map((entry) => (
          <li key={entry.index} className={`manifest__doc manifest__doc--${entry.status}`}>
            <div className="manifest__dochead">
              <span className={`chip chip--${ACTION_TONE[entry.status] ?? "unknown"}`}>
                {entry.status}
              </span>
              <strong>{entry.kind}</strong>
              <span>{entry.name}</span>
              <span className="muted">{entry.namespace ?? "cluster-scoped"}</span>
            </div>
            {entry.error && <p className="error manifest__docmsg">{entry.error}</p>}
            {entry.conflicts.length > 0 && (
              <p className="manifest__docmsg">
                Owned by {[...new Set(entry.conflicts.map((c) => c.manager))].join(", ")}. Enable
                Force and apply again to take those fields.
              </p>
            )}
          </li>
        ))}
      </ul>
    </section>
  );
}
