/** Human formatting shared by the overview gauges and charts. */

export function cores(value: number): string {
  if (value >= 10) return value.toFixed(0);
  if (value >= 1) return value.toFixed(2);
  return value.toFixed(3).replace(/0+$/, "").replace(/\.$/, "") || "0";
}

/** Binary units, matching how Kubernetes reports memory. */
export function bytes(value: number): string {
  if (value <= 0) return "0";
  const units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
  const index = Math.min(Math.floor(Math.log2(value) / 10), units.length - 1);
  const scaled = value / 1024 ** index;
  return `${scaled >= 100 ? scaled.toFixed(0) : scaled.toFixed(1)}${units[index]}`;
}

export function count(value: number): string {
  return Math.round(value).toString();
}

export function percent(value: number, total: number): string {
  if (total <= 0) return "—";
  return `${((value / total) * 100).toFixed(0)}%`;
}

