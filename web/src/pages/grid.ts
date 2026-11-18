import { get } from '../api';
import { COLORS, ChartSet, kwVal, kwhVal } from '../charts';
import { estimateHead, every, merge, notesList } from '../common';
import { card, h, replace, stat } from '../dom';
import { kw, kwh, num, pct, temp, tempDelta, tempUnit } from '../fmt';
import { store } from '../store';
import type { DayRow, Overview, StoredSample } from '../types';

export function gridPage(root: HTMLElement) {
  const charts = new ChartSet();
  const nowEl = h('div', { class: 'stats' });
  const todayEl = h('div', { class: 'stats' });
  const fcEl = h('div');
  const loadChart = charts.add({
    height: 280,
    yLabel: 'kW',
    series: [
      { label: 'Home (measured)', color: COLORS.home, width: 2, value: kwVal, fill: 0.12 },
      { label: 'Grid (+ import)', color: COLORS.grid, width: 1.4, value: kwVal },
      { label: 'Home forecast', color: COLORS.home, dash: [6, 4], width: 1.6, value: kwVal, span: true },
      { label: 'Forecast high', color: COLORS.home, width: 0, value: kwVal, span: true },
      { label: 'Forecast low', color: COLORS.home, width: 0, value: kwVal, span: true },
    ],
    bands: [{ series: [4, 5], color: COLORS.home }],
    data: [[], [], [], [], [], []],
  });
  const bars = charts.add({
    height: 260,
    yLabel: 'kWh',
    xFormat: 'date',
    zero: true,
    series: [
      { label: 'Imported', color: COLORS.grid, bars: true, barAlign: -1, barSize: 0.4, value: kwhVal },
      { label: 'Exported', color: COLORS.solar, bars: true, barAlign: 1, barSize: 0.4, value: kwhVal },
      { label: 'Home use', color: COLORS.home, width: 2, points: true, value: kwhVal },
    ],
    data: [[], [], [], []],
  });
  root.append(
    h('div', { class: 'grid-2' }, card('Right now', nowEl), card('Today', todayEl)),
    h('section', { class: 'card' }, estimateHead('Household use: last 24 h and forecast'), loadChart.el, fcEl),
    card('Daily grid energy (30 days)', bars.el),
  );

  const renderNow = () => {
    const st = store.live?.status;
    const f = store.live?.flows;
    replace(
      nowEl,
      stat('Home', `${kw(st?.load_w)} kW`, 'Household consumption now'),
      stat('Grid', st ? `${kw(Math.abs(st.grid_w))} kW ${st.grid_w > 20 ? 'import' : st.grid_w < -20 ? 'export' : ''}` : '—', 'Grid power now'),
      stat('From solar', `${kw(f?.pv_to_load)} kW`, 'Household load served by solar now'),
      stat('From battery', `${kw(f?.batt_to_load)} kW`, 'Household load served by the battery now'),
      stat('From grid', `${kw(f?.grid_to_load)} kW`, 'Household load served by the grid now'),
    );
  };
  const renderFc = () => {
    const l = store.intel?.load;
    replace(
      fcEl,
      l
        ? h(
            'div',
            null,
            h(
              'div',
              { class: 'stats' },
              stat('Rest of today', `${kwh(l.rest_of_today_wh)} kWh`, 'Estimated household use for the rest of today'),
              stat('Overnight', `${kwh(l.overnight_wh)} kWh`, 'Estimated use between sunset and sunrise'),
              stat('Tomorrow', `${kwh(l.tomorrow_wh)} kWh`, 'Estimated household use tomorrow'),
              stat('Tomorrow peak hour', `${kw(l.tomorrow_peak_w)} kW`, 'Highest hourly average expected tomorrow'),
              stat('History used', `${num(l.days_of_history, 1)} days`, 'Days of measured history behind this forecast'),
            ),
            l.temperature_term
              ? h('div', { class: 'small muted' }, `Temperature effect: +${num(l.temperature_term.cooling_wh_per_c / tempDelta(1), 0)} Wh/h per ${tempUnit()} above ${temp(22)}, +${num(l.temperature_term.heating_wh_per_c / tempDelta(1), 0)} Wh/h per ${tempUnit()} below ${temp(15)} (from ${l.temperature_term.n} hours).`)
              : null,
            h('div', { class: 'small muted' }, `Model ${l.model}. Patterns, not appliance detection.`),
            notesList('Notes', l.notes),
          )
        : h('div', { class: 'muted small' }, 'Household forecast needs about two days of history.'),
    );
  };
  const load = async () => {
    const now = Math.floor(store.now());
    const [hist, ov, daily] = await Promise.all([
      get<{ series: StoredSample[] }>(`/history?from=${now - 86400}&to=${now}&points=300`),
      get<Overview>('/overview'),
      get<{ days: DayRow[] }>('/daily?days=30'),
    ]);
    const lf = (store.intel?.load?.hours ?? []).filter((x) => x.ts < now + 48 * 3600);
    loadChart.update({
      marker: now,
      xRange: [now - 86400, now + 48 * 3600],
      data: merge([
        { x: hist.series.map((x) => x.ts), ys: [hist.series.map((x) => x.load_w / 1000), hist.series.map((x) => x.grid_w / 1000)] },
        { x: lf.map((x) => x.ts + 1800), ys: [lf.map((x) => x.wh / 1000), lf.map((x) => x.hi / 1000), lf.map((x) => x.lo / 1000)] },
      ]) as never,
    });
    const p = ov.periods[0];
    if (p) {
      const e = p.energy;
      replace(
        todayEl,
        stat('Home use', `${kwh(e.load_wh)} kWh`, 'Household energy today'),
        stat('Imported', `${kwh(e.import_wh)} kWh`, 'Bought from the grid today'),
        stat('Exported', `${kwh(e.export_wh)} kWh`, 'Sent to the grid today'),
        stat('Grid → battery', `${kwh(e.grid_to_batt_wh)} kWh`, 'Grid energy that charged the battery today'),
        stat('Grid independence', pct(p.grid_independence), 'Share of household use not from the grid'),
        stat('Peak demand', `${kw(ov.today_peak_load_w)} kW`, 'Highest household reading today'),
      );
    }
    const d = daily.days;
    bars.update({ xRange: [now - 30 * 86400, now + 86400] });
    bars.setData([d.map((x) => x.start + 43200), d.map((x) => x.energy.import_wh / 1000), d.map((x) => x.energy.export_wh / 1000), d.map((x) => x.energy.load_wh / 1000)]);
  };
  const offs = [store.onLive(renderNow), store.onIntel(() => { renderFc(); void load().catch(() => {}); })];
  renderNow();
  renderFc();
  const stop = every(120000, () => load().catch(() => {}));
  return () => {
    offs.forEach((f) => f());
    stop();
    charts.destroy();
  };
}
