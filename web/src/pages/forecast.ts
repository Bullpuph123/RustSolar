import { get } from '../api';
import { COLORS, ChartSet, kwVal, kwhVal } from '../charts';
import { estimateHead, every, merge, notesList } from '../common';
import { empty, h, replace } from '../dom';
import { dayLabel, kwh, num, time } from '../fmt';
import { store } from '../store';
import type { Accuracy, AccuracyResp } from '../types';

function accRow(name: string, a: Accuracy) {
  return h(
    'tr',
    null,
    h('td', null, name),
    h('td', { class: 'r num' }, String(a.n)),
    h('td', { class: 'r num' }, a.mae_wh === null ? '—' : `${num(a.mae_wh, 0)} Wh`),
    h('td', { class: 'r num' }, a.rmse_wh === null ? '—' : `${num(a.rmse_wh, 0)} Wh`),
    h('td', { class: 'r num' }, a.bias_wh === null ? '—' : `${a.bias_wh > 0 ? '+' : ''}${num(a.bias_wh, 0)} Wh`),
    h('td', { class: 'r num' }, a.mape_pct === null ? '—' : `${num(a.mape_pct, 0)} %`),
    h('td', { class: 'r num' }, a.daily_mape_pct === null ? '—' : `${num(a.daily_mape_pct, 0)} %`),
  );
}

export function forecastPage(root: HTMLElement) {
  const charts = new ChartSet();
  const daysEl = h('div', { class: 'day-cards' });
  const infoEl = h('div');
  const accEl = h('div');
  const hourly = charts.add({
    height: 300,
    yLabel: 'kW (hourly average)',
    series: [
      { label: 'Solar', color: COLORS.solar, width: 2, value: kwVal, fill: 0.12 },
      { label: 'Solar high', color: COLORS.solar, width: 0, value: kwVal },
      { label: 'Solar low', color: COLORS.solar, width: 0, value: kwVal },
      { label: 'Home use', color: COLORS.home, width: 1.6, dash: [6, 4], value: kwVal, span: true },
    ],
    bands: [{ series: [2, 3], color: COLORS.solar }],
    data: [[], [], [], [], []],
  });
  const fva = charts.add({
    height: 240,
    yLabel: 'kWh per hour',
    series: [
      { label: 'Forecast (≥1 h ahead)', color: COLORS.muted, dash: [6, 4], value: kwhVal },
      { label: 'Measured', color: COLORS.solar, width: 2, value: kwhVal },
    ],
    data: [[], [], []],
  });
  root.append(
    h('section', { class: 'card' }, estimateHead('Solar: next days'), daysEl, infoEl),
    h('section', { class: 'card' }, estimateHead('Hourly forecast'), hourly.el),
    h('section', { class: 'card' }, h('h2', null, 'Forecast accuracy (last 30 days)'), accEl, fva.el),
  );

  const render = () => {
    const i = store.intel;
    const sf = i?.solar;
    if (!sf) {
      replace(daysEl, empty('No solar forecast yet.', h('div', { class: 'small' }, (i?.needs ?? []).join(' ') || 'Needs a location and a weather forecast.')));
      replace(infoEl);
    } else {
      replace(
        daysEl,
        ...sf.days.map((d) =>
          h(
            'div',
            { class: 'day-card', title: `Range ${d.lo_kwh.toFixed(1)}–${d.hi_kwh.toFixed(1)} kWh; forecast covers ${(d.coverage * 100).toFixed(0)} % of daylight` },
            h('div', { class: 'day-name' }, dayLabel(d.date)),
            h('div', { class: 'day-kwh num' }, `${d.kwh.toFixed(1)}`, h('span', { class: 'unit' }, ' kWh')),
            h('div', { class: 'small muted num' }, `${d.lo_kwh.toFixed(1)}–${d.hi_kwh.toFixed(1)}`),
            h('div', { class: 'small muted' }, d.peak_ts ? `peak ${(d.peak_w / 1000).toFixed(1)} kW ~${time(d.peak_ts)}` : ''),
            d.coverage < 0.9 ? h('div', { class: 'small warn-text' }, 'partial day') : null,
          ),
        ),
      );
      replace(
        infoEl,
        h('div', { class: 'small muted' }, `Model ${sf.model} · confidence: ${sf.confidence} · learned from ${sf.learned_hours} daylight hours.`),
        i?.load ? h('div', { class: 'small muted' }, `Household tomorrow: ${kwh(i.load.tomorrow_wh)} kWh (estimate).`) : null,
        notesList('Notes', sf.notes),
      );
    }
    const sh = sf?.hours ?? [];
    const lh = i?.load?.hours ?? [];
    hourly.update({
      marker: Math.floor(store.now()),
      data: merge([
        { x: sh.map((x) => x.ts + 1800), ys: [sh.map((x) => x.wh / 1000), sh.map((x) => x.hi / 1000), sh.map((x) => x.lo / 1000)] },
        { x: lh.map((x) => x.ts + 1800), ys: [lh.map((x) => x.wh / 1000)] },
      ]) as never,
    });
  };
  const loadAcc = async () => {
    const a = await get<AccuracyResp>('/accuracy?days=30');
    if (!a.pv_hour_ahead.n && !a.load_hour_ahead.n) {
      replace(accEl, empty('No forecast has been checked against measurements yet.', h('div', { class: 'small' }, 'Accuracy appears once forecasts made before an hour can be compared with what was measured in that hour.')));
    } else {
      replace(
        accEl,
        h(
          'table',
          null,
          h('tr', null, h('th', null, 'Forecast'), h('th', { class: 'r' }, 'Hours'), h('th', { class: 'r' }, 'MAE'), h('th', { class: 'r' }, 'RMSE'), h('th', { class: 'r' }, 'Bias'), h('th', { class: 'r' }, 'MAPE'), h('th', { class: 'r' }, 'Daily MAPE')),
          accRow('Solar, ≥1 h ahead', a.pv_hour_ahead),
          accRow('Solar, ≥12 h ahead', a.pv_day_ahead),
          accRow('Home use, ≥1 h ahead', a.load_hour_ahead),
        ),
        a.pv_hour_ahead.by_sky.length
          ? h('div', { class: 'small muted' }, 'Solar MAE by sky: ', a.pv_hour_ahead.by_sky.map(([k, v, n]) => `${k} ${v.toFixed(0)} Wh (${n} h)`).join(' · '))
          : null,
        h('p', { class: 'small muted' }, a.note, ' MAPE uses hours with at least 200 Wh measured. Positive bias = forecast too high.'),
      );
    }
    fva.el.hidden = a.pv_series.length === 0;
    fva.setData([a.pv_series.map((p) => p[0] + 1800), a.pv_series.map((p) => p[1] / 1000), a.pv_series.map((p) => p[2] / 1000)]);
  };
  const off = store.onIntel(render);
  render();
  const stop = every(600000, () => loadAcc().catch(() => {}));
  return () => {
    off();
    stop();
    charts.destroy();
  };
}
