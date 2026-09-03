import { useLookup } from "../formContext";
import type { LookupOption } from "../types";

type Obj = Record<string, unknown>;

const PATH_TYPES = ["Prefix", "Exact", "ImplementationSpecific"];

/**
 * Host and path routing, with the backend chosen from the cluster.
 *
 * The two fields worth not typing are here: the Service the path routes to,
 * and the port on it. A backend naming a Service that does not exist, or a
 * port it never exposed, produces an Ingress the controller accepts and then
 * answers 503 from — the failure appears far from the mistake.
 */
export function IngressRulesField({
  value,
  onChange,
}: {
  value: unknown;
  onChange: (next: Obj[] | undefined) => void;
}) {
  const rules = Array.isArray(value) ? (value as Obj[]) : [];

  const write = (next: Obj[]) => onChange(next.length ? next : undefined);
  const updateRule = (index: number, patch: Obj) => {
    const next = rules.slice();
    next[index] = { ...next[index], ...patch };
    write(next);
  };

  return (
    <div className="rules">
      {rules.map((rule, index) => (
        <RuleBlock
          key={index}
          rule={rule}
          onChange={(patch) => updateRule(index, patch)}
          onRemove={() => write(rules.filter((_, at) => at !== index))}
        />
      ))}

      <button
        type="button"
        className="button button--ghost"
        onClick={() =>
          write([
            ...rules,
            { host: "", http: { paths: [newPath()] } },
          ])
        }
      >
        Add host
      </button>
    </div>
  );
}

function newPath(): Obj {
  return {
    path: "/",
    pathType: "Prefix",
    backend: { service: { name: "", port: {} } },
  };
}

function RuleBlock({
  rule,
  onChange,
  onRemove,
}: {
  rule: Obj;
  onChange: (patch: Obj) => void;
  onRemove: () => void;
}) {
  const http = (rule.http ?? {}) as Obj;
  const paths = Array.isArray(http.paths) ? (http.paths as Obj[]) : [];

  const writePaths = (next: Obj[]) => onChange({ http: { ...http, paths: next } });
  const updatePath = (index: number, patch: Obj) => {
    const next = paths.slice();
    next[index] = { ...next[index], ...patch };
    writePaths(next);
  };

  return (
    <fieldset className="rules__host">
      <div className="rules__hostrow">
        <input
          value={typeof rule.host === "string" ? rule.host : ""}
          placeholder="host (leave empty to match any)"
          onChange={(e) => onChange({ host: e.target.value || undefined })}
        />
        <button type="button" className="icon-button" onClick={onRemove} aria-label="Remove host">
          ✕
        </button>
      </div>

      {paths.map((path, index) => (
        <PathRow
          key={index}
          path={path}
          onChange={(patch) => updatePath(index, patch)}
          onRemove={() => writePaths(paths.filter((_, at) => at !== index))}
        />
      ))}

      <button
        type="button"
        className="button button--ghost rules__addpath"
        onClick={() => writePaths([...paths, newPath()])}
      >
        Add path
      </button>
    </fieldset>
  );
}

function PathRow({
  path,
  onChange,
  onRemove,
}: {
  path: Obj;
  onChange: (patch: Obj) => void;
  onRemove: () => void;
}) {
  const backend = (path.backend ?? {}) as Obj;
  const service = (backend.service ?? {}) as Obj;
  const serviceName = typeof service.name === "string" ? service.name : "";

  const services = useLookup("services", null);
  // Ports are listed for the Service already chosen; before that there is
  // nothing to ask for, and asking anyway is one failing request per path row.
  const ports = useLookup("servicePorts", serviceName || null, serviceName !== "");

  const setService = (name: string) =>
    // The port belongs to the old Service, so it cannot survive the change.
    onChange({ backend: { service: { name, port: {} } } });

  const setPort = (raw: string) => {
    const numeric = Number(raw);
    const port =
      raw === "" ? {} : Number.isInteger(numeric) && raw.trim() !== "" ? { number: numeric } : { name: raw };
    onChange({ backend: { service: { ...service, port } } });
  };

  const storedPort = portValue((service.port ?? {}) as Obj);
  // The lookup writes a named port's *name* as the option value ("http"), so
  // renaming survives renumbering — but a manifest that already references the
  // port by number ("80") would then match nothing by raw value, even though
  // it is the same port, and get wrongly flagged as gone from the service. A
  // label always leads with the number (`"80"` or `"80 · http"`), so match on
  // that too before deciding the stored port is genuinely stale.
  const matchedOption = ports.options.find(
    (option) => option.value === storedPort || option.label.split(" · ")[0] === storedPort,
  );
  const currentPort = matchedOption?.value ?? storedPort;

  return (
    <div className="rules__path">
      <input
        className="rules__pathvalue"
        value={typeof path.path === "string" ? path.path : ""}
        placeholder="/"
        onChange={(e) => onChange({ path: e.target.value })}
      />

      <select
        value={typeof path.pathType === "string" ? path.pathType : "Prefix"}
        onChange={(e) => onChange({ pathType: e.target.value })}
      >
        {PATH_TYPES.map((type) => (
          <option key={type} value={type}>
            {type}
          </option>
        ))}
      </select>

      <ServiceSelect
        value={serviceName}
        options={services.options}
        loading={services.loading}
        error={services.error}
        onChange={setService}
      />

      <select
        value={currentPort}
        onChange={(e) => setPort(e.target.value)}
        disabled={!serviceName}
        title={serviceName ? undefined : "Choose a service first"}
      >
        <option value="">
          {!serviceName
            ? "port"
            : ports.loading
              ? "loading…"
              : ports.options.length === 0
                ? "no ports found"
                : "port"}
        </option>
        {ports.options.map((option) => (
          <option key={option.value} value={option.value}>
            {option.label}
            {option.detail ? ` ${option.detail}` : ""}
          </option>
        ))}
        {/* An existing manifest may name a port the Service no longer has;
            keeping it visible beats silently blanking the field. */}
        {currentPort !== "" &&
          !ports.options.some((option) => option.value === currentPort) && (
            <option value={currentPort}>{currentPort} (not on this service)</option>
          )}
      </select>

      <button type="button" className="icon-button" onClick={onRemove} aria-label="Remove path">
        ✕
      </button>
    </div>
  );
}

function ServiceSelect({
  value,
  options,
  loading,
  error,
  onChange,
}: {
  value: string;
  options: LookupOption[];
  loading: boolean;
  error: string | null;
  onChange: (next: string) => void;
}) {
  if (error) {
    return (
      <input
        value={value}
        placeholder="service"
        onChange={(e) => onChange(e.target.value)}
        title={`Could not list services: ${error}`}
      />
    );
  }

  return (
    <select value={value} onChange={(e) => onChange(e.target.value)}>
      <option value="">{loading ? "loading…" : "service"}</option>
      {options.map((option) => (
        <option key={option.value} value={option.value}>
          {option.label}
        </option>
      ))}
      {/* Same reason as the port: a Service named here but not present yet. */}
      {value !== "" && !options.some((option) => option.value === value) && (
        <option value={value}>{value} (not in this namespace)</option>
      )}
    </select>
  );
}

/** The chosen port as one string, whichever of the two shapes it uses. */
export function portValue(port: Obj): string {
  if (typeof port.number === "number") return String(port.number);
  if (typeof port.name === "string") return port.name;
  return "";
}
