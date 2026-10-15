export function bytes(n: number): string {
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let i = 0;
  while (Math.abs(n) >= 1024 && i < units.length - 1) {
    n /= 1024;
    i++;
  }
  const s = n >= 100 || i === 0 || Number.isInteger(n) ? n.toFixed(0) : n.toFixed(1);
  return `${s} ${units[i]}`;
}

export const kib = (n: number) => bytes(n * 1024);
export const mib = (n: number) => bytes(n * 1024 * 1024);
export const GiB = 1024 ** 3;

/** Human memory amounts for sizes picked in MiB: "512 MiB", "2 GiB". */
export function memLabel(mibs: number): string {
  return mibs >= 1024 && mibs % 512 === 0 ? `${+(mibs / 1024).toFixed(1)} GiB` : `${mibs} MiB`;
}

export function duration(ns: number): string {
  let s = Math.floor(ns / 1e9);
  const h = Math.floor(s / 3600);
  s %= 3600;
  const m = Math.floor(s / 60);
  s %= 60;
  return h ? `${h}h ${m}m` : m ? `${m}m ${s}s` : `${s}s`;
}

const rtf = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });

/** "3 minutes ago" from a Date, ms timestamp or RFC 3339 string. */
export function ago(when: Date | number | string): string {
  const t = typeof when === "string" ? Date.parse(when) : +when;
  const s = Math.round((t - Date.now()) / 1000);
  const abs = Math.abs(s);
  if (abs < 45) return "just now";
  if (abs < 3600) return rtf.format(Math.round(s / 60), "minute");
  if (abs < 86400) return rtf.format(Math.round(s / 3600), "hour");
  if (abs < 86400 * 30) return rtf.format(Math.round(s / 86400), "day");
  return new Date(t).toLocaleDateString();
}

export function dateTime(when: Date | number | string): string {
  return new Date(typeof when === "string" ? Date.parse(when) : when).toLocaleString(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  });
}

export const pct = (f: number) => `${Math.round(f * 100)}%`;

export function plural(n: number, one: string, many = `${one}s`): string {
  return `${n} ${n === 1 ? one : many}`;
}
