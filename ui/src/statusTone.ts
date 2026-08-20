/**
 * Colour for a status value.
 *
 * Kubernetes spreads status across dozens of vocabularies — a pod is `Running`,
 * a PVC is `Bound`, a node is `Ready`, an Argo app is `Synced`. Rather than a
 * rule per kind, this maps the words themselves, which is also what makes it
 * work for CRD printer columns nobody anticipated.
 */

export type Tone = "ok" | "pending" | "warn" | "error";

/** Column names whose values are a status rather than free text. */
const STATUS_COLUMNS = new Set([
  "status",
  "phase",
  "ready",
  "health",
  "health status",
  "sync status",
  "state",
  "conditions",
  "available",
  "reason",
  "type",
]);

const OK = new Set([
  "ready",
  "running",
  "succeeded",
  "completed",
  "bound",
  "active",
  "available",
  "healthy",
  "deployed",
  "synced",
  "normal",
  "true",
  "valid",
  "established",
  "namesaccepted",
  "approved",
  "attached",
]);

const PENDING = new Set([
  "pending",
  "containercreating",
  "podinitializing",
  "progressing",
  "creating",
  "initializing",
  "waiting",
  "provisioning",
  "reconciling",
  "installing",
  "upgrading",
  "pending-install",
  "pending-upgrade",
  "pending-rollback",
]);

const WARN = new Set([
  "terminating",
  "schedulingdisabled",
  "warning",
  "suspended",
  "released",
  "outofsync",
  "superseded",
  "degraded",
  "notfound",
  "missing",
  "unknown",
  "uninstalling",
  "paused",
]);

const ERROR = new Set([
  "notready",
  "failed",
  "error",
  "crashloopbackoff",
  "imagepullbackoff",
  "errimagepull",
  "invalidimagename",
  "createcontainerconfigerror",
  "createcontainererror",
  "startererror",
  "starterror",
  "evicted",
  "oomkilled",
  "lost",
  "unhealthy",
  "backoff",
  "false",
  "unschedulable",
  "deadlineexceeded",
  "uninstalled",
  "rejected",
  "forbidden",
]);

function toneForWord(word: string): Tone | null {
  const key = word.trim().toLowerCase();
  if (key === "") return null;
  if (ERROR.has(key)) return "error";
  if (WARN.has(key)) return "warn";
  if (PENDING.has(key)) return "pending";
  if (OK.has(key)) return "ok";
  // Anything ending in these reads as trouble even when the exact word is new,
  // which happens constantly with controller-invented reasons.
  if (key.endsWith("backoff") || key.endsWith("error") || key.endsWith("failed")) return "error";
  return null;
}

/**
 * Tone for a cell, or `null` when the column is not a status.
 *
 * Compound values such as `Ready,SchedulingDisabled` take the most severe tone
 * of their parts: the cordon is the part that matters.
 */
export function toneFor(column: string, value: string): Tone | null {
  if (!STATUS_COLUMNS.has(column.trim().toLowerCase())) return null;

  const severity: Record<Tone, number> = { error: 3, warn: 2, pending: 1, ok: 0 };
  let best: Tone | null = null;

  for (const part of value.split(/[,/]/)) {
    const tone = toneForWord(part);
    if (tone && (best === null || severity[tone] > severity[best])) {
      best = tone;
    }
  }
  return best;
}

/**
 * Tone for a bare status word, for places that already know the value is a
 * status (condition rows, Helm release states).
 */
export function toneForValue(value: string): Tone | null {
  const severity: Record<Tone, number> = { error: 3, warn: 2, pending: 1, ok: 0 };
  let best: Tone | null = null;
  for (const part of value.split(/[,/]/)) {
    const tone = toneForWord(part);
    if (tone && (best === null || severity[tone] > severity[best])) {
      best = tone;
    }
  }
  return best;
}
