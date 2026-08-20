/**
 * Timestamp formatting.
 *
 * Kubernetes reports every timestamp in UTC. Rendering them raw is how an
 * incident timeline gets misread by a whole timezone, so absolute times are
 * converted for display. The YAML tab is deliberately excluded: it shows what
 * the cluster stores, and rewriting it would make a copied manifest wrong.
 */

/** `system` follows the machine; anything else is an IANA zone name. */
export type TimeZoneSetting = string;

function zoneOf(setting: TimeZoneSetting): string | undefined {
  return setting && setting !== "system" ? setting : undefined;
}

/** The zone actually in effect, resolved for display. */
export function effectiveZone(setting: TimeZoneSetting): string {
  return (
    zoneOf(setting) ??
    Intl.DateTimeFormat().resolvedOptions().timeZone ??
    "UTC"
  );
}

/** Zones the platform knows, for the picker. */
export function availableZones(): string[] {
  const supported = (
    Intl as typeof Intl & { supportedValuesOf?: (key: string) => string[] }
  ).supportedValuesOf;

  if (typeof supported === "function") {
    try {
      return supported("timeZone");
    } catch {
      // Fall through to the short list.
    }
  }

  // Enough to be useful where the full database is unavailable.
  return [
    "UTC",
    "Asia/Jakarta",
    "Asia/Makassar",
    "Asia/Jayapura",
    "Asia/Singapore",
    "Asia/Tokyo",
    "Asia/Kolkata",
    "Europe/London",
    "Europe/Berlin",
    "America/New_York",
    "America/Los_Angeles",
    "Australia/Sydney",
  ];
}

/** Full date and time, for detail panes. */
export function formatDateTime(iso: string | null, setting: TimeZoneSetting): string {
  if (!iso) return "—";
  const at = Date.parse(iso);
  if (Number.isNaN(at)) return iso;

  // 24-hour explicitly: correlating a log line at "21:30" with one at
  // "9:30 PM" is an unnecessary step in the middle of an incident.
  return new Intl.DateTimeFormat(undefined, {
    year: "numeric",
    month: "short",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
    timeZone: zoneOf(setting),
  }).format(at);
}

/** Clock time only, for chart axes. */
export function formatClock(at: number, setting: TimeZoneSetting): string {
  return new Intl.DateTimeFormat(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
    timeZone: zoneOf(setting),
  }).format(at);
}

/** Short zone label, e.g. `GMT+7`, for a heading. */
export function zoneLabel(setting: TimeZoneSetting): string {
  const parts = new Intl.DateTimeFormat(undefined, {
    timeZoneName: "shortOffset",
    timeZone: zoneOf(setting),
  }).formatToParts(Date.now());
  return parts.find((part) => part.type === "timeZoneName")?.value ?? "";
}

/** A log line's leading RFC3339 timestamp, if Kubernetes added one. */
const LOG_TIMESTAMP = /^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z)\s?/;

/**
 * Rewrite the timestamp Kubernetes prefixes to a log line.
 *
 * Those are always UTC. Reading them beside a local wall clock is exactly the
 * confusion this setting exists to remove, so the prefix is converted and the
 * rest of the line left untouched.
 */
export function localiseLogLine(line: string, setting: TimeZoneSetting): string {
  const match = LOG_TIMESTAMP.exec(line);
  if (!match) return line;

  const at = Date.parse(match[1]!);
  if (Number.isNaN(at)) return line;

  const stamp = new Intl.DateTimeFormat(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    fractionalSecondDigits: 3,
    hour12: false,
    timeZone: zoneOf(setting),
  }).format(at);

  return `${stamp} ${line.slice(match[0].length)}`;
}
