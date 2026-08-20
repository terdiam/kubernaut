import type { UpgradeDiff } from "../types";

/**
 * Diff summary for an install or upgrade.
 *
 * The per-object list comes first because that is the question being asked —
 * "what will this touch" — and a hundred lines of unified diff answers it
 * badly. The text diff stays available underneath.
 */
export function HelmDiff({ diff }: { diff: UpgradeDiff }) {
  if (!diff.changed) {
    return (
      <div className="helm__diff">
        <strong>No changes</strong>
        <p className="muted">The rendered manifest matches what is installed.</p>
      </div>
    );
  }

  const tone = (change: string) =>
    change === "added" ? "ok" : change === "removed" ? "error" : "unknown";

  return (
    <div className="helm__diff">
      <strong>
        {diff.documents.length} object{diff.documents.length === 1 ? "" : "s"} affected
      </strong>

      {diff.generatedOnly && (
        <p className="warning">
          Only regenerated Secret material differs — this chart mints a new certificate or
          password on every render, so applying it would rotate that value but change nothing
          else.
        </p>
      )}

      <ul className="helm__docs">
        {diff.documents.map((document) => (
          <li key={`${document.change}-${document.kind}-${document.name}`}>
            <span className={`chip chip--${tone(document.change)}`}>{document.change}</span>
            <span className="helm__dockind">{document.kind}</span>
            <span className="helm__docname">{document.name}</span>
            {document.generatedOnly && <span className="muted">regenerated value</span>}
          </li>
        ))}
      </ul>

      {diff.unified && (
        <details className="helm__unified">
          <summary>Full diff</summary>
          <pre className="diff__body">
            {diff.unified.split("\n").map((line, index) => (
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
        </details>
      )}
    </div>
  );
}
