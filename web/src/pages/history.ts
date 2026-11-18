import { get } from '../api';
import { COLORS, ChartSet, kwhVal } from '../charts';
import { powerChart, powerData } from '../common';
import { card, h, replace } from '../dom';
import { dayLabel, kwh, pct } from '../fmt';
import { store } from '../store';
import type { DayRow, StoredSample } from '../types';

const RANGES: [string, string, number][] = [
  ['24h', '24 hours', 86400],
  ['7d', '7 days', 7 * 86400],
  ['30d', '30 days', 30 * 86400],
  ['12m', '12 months', 365 * 86400],
];

const toInput = (ts: number) => {
  const d = new Date(ts * 1000);
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
};
const fromInput = (v: string) => new Date(v + 'T00:00:00').getTime() / 1000;

export function historyPage(root: HTMLElement) {
  const charts = new ChartSet();
  let from = Math.floor(store.now()) - 86400;
  let to = Math.floor(store.now());
  let active = '24h';
  const power = powerChart(charts, 300);
  const pvBars = charts.add({
    height: 240,
    yLabel: 'kWh',
    xFormat: 'date',
    zero: true,
    series: [
      { label: 'Solar', color: COLORS.solar, bars: true, barAlign: -1, barSize: 0.4, value: kwhVal },
      { label: 'Home', color: COLORS.home, bars: true, barAlign: 1, barSize: 0.4, value: kwhVal },
    ],
    data: [[], [], []],
  });
  const gridBars = charts.add({
    height: 240,
    yLabel: 'kWh',
    xFormat: 'date',
    zero: true,
    series: [
      { label: 'Imported', color: COLORS.grid, bars: true, barAlign: -1, barSize: 0.4, value: kwhVal },
      { label: 'Exported', color: COLORS.solar, bars: true, barAlign: 1, barSize: 0.4, value: kwhVal },
    ],
    data: [[], [], []],
  });
  const btns = new Map<string, HTMLElement>();
  const fromIn = h('input', { type: 'date', title: 'Start date (local)', value: toInput(from) });
  const toIn = h('input', { type: 'date', title: 'End date (local, inclusive)', value: toInput(to) });
  const rangeRow = h('div', { class: 'row seg-buttons' });
  for (const [id, label, span] of RANGES) {
    const b = h('button', { title: `Show the last ${label}`, onclick: () => { active = id; to = Math.floor(store.now()); from = to - span; void load(); } }, label);
    btns.set(id, b);
    rangeRow.appendChild(b);
  }
  rangeRow.append(
    h('span', { class: 'muted small' }, 'or'),
    fromIn,
    toIn,
    h('button', { class: 'primary', title: 'Show the chosen dates', onclick: () => { if (!fromIn.value || !toIn.value) return; active = 'custom'; from = fromInput(fromIn.value); to = fromInput(toIn.value) + 86400; void load(); } }, 'Show'),
  );
  const exportsEl = h('div', { class: 'row small' });
  const tableEl = h('div');
  const barsTitle = h('h2', null, 'Daily energy');
  root.append(card(null, rangeRow, exportsEl), card('Power', power.el), h('section', { class: 'card' }, barsTitle, pvBars.el, gridBars.el), card('Daily totals', tableEl));

  const load = async () => {
    btns.forEach((b, k) => b.classList.toggle('selected', k === active));
    fromIn.value = toInput(from);
    toIn.value = toInput(to - 1);
    const monthly = to - from > 100 * 86400;
    replace(barsTitle, monthly ? 'Monthly energy' : 'Daily energy');
    const [p, d] = await Promise.all([get<{ series: StoredSample[] }>(`/history?from=${from}&to=${to}&points=800`), get<{ days: DayRow[] }>(`/daily?from=${from}&to=${to}${monthly ? '&group=month' : ''}`)]);
    power.setData(powerData(p.series));
    const days = d.days;
    const xs = days.map((x) => x.start + (monthly ? 15 * 86400 : 43200));
    pvBars.update({ xRange: [from, to] });
    gridBars.update({ xRange: [from, to] });
    power.update({ xRange: [from, to] });
    pvBars.setData([xs, days.map((x) => x.energy.pv_wh / 1000), days.map((x) => x.energy.load_wh / 1000)]);
    gridBars.setData([xs, days.map((x) => x.energy.import_wh / 1000), days.map((x) => x.energy.export_wh / 1000)]);
    const q = `from=${from}&to=${to}`;
    replace(
      exportsEl,
      'Export: ',
      h('a', { href: `/api/export/csv?kind=samples&${q}`, title: 'Every stored reading (raw samples are kept for the retention period)' }, 'Readings CSV'),
      h('a', { href: `/api/export/csv?kind=hourly&${q}`, title: 'Hourly energy with flow attribution' }, 'Hourly CSV'),
      h('a', { href: `/api/export/csv?kind=daily&${q}`, title: 'Daily totals' }, 'Daily CSV'),
      h('a', { href: `/api/export/csv?kind=costs&${q}`, title: 'Hourly cost estimates (needs rates)' }, 'Costs CSV'),
      h('a', { href: `/api/export/json?${q}`, title: 'Hourly energy as JSON' }, 'JSON'),
    );
    replace(
      tableEl,
      days.length
        ? h(
            'table',
            null,
            h('tr', null, h('th', null, monthly ? 'Month' : 'Day'), h('th', { class: 'r' }, 'Solar kWh'), h('th', { class: 'r' }, 'Home kWh'), h('th', { class: 'r' }, 'Import kWh'), h('th', { class: 'r' }, 'Export kWh'), h('th', { class: 'r' }, 'Self-use'), h('th', { class: 'r' }, 'Independence')),
            ...days
              .slice()
              .reverse()
              .map((x) =>
                h(
                  'tr',
                  null,
                  h('td', null, monthly ? new Date(x.date + 'T12:00:00').toLocaleDateString([], { month: 'long', year: 'numeric' }) : dayLabel(x.date)),
                  h('td', { class: 'r num' }, kwh(x.energy.pv_wh)),
                  h('td', { class: 'r num' }, kwh(x.energy.load_wh)),
                  h('td', { class: 'r num' }, kwh(x.energy.import_wh)),
                  h('td', { class: 'r num' }, kwh(x.energy.export_wh)),
                  h('td', { class: 'r num' }, pct(x.self_consumption)),
                  h('td', { class: 'r num' }, pct(x.grid_independence)),
                ),
              ),
          )
        : h('div', { class: 'muted small' }, 'No data in this range.'),
      h('p', { class: 'small muted' }, 'Energy is integrated from measured power over real elapsed gateway time; gaps longer than the configured limit are not filled in.'),
    );
  };
  void load();
  return () => charts.destroy();
}
