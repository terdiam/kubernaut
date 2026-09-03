import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { useStore } from "../store";
import { age } from "../age";
import { useLiveColumns, type LiveCell } from "../liveColumns";
import { toneFor } from "../statusTone";
import { BulkBar } from "./BulkBar";
import { formatDateTime } from "../time";
import type { Row } from "../types";

const ROW_HEIGHT = 34;
const MIN_WIDTH = 64;

type SortKey = { index: number; desc: boolean } | null;

/** Sensible starting widths; the name column carries the longest values. */
function defaultWidths(
  columns: { name: string }[],
  showNamespace: boolean,
  showAbsoluteWidth: number,
): number[] {
  const widths = [280];
  if (showNamespace) widths.push(180);
  for (const column of columns) {
    // A bar plus its percentage needs more room than a short text value.
    widths.push(BAR_COLUMNS.has(column.name) ? 150 : 130);
  }
  // The last column is either a short age or a full timestamp.
  widths.push(showAbsoluteWidth);
  return widths;
}

/** Columns rendered as a usage bar rather than text. */
const BAR_COLUMNS = new Set(["CPU", "Memory", "Disk", "Pods"]);

export function ResourceTable() {
  const rows = useStore((s) => s.rows);
  const spec = useStore((s) => s.spec);
  const resource = useStore((s) => s.resource);
  const filter = useStore((s) => s.filter);
  const select = useStore((s) => s.select);
  const selected = useStore((s) => s.selected);
  const watchState = useStore((s) => s.watchState);
  const cluster = useStore((s) => s.activeCluster);
  const zone = useStore((s) => s.preferences?.timezone ?? "system");
  const showAbsolute = useStore((s) => s.preferences?.showAbsoluteTimes ?? false);

  // Kinds whose interesting numbers are not in the object itself contribute
  // extra columns; today that is nodes and their live usage.
  const live = useLiveColumns(cluster, resource);

  const [sort, setSort] = useState<SortKey>(null);
  const [now, setNow] = useState(() => Date.now());
  const [widths, setWidths] = useState<number[]>([]);
  // Checked rows, by uid. Separate from `selected`, which is the one row the
  // detail drawer is showing.
  const [checked, setChecked] = useState<Set<string>>(new Set());
  // Anchor for shift-click, so a range can be taken without clicking each row.
  const anchor = useRef<string | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const drag = useRef<{ index: number; startX: number; startWidth: number } | null>(null);

  const columns = useMemo(
    () => [...(spec?.columns ?? []), ...live.columns],
    [spec, live.columns],
  );
  const showNamespace = spec?.namespaced ?? false;

  // Age is derived, so one timer beats re-sending rows from Rust.
  useEffect(() => {
    const id = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, []);

  // Reset widths when the column set changes — carrying widths across resource
  // types would size a "Ready" column like a "Message" one.
  useEffect(() => {
    setWidths(defaultWidths(columns, showNamespace, showAbsolute ? 180 : 90));
  }, [resource?.key, columns, showNamespace, showAbsolute]);

  // A selection means nothing once the table shows a different kind.
  useEffect(() => {
    setChecked(new Set());
    anchor.current = null;
  }, [resource?.key]);

  const startResize = useCallback(
    (index: number, event: React.MouseEvent) => {
      event.preventDefault();
      event.stopPropagation();
      drag.current = {
        index,
        startX: event.clientX,
        startWidth: widths[index] ?? 120,
      };

      const onMove = (move: MouseEvent) => {
        const state = drag.current;
        if (!state) return;
        const next = Math.max(MIN_WIDTH, state.startWidth + (move.clientX - state.startX));
        setWidths((current) => {
          const copy = current.slice();
          copy[state.index] = next;
          return copy;
        });
      };
      const onUp = () => {
        drag.current = null;
        window.removeEventListener("mousemove", onMove);
        window.removeEventListener("mouseup", onUp);
        document.body.classList.remove("resizing");
      };

      document.body.classList.add("resizing");
      window.addEventListener("mousemove", onMove);
      window.addEventListener("mouseup", onUp);
    },
    [widths],
  );

  const visible = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    let list = Array.from(rows.values());
    if (needle) {
      list = list.filter(
        (row) =>
          row.name.toLowerCase().includes(needle) ||
          row.namespace?.toLowerCase().includes(needle) ||
          row.cells.some((c) => c.toLowerCase().includes(needle)),
      );
    }
    const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });
    list.sort((a, b) => {
      if (!sort) {
        return (
          collator.compare(a.namespace ?? "", b.namespace ?? "") ||
          collator.compare(a.name, b.name)
        );
      }
      const own = spec?.columns.length ?? 0;
      const value = (row: Row) => {
        if (sort.index === -2) return row.namespace ?? "";
        if (sort.index === -1) return row.name;
        if (sort.index === -3) return row.created ?? "";
        // Live columns sit after the object's own, and their values are
        // computed rather than stored on the row.
        return sort.index >= own
          ? (live.cells(row)[sort.index - own]?.text ?? "")
          : (row.cells[sort.index] ?? "");
      };
      const result = collator.compare(value(a), value(b));
      return sort.desc ? -result : result;
    });
    return list;
  }, [rows, filter, sort, spec, live]);

  const virtualizer = useVirtualizer({
    count: visible.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: 12,
  });

  if (!resource) {
    return (
      <div className="table-empty">
        <p className="muted">Choose a resource from the sidebar.</p>
      </div>
    );
  }

  const toggleSort = (index: number) =>
    setSort((prev) =>
      prev?.index === index ? (prev.desc ? null : { index, desc: true }) : { index, desc: false },
    );
  const indicator = (index: number) => (sort?.index === index ? (sort.desc ? " ▾" : " ▴") : "");

  // Auto-fit: widen to the longest value currently rendered, capped so one
  // enormous message column cannot push everything else off screen.
  const autoFit = (position: number, read: (row: Row) => string) => {
    const longest = visible.reduce((max, row) => Math.max(max, read(row).length), 0);
    setWidths((current) => {
      const copy = current.slice();
      copy[position] = Math.min(640, Math.max(MIN_WIDTH, longest * 7.6 + 28));
      return copy;
    });
  };

  const ownColumns = spec?.columns.length ?? 0;
  // Computing live values per cell keeps the row object untouched, so a watch
  // delta never fights with a metrics refresh.
  const liveCache = new Map<string, LiveCell[]>();
  const liveCell = (row: Row, index: number): LiveCell => {
    let values = liveCache.get(row.uid);
    if (!values) {
      values = live.cells(row);
      liveCache.set(row.uid, values);
    }
    return values[index - ownColumns] ?? { text: "" };
  };
  const cellValue = (row: Row, index: number): string =>
    index < ownColumns ? (row.cells[index] ?? "") : liveCell(row, index).text;

  const checkedRows = visible.filter((row) => checked.has(row.uid));
  const allChecked = visible.length > 0 && checkedRows.length === visible.length;

  const toggleRow = (row: Row, event: React.MouseEvent) => {
    event.stopPropagation();
    setChecked((current) => {
      const next = new Set(current);
      // Shift extends from the last row clicked, which is how selecting every
      // failing pod in a list stays a two-click job rather than forty.
      const from = anchor.current ? visible.findIndex((r) => r.uid === anchor.current) : -1;
      const to = visible.findIndex((r) => r.uid === row.uid);
      if (event.shiftKey && from >= 0 && to >= 0) {
        const [start, end] = from < to ? [from, to] : [to, from];
        const adding = !next.has(row.uid);
        for (let i = start; i <= end; i += 1) {
          const uid = visible[i]?.uid;
          if (!uid) continue;
          if (adding) next.add(uid);
          else next.delete(uid);
        }
      } else if (!next.delete(row.uid)) {
        next.add(row.uid);
      }
      anchor.current = row.uid;
      return next;
    });
  };

  const toggleAll = () =>
    setChecked((current) => {
      if (visible.length > 0 && visible.every((row) => current.has(row.uid))) {
        return new Set();
      }
      // Only what the filter leaves visible: checking rows nobody can see is
      // how a bulk delete surprises someone.
      return new Set(visible.map((row) => row.uid));
    });

  // A fixed first column, kept out of `widths` so the resize handles below
  // keep indexing the columns they already did.
  const template = ["36px", ...widths.map((w) => `${w}px`)].join(" ");
  let position = 0;
  const namePosition = position++;
  const namespacePosition = showNamespace ? position++ : -1;
  const cellStart = position;
  const agePosition = cellStart + columns.length;

  const headerCell = (
    key: string,
    label: string,
    sortIndex: number,
    slot: number,
    read: (row: Row) => string,
    title?: string,
  ) => (
    <div className="th" key={key} title={title}>
      <button className="th__label" onClick={() => toggleSort(sortIndex)}>
        {label}
        {indicator(sortIndex)}
      </button>
      <span
        className="th__grip"
        onMouseDown={(event) => startResize(slot, event)}
        onDoubleClick={() => autoFit(slot, read)}
        title="Drag to resize, double-click to fit"
      />
    </div>
  );

  return (
    <div className="table">
      {cluster && (
        <BulkBar
          cluster={cluster}
          resource={resource}
          selected={checkedRows}
          visible={visible}
          onClear={() => setChecked(new Set())}
          onDone={() => setChecked(new Set())}
        />
      )}

      <div className="table__scroll" ref={scrollRef}>
        <div className="table__inner" style={{ minWidth: "max-content" }}>
          <div className="table__head" style={{ gridTemplateColumns: template }} role="row">
            <div className="th th--check">
              <input
                type="checkbox"
                checked={allChecked}
                ref={(element) => {
                  if (element) {
                    element.indeterminate = checkedRows.length > 0 && !allChecked;
                  }
                }}
                onChange={toggleAll}
                aria-label={allChecked ? "Clear selection" : "Select every visible row"}
              />
            </div>
            {headerCell("name", "Name", -1, namePosition, (row) => row.name)}
            {showNamespace &&
              headerCell(
                "namespace",
                "Namespace",
                -2,
                namespacePosition,
                (row) => row.namespace ?? "",
              )}
            {columns.map((col, index) =>
              headerCell(
                col.name,
                col.name,
                index,
                cellStart + index,
                (row) => cellValue(row, index),
                col.description ?? undefined,
              ),
            )}
            {headerCell(
              "age",
              showAbsolute ? "Created" : "Age",
              -3,
              agePosition,
              (row) => age(row.created, now),
            )}
          </div>

          <div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
            {visible.length === 0 && watchState.state === "ready" && (
              <p className="muted table-empty">No {resource.kind} found.</p>
            )}
            {virtualizer.getVirtualItems().map((item) => {
              const row = visible[item.index];
              if (!row) return null;
              return (
                <div
                  key={row.uid}
                  role="row"
                  className={`tr tr--${row.health}${
                    selected?.uid === row.uid ? " tr--selected" : ""
                  }${row.terminating ? " tr--terminating" : ""}`}
                  style={{
                    gridTemplateColumns: template,
                    position: "absolute",
                    top: 0,
                    left: 0,
                    height: item.size,
                    transform: `translateY(${item.start}px)`,
                  }}
                  onClick={() => select(row)}
                >
                  <span className="td td--check">
                    <input
                      type="checkbox"
                      checked={checked.has(row.uid)}
                      onClick={(event) => toggleRow(row, event)}
                      onChange={() => {}}
                      aria-label={`Select ${row.name}`}
                    />
                  </span>
                  <span className="td td--name" title={row.name}>
                    <span className="health" />
                    <span className="td__text">{row.name}</span>
                  </span>
                  {showNamespace && (
                    <span className="td" title={row.namespace ?? ""}>
                      <span className="td__text">{row.namespace ?? ""}</span>
                    </span>
                  )}
                  {columns.map((col, index) => {
                    if (index >= ownColumns) {
                      const cell = liveCell(row, index);
                      return (
                        <span key={col.name} className="td">
                          {cell.node ?? <span className="td__text">{cell.text}</span>}
                        </span>
                      );
                    }
                    const value = row.cells[index] ?? "";
                    const tone = toneFor(col.name, value);
                    return (
                      <span key={col.name} className="td" title={value}>
                        <span className={`td__text${tone ? ` status status--${tone}` : ""}`}>
                          {value}
                        </span>
                      </span>
                    );
                  })}
                  <span className="td" title={formatDateTime(row.created, zone)}>
                    <span className="td__text">
                      {showAbsolute ? formatDateTime(row.created, zone) : age(row.created, now)}
                    </span>
                  </span>
                </div>
              );
            })}
          </div>
        </div>
      </div>
    </div>
  );
}
