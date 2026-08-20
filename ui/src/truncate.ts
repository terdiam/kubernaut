/**
 * Shorten a name from the middle.
 *
 * Kubernetes names carry the distinguishing part at the end — nine pods of one
 * CronJob differ only in the hash suffix. Truncating at the end turns them into
 * nine identical-looking rows, so the ellipsis goes in the middle instead.
 */
export function middleTruncate(value: string, limit: number): string {
  if (value.length <= limit) return value;
  const head = Math.ceil((limit - 1) * 0.6);
  return `${value.slice(0, head)}…${value.slice(value.length - (limit - 1 - head))}`;
}
