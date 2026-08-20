import { useEffect, useMemo, useState } from "react";
import { api } from "../api";
import { useStore } from "../store";
import type {
  Finding,
  ImageUsage,
  ScanReport,
  Scanner,
  Severity,
  Vulnerability,
} from "../types";

type Panel = "posture" | "rbac" | "images";

const SEVERITIES: Severity[] = ["critical", "high", "medium", "low", "info"];

/**
 * Default threshold.
 *
 * A real cluster produces thousands of low-severity hardening notes — every
 * container without a read-only root filesystem, every workload without a
 * memory limit. They are all true and none of them is why you opened this
 * screen, so the list starts at medium and the rest is one click away.
 */
const DEFAULT_MINIMUM: Severity = "medium";

function atLeast(severity: Severity, minimum: Severity): boolean {
  return SEVERITIES.indexOf(severity) <= SEVERITIES.indexOf(minimum);
}

export function SecurityCenter() {
  const cluster = useStore((s) => s.activeCluster);
  const selectedNamespaces = useStore((s) => s.selectedNamespaces);
  const openObject = useStore((s) => s.openObject);

  const [panel, setPanel] = useState<Panel>("posture");
  const [posture, setPosture] = useState<ScanReport | null>(null);
  const [rbac, setRbac] = useState<ScanReport | null>(null);
  const [images, setImages] = useState<ImageUsage[]>([]);
  const [scanner, setScanner] = useState<Scanner | null>(null);
  const [minimum, setMinimum] = useState<Severity>(DEFAULT_MINIMUM);
  const [showBuiltin, setShowBuiltin] = useState(false);
  const [filter, setFilter] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const namespace = selectedNamespaces.length === 1 ? selectedNamespaces[0]! : null;

  const scan = async () => {
    if (!cluster) return;
    setBusy(true);
    setError(null);
    try {
      const [postureReport, rbacReport, imageList, detected] = await Promise.all([
        api.postureScan(cluster, namespace),
        api.rbacScan(cluster),
        api.clusterImages(cluster, namespace),
        api.vulnerabilityScanner(cluster),
      ]);
      setPosture(postureReport);
      setRbac(rbacReport);
      setImages(imageList);
      setScanner(detected);
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  useEffect(() => {
    void scan();
    // Scans are expensive and the answer changes slowly; refreshing is manual.
  }, [cluster, namespace]);

  const report = panel === "rbac" ? rbac : posture;

  const visible = useMemo(() => {
    if (!report) return [];
    const needle = filter.trim().toLowerCase();
    return report.findings.filter((finding) => {
      if (!atLeast(finding.severity, minimum)) return false;
      if (finding.builtin && !showBuiltin) return false;
      if (!needle) return true;
      return (
        finding.name.toLowerCase().includes(needle) ||
        finding.title.toLowerCase().includes(needle) ||
        finding.id.toLowerCase().includes(needle) ||
        (finding.namespace ?? "").toLowerCase().includes(needle)
      );
    });
  }, [report, minimum, showBuiltin, filter]);

  if (!cluster) {
    return <p className="muted overview__note">Connect a cluster to scan it.</p>;
  }

  return (
    <div className="sec">
      <nav className="overview__panels">
        {(["posture", "rbac", "images"] as Panel[]).map((entry) => (
          <button
            key={entry}
            className={`tab${panel === entry ? " tab--active" : ""}`}
            onClick={() => setPanel(entry)}
          >
            {entry === "posture"
              ? "Workloads"
              : entry === "rbac"
                ? "Access control"
                : "Images"}
          </button>
        ))}
      </nav>

      <div className="sec__toolbar">
        <button className="button" disabled={busy} onClick={() => void scan()}>
          {busy ? "Scanning…" : "Rescan"}
        </button>

        {panel !== "images" && (
          <>
            <label className="sec__field">
              Minimum severity
              <select
                value={minimum}
                onChange={(e) => setMinimum(e.target.value as Severity)}
              >
                {SEVERITIES.map((severity) => (
                  <option key={severity} value={severity}>
                    {severity}
                  </option>
                ))}
              </select>
            </label>

            <label
              className="checkbox"
              title="Roles and bindings Kubernetes and the distribution install themselves"
            >
              <input
                type="checkbox"
                checked={showBuiltin}
                onChange={(e) => setShowBuiltin(e.target.checked)}
              />
              Include built-in objects
              {report && report.builtinHidden > 0 && !showBuiltin
                ? ` (${report.builtinHidden} hidden)`
                : ""}
            </label>

            <input
              className="sec__filter"
              value={filter}
              placeholder="Filter findings"
              onChange={(e) => setFilter(e.target.value)}
            />
          </>
        )}

        {namespace && <span className="muted">namespace: {namespace}</span>}
        {error && <span className="error">{error}</span>}
      </div>

      {report && panel !== "images" && (
        <div className="sec__summary">
          <Counters counts={report.counts} />
          <span className="muted">
            {report.examined} object{report.examined === 1 ? "" : "s"} examined ·{" "}
            {visible.length} shown of {report.findings.length}
          </span>
        </div>
      )}

      {report?.limitations.map((limitation) => (
        <p key={limitation} className="warning sec__note">
          {limitation}
        </p>
      ))}

      {panel === "images" ? (
        <ImagePanel cluster={cluster} namespace={namespace} images={images} scanner={scanner} />
      ) : (
        <div className="sec__body">
          {visible.length === 0 ? (
            <p className="muted">
              {report && report.findings.length > 0
                ? "Nothing at this severity. Lower the threshold to see the rest."
                : "No findings."}
            </p>
          ) : (
            <ul className="findings">
              {visible.map((finding, index) => (
                <FindingRow
                  key={`${finding.id}-${finding.namespace}-${finding.name}-${finding.container}-${index}`}
                  finding={finding}
                  onOpen={() =>
                    void openObject(finding.resource, finding.namespace, finding.name)
                  }
                />
              ))}
            </ul>
          )}
        </div>
      )}
    </div>
  );
}

function Counters({ counts }: { counts: ScanReport["counts"] }) {
  const entries: [Severity, number][] = [
    ["critical", counts.critical],
    ["high", counts.high],
    ["medium", counts.medium],
    ["low", counts.low],
  ];
  return (
    <span className="sec__counters">
      {entries.map(([severity, count]) => (
        <span key={severity} className={`sev sev--${severity}`}>
          {count} {severity}
        </span>
      ))}
    </span>
  );
}

function FindingRow({ finding, onOpen }: { finding: Finding; onOpen: () => void }) {
  const [open, setOpen] = useState(false);

  return (
    <li className={`finding finding--${finding.severity}`}>
      <button className="finding__head" onClick={() => setOpen((value) => !value)}>
        <span className={`sev sev--${finding.severity}`}>{finding.severity}</span>
        <span className="finding__title">{finding.title}</span>
        <span className="finding__object">
          {finding.kind} {finding.namespace ? `${finding.namespace}/` : ""}
          {finding.name}
          {finding.container ? ` · ${finding.container}` : ""}
        </span>
        {finding.builtin && <span className="chip">built-in</span>}
        <span className="finding__caret">{open ? "⌄" : "›"}</span>
      </button>

      {open && (
        <div className="finding__body">
          <p>{finding.message}</p>
          <p className="finding__fix">
            <strong>Fix:</strong> {finding.remediation}
          </p>
          <div className="finding__actions">
            <span className="muted">{finding.id}</span>
            <button className="button" onClick={onOpen}>
              Open {finding.kind}
            </button>
          </div>
        </div>
      )}
    </li>
  );
}

/** How many images a local scan covers by default. */
const DEFAULT_SCAN_LIMIT = 10;

function ImagePanel({
  cluster,
  namespace,
  images,
  scanner: initialScanner,
}: {
  cluster: string;
  namespace: string | null;
  images: ImageUsage[];
  scanner: Scanner | null;
}) {
  const [scanner, setScanner] = useState(initialScanner);
  const [preparing, setPreparing] = useState(false);
  const [prepareError, setPrepareError] = useState<string | null>(null);
  const [limit, setLimit] = useState(DEFAULT_SCAN_LIMIT);

  useEffect(() => setScanner(initialScanner), [initialScanner]);

  const prepare = async () => {
    setPreparing(true);
    setPrepareError(null);
    try {
      setScanner(await api.downloadVulnerabilityDatabase(cluster));
    } catch (err) {
      setPrepareError(String(err));
    } finally {
      setPreparing(false);
    }
  };
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [report, setReport] = useState<ScanReport | null>(null);
  const [vulnerabilities, setVulnerabilities] = useState<Vulnerability[]>([]);

  const scanImages = async () => {
    setBusy(true);
    setError(null);
    try {
      const result = await api.vulnerabilityScan(cluster, namespace, limit);
      setReport(result.report);
      setVulnerabilities(result.vulnerabilities);
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const worst = useMemo(() => {
    const byImage = new Map<string, { critical: number; high: number }>();
    for (const vulnerability of vulnerabilities) {
      const entry = byImage.get(vulnerability.image) ?? { critical: 0, high: 0 };
      if (vulnerability.severity === "critical") entry.critical += 1;
      if (vulnerability.severity === "high") entry.high += 1;
      byImage.set(vulnerability.image, entry);
    }
    return byImage;
  }, [vulnerabilities]);

  return (
    <div className="sec__body">
      {scanner?.kind === "none" && (
        <p className="warning sec__note">
          No vulnerability scanner available. {scanner.reason} The images below are still listed
          so you can see what is running.
        </p>
      )}
      {scanner?.kind === "trivyOperator" && (
        <p className="muted sec__note">
          Trivy Operator is installed; its reports are read directly from the cluster.
        </p>
      )}
      {scanner?.kind === "trivyBinary" && !scanner.databaseReady && (
        <div className="sec__note warning">
          <p>
            Trivy is installed but its vulnerability database has not been downloaded — about
            110 MiB to fetch, roughly 1.2 GB on disk. Scanning before it is in place just times
            out.
          </p>
          <button
            className="button button--primary"
            disabled={preparing}
            onClick={() => void prepare()}
          >
            {preparing ? "Downloading… (several minutes)" : "Download database"}
          </button>
          {prepareError && <p className="error">{prepareError}</p>}
        </div>
      )}

      {scanner?.kind === "trivyBinary" && scanner.databaseReady && (
        <>
          <p className="muted sec__note">
            Scanning locally with {scanner.version}. Each image is pulled and analysed, so a full
            pass over {images.length} images takes a long time — the most-used ones are scanned
            first.
          </p>
          <div className="sec__toolbar">
            <label className="sec__field">
              Images to scan
              <select value={limit} onChange={(e) => setLimit(Number(e.target.value))}>
                {[5, 10, 25, 50].map((value) => (
                  <option key={value} value={value}>
                    {value}
                  </option>
                ))}
              </select>
            </label>
            <button className="button button--primary" disabled={busy} onClick={() => void scanImages()}>
              {busy ? "Scanning…" : "Scan images"}
            </button>
            {error && <span className="error">{error}</span>}
          </div>
        </>
      )}

      {scanner?.kind === "trivyOperator" && (
        <div className="sec__toolbar">
          <button className="button" disabled={busy} onClick={() => void scanImages()}>
            {busy ? "Loading…" : "Load reports"}
          </button>
          {error && <span className="error">{error}</span>}
        </div>
      )}

      {report && (
        <>
          <div className="sec__summary">
            <Counters counts={report.counts} />
            <span className="muted">
              {vulnerabilities.length} vulnerabilities across {report.examined} images
            </span>
          </div>
          {report.limitations.map((limitation) => (
            <p key={limitation} className="muted sec__note">
              {limitation}
            </p>
          ))}
          {report.findings.length > 0 && (
            <ul className="findings sec__findings">
              {report.findings.map((finding, index) => (
                <FindingRow
                  key={`${finding.id}-${finding.name}-${index}`}
                  finding={finding}
                  onOpen={() => {}}
                />
              ))}
            </ul>
          )}
        </>
      )}

      <table className="helm__table">
        <thead>
          <tr>
            <th>Image</th>
            <th>Pods</th>
            <th>Critical / High</th>
            <th>Running in</th>
          </tr>
        </thead>
        <tbody>
          {images.map((usage) => (
            <tr key={usage.image}>
              <td className="sec__image" title={usage.image}>
                {usage.image}
              </td>
              <td className="helm__num">{usage.podCount}</td>
              <td className="helm__num">
                {(() => {
                  const counts = worst.get(usage.image);
                  if (!counts) return <span className="muted">—</span>;
                  return (
                    <>
                      {counts.critical > 0 && (
                        <span className="sev sev--critical">{counts.critical}</span>
                      )}
                      {counts.high > 0 && <span className="sev sev--high">{counts.high}</span>}
                      {counts.critical === 0 && counts.high === 0 && (
                        <span className="muted">clean</span>
                      )}
                    </>
                  );
                })()}
              </td>
              <td className="muted sec__image" title={usage.usedBy.join(", ")}>
                {usage.usedBy.slice(0, 2).join(", ")}
                {usage.podCount > 2 ? ` +${usage.podCount - 2}` : ""}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
