import { useStore } from "../store";
import { middleTruncate } from "../truncate";
import type { Issue } from "../types";

/** The cluster's current problems, each row opening the object it is about. */
export function IssuePanel({ issues }: { issues: Issue[] }) {
  // Navigate by resource key rather than by looking the row up in the table
  // that is currently open: the overview watches no table, so a lookup found
  // nothing and every row rendered disabled.
  const openObject = useStore((s) => s.openObject);

  if (issues.length === 0) {
    return (
      <section className="issues issues--none">
        <div className="issues__tick">✓</div>
        <strong>No issues found</strong>
        <p className="muted">Everything is fine in the cluster.</p>
      </section>
    );
  }

  const errors = issues.filter((i) => i.severity === "error").length;

  return (
    <section className="issues">
      <header className="issues__head">
        <strong>
          {issues.length} issue{issues.length === 1 ? "" : "s"}
        </strong>
        {errors > 0 && <span className="pill pill--unreachable">{errors} error</span>}
      </header>
      <ul className="issues__list">
        {issues.map((issue, index) => {
          const full = `${issue.namespace ? `${issue.namespace}/` : ""}${issue.name}`;
          return (
            <li key={`${issue.kind}-${issue.namespace}-${issue.name}-${index}`}>
              <button
                className={`issues__item issues__item--${issue.severity}`}
                title={`${full}\n${issue.message}\n\nClick to open`}
                onClick={() => void openObject(issue.resource, issue.namespace, issue.name)}
              >
                <span className="issues__kind">{issue.kind}</span>
                <span className="issues__name">{middleTruncate(full, 46)}</span>
                <span className="issues__message">{issue.message}</span>
              </button>
            </li>
          );
        })}
      </ul>
    </section>
  );
}
