import { useEffect, useMemo, useRef, useState } from "react";
import { useStore } from "../store";

/**
 * Searchable multi-select namespace filter.
 *
 * A cluster with hundreds of namespaces makes a plain `<select>` unusable, and
 * picking exactly one is the wrong model — related workloads routinely live in
 * two or three namespaces. Empty selection means all namespaces.
 */
export function NamespacePicker() {
  const namespaces = useStore((s) => s.namespaces);
  const selected = useStore((s) => s.selectedNamespaces);
  const setNamespaces = useStore((s) => s.setNamespaces);
  const activeCluster = useStore((s) => s.activeCluster);

  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [cursor, setCursor] = useState(0);
  const root = useRef<HTMLDivElement>(null);
  const search = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (!open) return;
    search.current?.focus();
    const onClick = (event: MouseEvent) => {
      if (!root.current?.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", onClick);
    return () => document.removeEventListener("mousedown", onClick);
  }, [open]);

  const matches = useMemo(() => {
    const needle = query.trim().toLowerCase();
    const list = needle
      ? namespaces.filter((ns) => ns.toLowerCase().includes(needle))
      : namespaces;
    // Selected namespaces float to the top so they stay visible while filtering.
    return [...list].sort((a, b) => {
      const rank = (ns: string) => (selected.includes(ns) ? 0 : 1);
      return rank(a) - rank(b) || a.localeCompare(b);
    });
  }, [namespaces, query, selected]);

  useEffect(() => setCursor(0), [query, open]);

  const toggle = (ns: string) => {
    const next = selected.includes(ns)
      ? selected.filter((entry) => entry !== ns)
      : [...selected, ns];
    void setNamespaces(next);
  };

  const allSelected = selected.length === 0;
  const label = allSelected
    ? "Select Namespace"
    : selected.length === 1
      ? selected[0]!
      : `${selected.length} namespaces`;

  return (
    <div className="ns" ref={root}>
      <button
        className={`ns__trigger${selected.length > 0 ? " ns__trigger--active" : ""}`}
        disabled={!activeCluster}
        onClick={() => setOpen((value) => !value)}
        title={selected.length > 0 ? selected.join(", ") : "All namespaces"}
      >
        <span className="ns__label">{label}</span>
        <span className="ns__caret">⌄</span>
      </button>

      {open && (
        <div className="ns__menu">
          <input
            ref={search}
            className="ns__search"
            value={query}
            placeholder={`Search ${namespaces.length} namespaces`}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(event) => {
              if (event.key === "ArrowDown") {
                event.preventDefault();
                setCursor((c) => Math.min(c + 1, matches.length - 1));
              } else if (event.key === "ArrowUp") {
                event.preventDefault();
                setCursor((c) => Math.max(c - 1, 0));
              } else if (event.key === "Enter") {
                event.preventDefault();
                const target = matches[cursor];
                if (target) toggle(target);
              } else if (event.key === "Escape") {
                setOpen(false);
              }
            }}
          />

          <ul className="ns__list">
            {/* Clearing the selection *is* "all namespaces", so this row is a
                checkbox like the others rather than a separate reset button. */}
            <li>
              <button
                className={`ns__item${allSelected ? " ns__item--on" : ""}`}
                onClick={() => void setNamespaces([])}
              >
                <span className={`ns__box${allSelected ? " ns__box--on" : ""}`} aria-hidden />
                All namespaces
              </button>
            </li>

            {matches.map((ns, index) => {
              const on = selected.includes(ns);
              return (
                <li key={ns}>
                  <button
                    className={`ns__item${index === cursor ? " ns__item--cursor" : ""}${
                      on ? " ns__item--on" : ""
                    }`}
                    onMouseEnter={() => setCursor(index)}
                    onClick={() => toggle(ns)}
                  >
                    <span className={`ns__box${on ? " ns__box--on" : ""}`} aria-hidden />
                    {ns}
                  </button>
                </li>
              );
            })}

            {matches.length === 0 && <li className="muted ns__empty">No namespace matches.</li>}
          </ul>

          {query.trim() !== "" && matches.length > 1 && (
            <div className="ns__actions">
              <button
                className="button button--ghost"
                onClick={() => void setNamespaces([...new Set([...selected, ...matches])])}
              >
                Select all {matches.length} shown
              </button>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
