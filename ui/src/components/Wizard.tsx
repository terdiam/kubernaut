import { useState } from "react";
import type { Section } from "../formSpec";
import { getPath } from "../path";
import { FieldRow } from "./FormEditor";

/**
 * A create form's sections, one at a time.
 *
 * The same `Section[]` layout that `FormEditor` lays out flat — every field
 * open at once — is walked here as steps instead. Creating a Deployment means
 * touching a dozen fields across three unrelated concerns (rollout, the pod
 * template, metadata); one long page hides which fields matter for the step
 * someone is actually on. Nothing about the data changes: this is the same
 * `draft`, the same `onChange`, the same fields — only how much of the form is
 * visible at once.
 */
export function Wizard({
  sections,
  draft,
  onChange,
}: {
  sections: Section[];
  draft: Record<string, unknown>;
  onChange: (path: string, value: unknown) => void;
}) {
  const [step, setStep] = useState(0);
  // A kind change mid-session (rare, but `key`-less callers exist) could leave
  // `step` pointing past the end of a shorter layout — clamp rather than crash.
  const current = Math.min(step, sections.length - 1);
  const section = sections[current];
  const isFirst = current === 0;
  const isLast = current === sections.length - 1;

  if (!section) return null;

  return (
    <div className="wizard">
      <ol className="wizard__rail">
        {sections.map((entry, index) => (
          <li key={entry.title}>
            <button
              type="button"
              className={`wizard__step${index === current ? " wizard__step--active" : ""}${
                index < current ? " wizard__step--done" : ""
              }`}
              onClick={() => setStep(index)}
            >
              <span className="wizard__num">{index < current ? "✓" : index + 1}</span>
              {entry.title}
            </button>
          </li>
        ))}
      </ol>

      <div className="wizard__panel">
        <fieldset className="form__section wizard__section">
          <legend>{section.title}</legend>
          {section.description && <p className="muted form__note">{section.description}</p>}
          {section.fields.map((field) => (
            <FieldRow
              key={field.path}
              field={field}
              value={getPath(draft, field.path)}
              onChange={onChange}
            />
          ))}
        </fieldset>

        <div className="wizard__nav">
          <button className="button" onClick={() => setStep(current - 1)} disabled={isFirst}>
            ← Back
          </button>
          <span className="muted wizard__count">
            Step {current + 1} of {sections.length}
          </span>
          <button className="button" onClick={() => setStep(current + 1)} disabled={isLast}>
            Next →
          </button>
        </div>
      </div>
    </div>
  );
}
