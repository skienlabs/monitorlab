const UNITS = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];

export function bytes(n: number | null | undefined, digits = 1): string {
  if (n == null || !isFinite(n)) return "—";
  if (n < 0) n = 0;
  let i = 0;
  while (n >= 1024 && i < UNITS.length - 1) {
    n /= 1024;
    i++;
  }
  return `${n.toFixed(i === 0 ? 0 : n >= 100 ? 0 : digits)} ${UNITS[i]}`;
}

export function rate(n: number | null | undefined): string {
  if (n == null || !isFinite(n)) return "—";
  return `${bytes(n)}/s`;
}

export function pct(n: number | null | undefined, digits = 0): string {
  if (n == null || !isFinite(n)) return "—";
  return `${n.toFixed(digits)}%`;
}

export function ratio(used: number, total: number): number {
  return total > 0 ? (used * 100) / total : 0;
}

export function duration(secs: number | null | undefined): string {
  if (secs == null || !isFinite(secs)) return "—";
  const d = Math.floor(secs / 86400);
  const h = Math.floor((secs % 86400) / 3600);
  const m = Math.floor((secs % 3600) / 60);
  if (d > 0) return `${d}d ${h}h`;
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m`;
  return `${Math.max(0, Math.floor(secs))}s`;
}

export function ago(ms: number | null | undefined): string {
  if (!ms) return "never";
  const s = (Date.now() - ms) / 1000;
  if (s < 45) return "just now";
  if (s < 3600) return `${Math.round(s / 60)} min ago`;
  if (s < 86400) return `${Math.round(s / 3600)} h ago`;
  if (s < 86400 * 30) return `${Math.round(s / 86400)} days ago`;
  return new Date(ms).toLocaleDateString();
}

export function dateTime(ms: number | null | undefined): string {
  if (!ms) return "—";
  return new Date(ms).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
}

export function shortId(id: string | null | undefined): string {
  if (!id) return "";
  return id.replace(/^sha256:/, "").slice(0, 12);
}

export function level(p: number, warn: number, crit: number): "" | "warn" | "crit" {
  return p >= crit ? "crit" : p >= warn ? "warn" : "";
}

export function plural(n: number, word: string, pluralWord?: string) {
  return `${n} ${n === 1 ? word : pluralWord ?? word + "s"}`;
}
