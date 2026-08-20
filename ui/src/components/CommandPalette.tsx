import { useEffect, useMemo, useRef, useState } from "react";
import { useStore } from "../store";
import type { ResourceDescriptor, Row } from "../types";

type Entry =
  | { kind: "resource"; label: string; sub: string; resource: ResourceDescriptor }
  | { kind: "row"; label: string; sub: string; row: Row }
  | { kind: "cluster"; label: string; sub: string; context: string };

/**
 * ⌘K jump-to. Rows are matched against the reflector store already in memory,
 * so search is instant and costs the apiserver nothing.
 */
export function CommandPalette({ onClose }: { onClose: () => void }) {
  const discovery = useStore((s) => s.discovery);
  const rows = useStore((s) => s.rows);
  const contexts = useStore((s) => s.contexts);
  const selectResource = useStore((s) => s.selectResource);
  const select = useStore((s) => s.select);
  const connect = useStore((s) => s.connect);

  const [query, setQuery] = useState("");
  const [cursor, setCursor] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => inputRef.current?.focus(), []);

  const entries = useMemo<Entry[]>(() => {
    const needle = query.trim().toLowerCase();
    const out: Entry[] = [];

    for (const context of contexts) {
      if (!needle || context.name.toLowerCase().includes(needle)) {
        out.push({
          kind: "cluster",
          label: context.name,
          sub: "cluster",
          context: context.name,
        });
      }
    }

    for (const group of discovery?.groups ?? []) {
      for (const resource of group.resources) {
        if (!resource.watchable || resource.version !== group.preferredVersion) continue;
        const matches =
          !needle ||
          resource.kind.toLowerCase().includes(needle) ||
          resource.plural.toLowerCase().includes(needle);
        if (matches) {
          out.push({
            kind: "resource",
            label: resource.kind,
            sub: group.name,
            resource,
          });
        }
      }
    }

    if (needle) {
      for (const row of rows.values()) {
        if (row.name.toLowerCase().includes(needle)) {
          out.push({
            kind: "row",
            label: row.name,
            sub: row.namespace ?? "cluster-scoped",
            row,
          });
        }
        if (out.length > 200) break;
      }
    }

    return out.slice(0, 60);
  }, [query, discovery, rows, contexts]);

  useEffect(() => setCursor(0), [query]);

  const activate = (entry: Entry | undefined) => {
    if (!entry) return;
    switch (entry.kind) {
      case "cluster":
        void connect(entry.context);
        break;
      case "resource":
        void selectResource(entry.resource);
        break;
      case "row":
        select(entry.row);
        break;
    }
    onClose();
  };

  return (
    <div className="modal modal--top" onClick={onClose}>
      <div className="palette" onClick={(e) => e.stopPropagation()}>
        <input
          ref={inputRef}
          value={query}
          placeholder="Jump to a cluster, resource or object…"
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(event) => {
            if (event.key === "ArrowDown") {
              event.preventDefault();
              setCursor((c) => Math.min(c + 1, entries.length - 1));
            } else if (event.key === "ArrowUp") {
              event.preventDefault();
              setCursor((c) => Math.max(c - 1, 0));
            } else if (event.key === "Enter") {
              event.preventDefault();
              activate(entries[cursor]);
            } else if (event.key === "Escape") {
              onClose();
            }
          }}
        />
        <ul className="palette__list">
          {entries.map((entry, index) => (
            <li key={`${entry.kind}-${entry.label}-${index}`}>
              <button
                className={`palette__item${index === cursor ? " palette__item--active" : ""}`}
                onMouseEnter={() => setCursor(index)}
                onClick={() => activate(entry)}
              >
                <span className={`palette__kind palette__kind--${entry.kind}`}>{entry.kind}</span>
                <span className="palette__label">{entry.label}</span>
                <span className="muted">{entry.sub}</span>
              </button>
            </li>
          ))}
          {entries.length === 0 && <li className="muted palette__empty">Nothing matches.</li>}
        </ul>
      </div>
    </div>
  );
}
