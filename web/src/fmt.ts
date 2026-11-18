// Formatting helpers. Power in W from the API, shown in kW; energy Wh → kWh.

export const kw = (w: number | null | undefined, digits = 2): string => (w === null || w === undefined || !isFinite(w) ? '—' : (w / 1000).toFixed(digits));
export const kwh = (wh: number | null | undefined, digits = 1): string => (wh === null || wh === undefined || !isFinite(wh) ? '—' : (wh / 1000).toFixed(digits));
export const pct = (frac: number | null | undefined, digits = 0): string => (frac === null || frac === undefined || !isFinite(frac) ? '—' : `${(frac * 100).toFixed(digits)} %`);
export const pctv = (v: number | null | undefined, digits = 0): string => (v === null || v === undefined || !isFinite(v) ? '—' : `${v.toFixed(digits)} %`);
export const num = (v: number | null | undefined, digits = 1, unit = ''): string => (v === null || v === undefined || !isFinite(v) ? '—' : `${v.toFixed(digits)}${unit ? ' ' + unit : ''}`);

export function money(v: number | null | undefined, currency = 'USD'): string {
  if (v === null || v === undefined || !isFinite(v)) return '—';
  try {
    return new Intl.NumberFormat(undefined, { style: 'currency', currency }).format(v);
  } catch {
    return v.toFixed(2);
  }
}

export function time(ts: number | null | undefined): string {
  if (!ts) return '—';
  return new Date(ts * 1000).toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' });
}

export function timeIso(iso: string | null | undefined): string {
  if (!iso) return '—';
  return new Date(iso).toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' });
}

export function dateTime(ts: number | null | undefined): string {
  if (!ts) return '—';
  return new Date(ts * 1000).toLocaleString([], { month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' });
}

export function dayLabel(dateStr: string): string {
  const [y, m, d] = dateStr.split('-').map(Number);
  return new Date(y, m - 1, d).toLocaleDateString([], { weekday: 'short', month: 'short', day: 'numeric' });
}

export function duration(seconds: number | null | undefined): string {
  if (seconds === null || seconds === undefined || !isFinite(seconds)) return '—';
  const s = Math.max(0, Math.round(seconds));
  if (s < 90) return `${s} s`;
  const m = Math.round(s / 60);
  if (m < 90) return `${m} min`;
  const hrs = Math.floor(m / 60);
  const mm = m % 60;
  if (hrs < 48) return `${hrs} h ${mm.toString().padStart(2, '0')} min`;
  return `${(hrs / 24).toFixed(1)} days`;
}

export function hours(h: number | null | undefined): string {
  if (h === null || h === undefined || !isFinite(h)) return '—';
  return duration(h * 3600);
}

export function ago(ts: number | null | undefined, now = Date.now() / 1000): string {
  if (!ts) return 'never';
  return `${duration(now - ts)} ago`;
}

export const nowS = () => Math.floor(Date.now() / 1000);

// ---- Display units (per browser; the server always stores SI) ----

export type Units = 'imperial' | 'metric';

export function units(): Units {
  try {
    return localStorage.getItem('rs-units') === 'metric' ? 'metric' : 'imperial';
  } catch {
    return 'imperial';
  }
}

export function setUnits(u: Units) {
  try {
    localStorage.setItem('rs-units', u);
  } catch {
    /* private mode */
  }
}

const ok = (v: number | null | undefined): v is number => v !== null && v !== undefined && isFinite(v);

/** °C → display value (°F for imperial). */
export const tempVal = (c: number | null | undefined): number | null => (ok(c) ? (units() === 'imperial' ? c * 1.8 + 32 : c) : null);
export const tempUnit = (): string => (units() === 'imperial' ? '°F' : '°C');
export const temp = (c: number | null | undefined, digits = 0): string => (ok(c) ? `${tempVal(c)!.toFixed(digits)} ${tempUnit()}` : '—');
/** Temperature difference (no +32 offset). */
export const tempDelta = (c: number): number => (units() === 'imperial' ? c * 1.8 : c);

/** m/s → mph or m/s. */
export const wind = (ms: number | null | undefined, digits = 0): string => (ok(ms) ? (units() === 'imperial' ? `${(ms * 2.236936).toFixed(digits)} mph` : `${ms.toFixed(digits)} m/s`) : '—');
/** mm → in or mm. */
export const rain = (mm: number | null | undefined): string => (ok(mm) ? (units() === 'imperial' ? `${(mm / 25.4).toFixed(2)} in` : `${mm.toFixed(1)} mm`) : '—');
