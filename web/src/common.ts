// Components shared by several pages.

import { post, errText } from './api';
import { COLORS, ChartSet, Chart, kwVal, pctVal } from './charts';
import { h, replace, toast, estimateTag, type Child } from './dom';
import { dateTime, kw, pctv, time, timeIso } from './fmt';
import type { AlertRow, LiveSnapshot, Recommendation, StoredSample, SunTimes } from './types';

/** Run `fn` now and every `ms`; returns a cancel function. */
export function every(ms: number, fn: () => void | Promise<void>): () => void {
  void fn();
  const id = window.setInterval(() => void fn(), ms);
  return () => window.clearInterval(id);
}

/** Live KPI tiles: solar, home, battery, grid, SOC. */
export function kpiTiles(): { el: HTMLElement; update: (l: LiveSnapshot | null) => void } {
  const mk = (cls: string, label: string, tip: string) => {
    const value = h('div', { class: 'tile-value num' }, '—');
    const sub = h('div', { class: 'tile-sub' }, '');
    const el = h('div', { class: `tile ${cls}`, title: tip }, h('div', { class: 'tile-label' }, label), value, sub);
    return { el, value, sub };
  };
  const solar = mk('t-solar', 'Solar', 'Solar production right now');
  const home = mk('t-home', 'Home', 'Household consumption right now');
  const batt = mk('t-battery', 'Battery', 'Battery power: charging stores energy, discharging supplies the home');
  const grid = mk('t-grid', 'Grid', 'Grid power: import is buying, export is selling back');
  const soc = mk('t-soc', 'Charge', 'Battery state of charge and backup reserve');
  const el = h('div', { class: 'tiles' }, solar.el, home.el, batt.el, grid.el, soc.el);
  const update = (l: LiveSnapshot | null) => {
    const s = l?.status;
    el.classList.toggle('stale', !!l?.warn || !s);
    if (!s) {
      [solar, home, batt, grid, soc].forEach((t) => {
        t.value.textContent = '—';
        t.sub.textContent = '';
      });
      return;
    }
    solar.value.textContent = `${kw(Math.max(0, s.pv_w))} kW`;
    solar.sub.textContent = s.pv_w > 20 ? 'producing' : 'not producing';
    home.value.textContent = `${kw(s.load_w)} kW`;
    home.sub.textContent = 'consumption';
    batt.value.textContent = `${kw(Math.abs(s.storage_w))} kW`;
    batt.sub.textContent = s.storage_w < -20 ? 'charging' : s.storage_w > 20 ? 'discharging' : 'idle';
    grid.value.textContent = `${kw(Math.abs(s.grid_w))} kW`;
    grid.sub.textContent = s.grid_w > 20 ? 'importing' : s.grid_w < -20 ? 'exporting' : 'idle';
    soc.value.textContent = pctv(s.soc_pct);
    soc.sub.textContent = s.backup_soc_pct !== null ? `reserve ${pctv(s.backup_soc_pct)}` : '';
  };
  return { el, update };
}

/** Horizontal day strip: night / twilight / day with sunrise, noon, sunset and a now marker. */
export function sunStrip(sun: SunTimes | null, nowS: number): HTMLElement {
  if (!sun) return h('div', { class: 'muted small' }, 'Set your location to see sun times.');
  const d0 = new Date(sun.date + 'T00:00:00');
  const start = d0.getTime() / 1000;
  const end = start + 86400;
  const pos = (iso: string | null) => (iso ? Math.min(100, Math.max(0, ((new Date(iso).getTime() / 1000 - start) / (end - start)) * 100)) : null);
  const cd = pos(sun.civil_dawn);
  const sr = pos(sun.sunrise);
  const ss = pos(sun.sunset);
  const ck = pos(sun.civil_dusk);
  const bar = h('div', { class: 'sunbar', title: 'Today: night, civil twilight and daylight' });
  if (sr !== null && ss !== null) {
    if (cd !== null) bar.appendChild(h('div', { class: 'seg twilight', style: `left:${cd}%;width:${sr - cd}%` }));
    bar.appendChild(h('div', { class: 'seg day', style: `left:${sr}%;width:${ss - sr}%` }));
    if (ck !== null) bar.appendChild(h('div', { class: 'seg twilight', style: `left:${ss}%;width:${ck - ss}%` }));
  } else if (sun.polar === 'Day') {
    bar.appendChild(h('div', { class: 'seg day', style: 'left:0;width:100%' }));
  }
  const np = ((nowS - start) / (end - start)) * 100;
  if (np >= 0 && np <= 100) bar.appendChild(h('div', { class: 'now', style: `left:${np}%`, title: 'Now' }));
  const len = sun.day_length_s;
  return h(
    'div',
    { class: 'sunstrip' },
    bar,
    h(
      'div',
      { class: 'sun-labels small' },
      h('span', { title: 'Sunrise' }, '☀↑ ', timeIso(sun.sunrise)),
      h('span', { title: 'Solar noon (sun highest)' }, 'Noon ', timeIso(sun.solar_noon), ` · ${sun.noon_elevation_deg.toFixed(0)}°`),
      h('span', { title: 'Sunset' }, '☀↓ ', timeIso(sun.sunset)),
      h('span', { title: 'Day length' }, `${Math.floor(len / 3600)} h ${Math.round((len % 3600) / 60)} min`),
    ),
  );
}

export function recCard(r: Recommendation): HTMLElement {
  const details = h(
    'details',
    { class: 'rec-why' },
    h('summary', { title: 'Show why this was suggested' }, 'Why?'),
    h('p', null, r.why),
    r.evidence.length ? h('table', { class: 'kv' }, ...r.evidence.map(([k, v]) => h('tr', null, h('th', null, k), h('td', { class: 'num' }, v)))) : null,
  );
  return h(
    'article',
    { class: `rec sev-${r.severity}` },
    h('div', { class: 'rec-head' }, h('strong', null, r.title), r.actionable_now ? h('span', { class: 'tag now' }, 'Now') : null, h('span', { class: 'tag' }, `${r.confidence} confidence`)),
    h('p', null, r.message),
    r.window ? h('div', { class: 'small muted' }, `Window: ${time(r.window[0])} – ${time(r.window[1])}`) : null,
    r.benefit ? h('div', { class: 'small benefit' }, r.benefit) : null,
    details,
  );
}

export function alertItem(a: AlertRow, onChange: () => void): HTMLElement {
  const ack = a.acked_at
    ? h('span', { class: 'small muted' }, `Acknowledged ${dateTime(a.acked_at)}`)
    : h(
        'button',
        {
          class: 'ghost',
          title: 'Mark as seen (it stays in history)',
          onclick: async () => {
            try {
              await post(`/alerts/${a.id}/ack`);
              onChange();
            } catch (e) {
              toast(errText(e), 'error');
            }
          },
        },
        'Acknowledge',
      );
  return h(
    'div',
    { class: `alert sev-${a.severity}${a.cleared_at ? ' cleared' : ''}` },
    h('div', { class: 'alert-head' }, h('strong', null, a.title), h('span', { class: 'small muted' }, `${dateTime(a.created_at)}${a.count > 1 ? ` · ×${a.count}` : ''}${a.cleared_at ? ` · cleared ${dateTime(a.cleared_at)}` : ''}`)),
    h('div', null, a.detail),
    a.cleared_at ? null : ack,
  );
}

/** Power chart (kW) for solar/home/battery/grid plus SOC on a right axis. */
export function powerChart(set: ChartSet, height = 280): Chart {
  return set.add({
    height,
    yLabel: 'kW',
    y2Label: 'SOC %',
    y2Range: [0, 100],
    series: [
      { label: 'Solar', color: COLORS.solar, fill: 0.15, value: kwVal, width: 2 },
      { label: 'Home', color: COLORS.home, value: kwVal, width: 1.8 },
      { label: 'Battery (+ discharge)', color: COLORS.battery, value: kwVal },
      { label: 'Grid (+ import)', color: COLORS.grid, value: kwVal },
      { label: 'SOC', color: COLORS.violet, scale: 'y2', dash: [4, 3], value: pctVal },
    ],
    data: [[], [], [], [], [], []],
  });
}

export function powerData(series: StoredSample[]): [number[], (number | null)[], (number | null)[], (number | null)[], (number | null)[], (number | null)[]] {
  return [
    series.map((x) => x.ts),
    series.map((x) => Math.max(0, x.pv_w) / 1000),
    series.map((x) => x.load_w / 1000),
    series.map((x) => x.storage_w / 1000),
    series.map((x) => x.grid_w / 1000),
    series.map((x) => x.soc),
  ];
}

export function notesList(title: string, items: string[] | undefined | null): HTMLElement | null {
  if (!items || !items.length) return null;
  return h('div', { class: 'notes' }, h('div', { class: 'small muted' }, title), h('ul', { class: 'small' }, ...items.map((n) => h('li', null, n))));
}

export function headerRow(title: Child, ...right: Child[]): HTMLElement {
  return h('div', { class: 'card-head' }, h('h2', null, title), h('div', { class: 'spacer' }), ...right);
}

export function estimateHead(title: string, ...right: Child[]): HTMLElement {
  return headerRow(h('span', null, title, ' ', estimateTag()), ...right);
}

export function loading(el: HTMLElement) {
  replace(el, h('div', { class: 'muted small' }, 'Loading…'));
}

/** Merge several series sampled on different x grids into one aligned uPlot data array. */
export function merge(groups: { x: number[]; ys: (number | null)[][] }[]): (number | null)[][] {
  const xs = Array.from(new Set(groups.flatMap((g) => g.x))).sort((a, b) => a - b);
  const index = new Map(xs.map((x, i) => [x, i]));
  const out: (number | null)[][] = [xs];
  for (const g of groups) {
    for (const y of g.ys) {
      const col: (number | null)[] = new Array(xs.length).fill(null);
      g.x.forEach((x, i) => (col[index.get(x)!] = y[i] ?? null));
      out.push(col);
    }
  }
  return out;
}

/** Local midnight today as Unix seconds (browser time zone). */
export function todayStart(): number {
  const d = new Date();
  d.setHours(0, 0, 0, 0);
  return Math.floor(d.getTime() / 1000);
}
