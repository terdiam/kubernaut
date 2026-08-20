/** kubectl-style age string, computed client-side so it ticks without IPC. */
export function age(created: string | null, now: number): string {
  if (!created) return "";
  const started = Date.parse(created);
  if (Number.isNaN(started)) return "";
  const secs = Math.max(0, Math.floor((now - started) / 1000));
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m${secs % 60}s`;
  if (secs < 86400) return `${Math.floor(secs / 3600)}h${Math.floor((secs % 3600) / 60)}m`;
  if (secs < 86400 * 100)
    return `${Math.floor(secs / 86400)}d${Math.floor((secs % 86400) / 3600)}h`;
  return `${Math.floor(secs / 86400)}d`;
}
