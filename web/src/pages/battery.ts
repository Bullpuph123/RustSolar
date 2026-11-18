import { get } from '../api';
import { COLORS, ChartSet, pctVal } from '../charts';
import { estimateHead, every, merge, notesList } from '../common';
import { card, h, replace, s, stat } from '../dom';
import { dateTime, hours, kw, kwh, num, pct, pctv, time } from '../fmt';
import { store } from '../store';
import type { StoredSample } from '../types';

function gauge(soc: number | null, reserve: number | null): SVGElement {
  const r = 70;
  const c = 2 * Math.PI * r;
  const frac = soc === null ? 0 : Math.max(0, Math.min(1, soc / 100));
  const svg = s('svg', { viewBox: '0 0 180 180', class: 'gauge', role: 'img', 'aria-label': `Battery ${soc ?? '—'} percent` });
  svg.appendChild(s('circle', { cx: 90, cy: 90, r, class: 'gauge-track' }));
  svg.appendChild(s('circle', { cx: 90, cy: 90, r, class: 'gauge-fill', 'stroke-dasharray': `${c * frac} ${c}`, transform: 'rotate(-90 90 90)' }));
  if (reserve !== null) {
    const a = (reserve / 100) * 2 * Math.PI - Math.PI / 2;
    svg.appendChild(s('line', { x1: 90 + (r - 12) * Math.cos(a), y1: 90 + (r - 12) * Math.sin(a), x2: 90 + (r + 12) * Math.cos(a), y2: 90 + (r + 12) * Math.sin(a), class: 'reserve-tick' }, s('title', {}, `Reserve ${reserve}%`)));
  }
  svg.appendChild(s('text', { x: 90, y: 96, class: 'gauge-text', 'text-anchor': 'middle' }, soc === null ? '—' : `${soc.toFixed(0)}%`));
  svg.appendChild(s('text', { x: 90, y: 120, class: 'gauge-sub', 'text-anchor': 'middle' }, reserve === null ? '' : `reserve ${reserve.toFixed(0)}%`));
  return svg;
}

export function batteryPage(root: HTMLElement) {
  const charts = new ChartSet();
  const gaugeEl = h('div', { class: 'gauge-wrap' });
  const nowEl = h('div', { class: 'stats' });
  const outlookEl = h('div');
  const outageEl = h('div');
  const healthEl = h('div');
  const socChart = charts.add({
    height: 260,
    yLabel: 'SOC %',
    yRange: [0, 100],
    series: [
      { label: 'Measured SOC', color: COLORS.battery, width: 2, value: pctVal, fill: 0.12 },
      { label: 'Simulated SOC (estimate)', color: COLORS.battery, dash: [6, 4], width: 2, value: pctVal, span: true },
    ],
    data: [[], [], []],
  });
  const trendChart = charts.add({
    height: 180,
    yLabel: 'kWh',
    series: [{ label: 'Implied capacity', color: COLORS.battery, points: true, value: (v) => (v === null ? '—' : `${v.toFixed(2)} kWh`) }],
    data: [[], []],
  });
  root.append(
    h('div', { class: 'grid-2 wide-right' }, card('State of charge', gaugeEl, nowEl), h('section', { class: 'card' }, estimateHead('Outlook'), outlookEl)),
    h('section', { class: 'card' }, estimateHead('SOC: last 24 h and next 48 h'), socChart.el),
    h('div', { class: 'grid-2' }, h('section', { class: 'card' }, estimateHead('Outage planning'), outageEl), h('section', { class: 'card' }, h('h2', null, 'Battery health (calculated)'), healthEl, trendChart.el)),
  );

  const renderLive = () => {
    const st = store.live?.status;
    const b = store.intel?.battery;
    replace(gaugeEl, gauge(st?.soc_pct ?? null, st?.backup_soc_pct ?? null));
    const p = st?.storage_w ?? 0;
    replace(
      nowEl,
      stat('State', st ? (p < -20 ? 'Charging' : p > 20 ? 'Discharging' : 'Idle') : '—', 'Battery state now'),
      stat('Power', `${kw(st ? Math.abs(p) : null)} kW`, 'Charge or discharge power now'),
      stat('Usable capacity', b?.usable_wh ? `${kwh(b.usable_wh)} kWh` : '—', b?.capacity_source ?? 'Usable battery energy', h('span', { class: 'small muted' }, b?.capacity_source ?? '')),
      stat('Max power', b?.max_power_w ? `${kw(b.max_power_w, 1)} kW` : '—', b?.power_source ?? 'Continuous power limit', h('span', { class: 'small muted' }, b?.power_source ?? '')),
      b?.units ? stat('Units', String(b.units), 'Battery units in the gateway inventory') : null,
      st?.enc_agg_energy != null ? stat('Stored energy', `${kwh(st.enc_agg_energy)} kWh`, 'Gateway-reported aggregate energy (enc_agg_energy); treated as unverified') : null,
    );
  };
  const renderIntel = () => {
    renderLive();
    const b = store.intel?.battery;
    if (!b) {
      replace(outlookEl, h('div', { class: 'muted small' }, 'Calculating…'));
      return;
    }
    replace(
      outlookEl,
      h(
        'div',
        { class: 'stats' },
        stat('Full at', b.time_to_full ? time(b.time_to_full) : b.soc_pct !== null && b.soc_pct >= 99.5 ? 'Full' : 'Not expected', 'When the battery is expected to reach 100 % (next 48 h)'),
        stat('Reserve at', b.time_to_reserve ? dateTime(b.time_to_reserve) : 'Not expected', 'When the battery is expected to reach the backup reserve (next 48 h)'),
        stat('SOC at sunset', pctv(b.soc_at_sunset), 'Simulated charge at today’s sunset'),
        stat('SOC at sunrise', pctv(b.soc_at_sunrise), 'Simulated charge at the next sunrise'),
        stat('Grid import 24 h', `${kwh(b.expected_import_wh_24h)} kWh`, 'Expected energy bought from the grid in the next 24 h'),
        stat('Surplus export 24 h', `${kwh(b.expected_export_wh_24h)} kWh`, 'Expected surplus solar sent to the grid in the next 24 h'),
        stat('Overnight from battery', `${kwh(b.overnight_battery_wh)} kWh`, 'Expected battery contribution between sunset and sunrise'),
      ),
      notesList('Assumptions', b.assumptions),
    );
    replace(
      outageEl,
      b.outage.length
        ? h(
            'table',
            null,
            h('tr', null, h('th', null, 'Scenario'), h('th', { class: 'r' }, 'Runtime'), h('th', null, 'Notes')),
            ...b.outage.map((o) => h('tr', null, h('td', null, o.name), h('td', { class: 'r num' }, o.hours === null ? (o.note.includes('full') ? '> 48 h' : '—') : hours(o.hours)), h('td', { class: 'small muted' }, o.note))),
          )
        : h('div', { class: 'muted small' }, 'Needs battery capacity and power limit.'),
      h('p', { class: 'small muted' }, 'Starting from the current charge. Not a guaranteed backup time.'),
    );
    const hl = b.health;
    replace(
      healthEl,
      h(
        'div',
        { class: 'stats' },
        stat('Charged', `${num(hl.charge_kwh, 1)} kWh`, 'Total energy into the battery since monitoring began'),
        stat('Discharged', `${num(hl.discharge_kwh, 1)} kWh`, 'Total energy out of the battery since monitoring began'),
        stat('Equivalent cycles', num(hl.equivalent_cycles, 1), 'Discharge throughput ÷ usable capacity'),
        stat('Round-trip (observed)', pct(hl.observed_round_trip), 'Discharge ÷ charge over 14 days when SOC returned to its start; blank until measurable'),
        stat('Implied capacity', hl.implied_capacity_wh ? `${kwh(hl.implied_capacity_wh, 2)} kWh` : '—', 'Gateway stored energy ÷ SOC (median, last 3 days)'),
        stat('Since', hl.since ? dateTime(hl.since) : '—', 'First hour with data'),
      ),
      h('p', { class: 'small muted' }, hl.note),
    );
    const enough = hl.capacity_trend.length >= 2;
    trendChart.el.hidden = !enough;
    if (!enough) healthEl.appendChild(h('p', { class: 'small muted' }, 'Capacity trend appears after two weeks of readings above 30 % charge.'));
    else trendChart.setData([hl.capacity_trend.map((x) => x[0]), hl.capacity_trend.map((x) => x[1] / 1000)]);
    void loadSoc();
  };
  const loadSoc = async () => {
    const now = Math.floor(store.now());
    const r = await get<{ series: StoredSample[] }>(`/history?from=${now - 86400}&to=${now}&points=300`);
    const sim = store.intel?.battery.sim ?? [];
    socChart.update({
      marker: now,
      xRange: [now - 86400, now + 48 * 3600],
      data: merge([
        { x: r.series.map((x) => x.ts), ys: [r.series.map((x) => x.soc)] },
        { x: sim.map((x) => x[0]), ys: [sim.map((x) => x[1])] },
      ]) as never,
    });
  };
  const offs = [store.onLive(renderLive), store.onIntel(renderIntel)];
  renderIntel();
  const stop = every(120000, () => loadSoc().catch(() => {}));
  return () => {
    offs.forEach((f) => f());
    stop();
    charts.destroy();
  };
}
