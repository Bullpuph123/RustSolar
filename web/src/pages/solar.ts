import { get } from '../api';
import { COLORS, ChartSet, kwVal } from '../charts';
import { estimateHead, every, merge, notesList, todayStart } from '../common';
import { card, empty, h, replace, stat } from '../dom';
import { kw, kwh, num } from '../fmt';
import { store } from '../store';
import type { Overview, StoredSample } from '../types';

interface SunResp {
  now: { elevation_deg: number; azimuth_deg: number; declination_deg: number; equation_of_time_min: number };
  path: [number, number, number][];
}

export function solarPage(root: HTMLElement) {
  const charts = new ChartSet();
  const statsEl = h('div', { class: 'stats' });
  const fcInfo = h('div');
  const today = charts.add({
    height: 300,
    yLabel: 'kW',
    series: [
      { label: 'Measured', color: COLORS.solar, fill: 0.2, width: 2, value: kwVal },
      { label: 'Forecast', color: COLORS.muted, dash: [6, 4], width: 1.6, value: kwVal, span: true },
      { label: 'Forecast high', color: COLORS.muted, width: 0, value: kwVal, span: true },
      { label: 'Forecast low', color: COLORS.muted, width: 0, value: kwVal, span: true },
    ],
    bands: [{ series: [3, 4], color: COLORS.solar }],
    data: [[], [], [], [], []],
  });
  const sunChart = charts.add({
    height: 220,
    yLabel: 'Elevation °',
    series: [{ label: 'Sun elevation', color: COLORS.solar, width: 2, value: (v) => (v === null ? '—' : `${v.toFixed(1)}°`) }],
    data: [[], []],
  });
  const sunInfo = h('div', { class: 'small muted' });
  const periodsEl = h('div', { class: 'stats' });
  root.append(
    h('div', { class: 'stats-card card' }, statsEl),
    h('section', { class: 'card' }, estimateHead('Today: measured vs forecast'), today.el, fcInfo),
    h('div', { class: 'grid-2' }, card('Production totals', periodsEl), h('section', { class: 'card' }, h('h2', null, 'Sun path today'), sunChart.el, sunInfo)),
  );

  const renderStats = () => {
    const s = store.live?.status;
    const i = store.intel;
    replace(
      statsEl,
      stat('Now', `${kw(s ? Math.max(0, s.pv_w) : null)} kW`, 'Solar power right now (measured)'),
      stat('Next hour', i?.solar?.next_hour_wh != null ? `${kwh(i.solar.next_hour_wh, 2)} kWh` : '—', 'Estimated energy over the next 60 minutes'),
      stat('Rest of today', i?.solar?.rest_of_today_wh != null ? `${kwh(i.solar.rest_of_today_wh)} kWh` : '—', 'Estimated solar energy still to come today'),
      stat('Tomorrow', i?.solar?.tomorrow_wh != null ? `${kwh(i.solar.tomorrow_wh)} kWh` : '—', 'Estimated solar energy tomorrow'),
    );
  };

  const load = async () => {
    const t0 = todayStart();
    const now = Math.floor(store.now());
    const [hist, ov] = await Promise.all([get<{ series: StoredSample[] }>(`/history?from=${t0}&to=${now}&points=288`), get<Overview>('/overview')]);
    const fc = (store.intel?.solar?.hours ?? []).filter((x) => x.ts >= t0 && x.ts < t0 + 86400);
    const fx = fc.map((x) => x.ts + 1800);
    today.update({ xRange: [t0, t0 + 86400] });
    today.setData(
      merge([
        { x: hist.series.map((x) => x.ts), ys: [hist.series.map((x) => Math.max(0, x.pv_w) / 1000)] },
        { x: fx, ys: [fc.map((x) => x.wh / 1000), fc.map((x) => x.hi / 1000), fc.map((x) => x.lo / 1000)] },
      ]) as never,
    );
    const sf = store.intel?.solar;
    replace(
      fcInfo,
      sf ? h('div', { class: 'small muted' }, `Model ${sf.model} · confidence: ${sf.confidence}. Forecast is the hourly average power; the band is the ~80 % range.`) : h('div', { class: 'small muted' }, 'No forecast yet — needs location and weather (and array specs for the physical model).'),
      notesList('Notes', sf?.notes),
    );
    const g = ov.gateway_totals;
    replace(
      periodsEl,
      ...ov.periods.map((p) => stat(p.name === 'all' ? 'Since monitoring began' : p.name[0].toUpperCase() + p.name.slice(1), `${kwh(p.energy.pv_wh)} kWh`, `Measured solar energy (${p.name})`)),
      g?.pv_lifetime_wh ? stat('System lifetime', `${(g.pv_lifetime_wh / 1e6).toFixed(2)} MWh`, 'Lifetime production reported by the gateway') : null,
      g?.inverter_count ? stat('Microinverters', String(g.inverter_count), 'Active microinverters reported by the gateway') : null,
      stat('Peak today', `${kw(ov.today_peak_pv_w)} kW`, 'Highest solar reading today'),
    );
  };
  const loadSun = async () => {
    try {
      const r = await get<SunResp>('/sun');
      sunChart.update({ data: [r.path.map((p) => p[0]), r.path.map((p) => p[1])], marker: Math.floor(store.now()), yRange: [-20, 90] });
      replace(sunInfo, `Now: elevation ${num(r.now.elevation_deg)}°, azimuth ${num(r.now.azimuth_deg)}° · declination ${num(r.now.declination_deg, 2)}° · equation of time ${num(r.now.equation_of_time_min)} min. Calculated locally (NOAA equations).`);
    } catch {
      replace(sunInfo, empty('Set your location in Settings to see the sun path.'));
    }
  };
  const offs = [store.onLive(renderStats), store.onIntel(() => { renderStats(); void load().catch(() => {}); })];
  renderStats();
  const stops = [every(120000, () => load().catch(() => {})), every(600000, loadSun)];
  return () => {
    offs.forEach((f) => f());
    stops.forEach((f) => f());
    charts.destroy();
  };
}
