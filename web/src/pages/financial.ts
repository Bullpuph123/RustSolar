import { get } from '../api';
import { COLORS, ChartSet } from '../charts';
import { every } from '../common';
import { card, empty, h, note, replace, stat } from '../dom';
import { money, num } from '../fmt';
import type { CostResp } from '../types';

interface TariffDay {
  enabled: boolean;
  date?: string;
  schedule?: [number, string, number, number][];
  now?: { plan: string; season: string; period: string; import_per_kwh: number; export_per_kwh: number } | null;
  currency?: string;
}

export function financialPage(root: HTMLElement) {
  const charts = new ChartSet();
  const body = h('div');
  root.append(body);
  let range = 30;

  const load = async () => {
    const [c, t] = await Promise.all([get<CostResp>(`/costs?days=${range}`), get<TariffDay>('/tariff/day')]);
    charts.destroy();
    if (!c.enabled || !c.report) {
      replace(
        body,
        card(
          'Cost estimates',
          empty(
            'Enter your electricity rates to see cost and savings estimates.',
            h('p', { class: 'small' }, 'RustSolar never assumes utility prices. Copy the time-of-use rates, fixed charges and export-credit rules from your PG&E bill or tariff sheet.'),
            h('a', { class: 'button primary', href: '#/settings', title: 'Open Settings → Tariff' }, 'Enter rates in Settings'),
          ),
        ),
      );
      return;
    }
    const r = c.report;
    const cur = r.currency;
    const tt = r.totals;
    const m = (v: number) => money(v, cur);
    const rangeSel = h(
      'select',
      { title: 'Period to analyse', onchange: (e: Event) => { range = Number((e.target as HTMLSelectElement).value); void load(); } },
      ...[7, 30, 90, 365].map((d) => h('option', { value: d }, d === 365 ? 'Last 12 months' : `Last ${d} days`)),
    );
    rangeSel.value = String(range);
    const daily = charts.add({
      height: 260,
      yLabel: cur,
      xFormat: 'date',
      zero: true,
      series: [
        { label: 'Net cost', color: COLORS.grid, bars: true, value: (v) => (v === null ? '—' : m(v)) },
        { label: 'Without solar/battery', color: COLORS.muted, dash: [5, 4], points: true, value: (v) => (v === null ? '—' : m(v)) },
      ],
      data: [[], [], []],
    });
    const days = r.days;
    const toTs = (d: string) => new Date(d + 'T12:00:00').getTime() / 1000;
    daily.update({ xRange: [Date.now() / 1000 - range * 86400, Date.now() / 1000 + 86400] });
    daily.setData([days.map((d) => toTs(d.date)), days.map((d) => d.net_cost), days.map((d) => d.no_solar_cost + d.fixed_charge)]);
    const sched = t.schedule ?? [];
    replace(
      body,
      note('info', 'Estimates from measured energy and the rates you entered. Your utility bill is authoritative.'),
      h('div', { class: 'row' }, h('label', { title: 'Period' }, 'Period ', rangeSel), h('div', { class: 'spacer' }), h('a', { href: `/api/export/csv?kind=costs&days=${range}`, title: 'Download hourly cost lines for reconciliation with your bill' }, 'Download cost CSV')),
      card(
        'Totals',
        h(
          'div',
          { class: 'stats' },
          stat('Net cost', m(tt.net_cost), 'Imports + fixed + demand − export credits − baseline credits'),
          stat('Grid purchases', m(tt.import_cost + tt.tier_adder_cost), `${num(tt.import_kwh, 1)} kWh imported`),
          stat('Export credit', m(tt.export_credit), `${num(tt.export_kwh, 1)} kWh exported`),
          stat('Fixed charges', m(tt.fixed_charge), 'Daily fixed charges for days with data'),
          tt.baseline_credit ? stat('Baseline credit', m(tt.baseline_credit), 'Baseline allowance credit') : null,
          tt.demand_charge ? stat('Demand charges', m(tt.demand_charge), 'Demand charges') : null,
          stat('Avoided purchases', m(tt.avoided_cost), 'Value of home use supplied by solar and battery at the time-of-use price'),
          stat('Without solar/battery', m(tt.no_solar_cost + tt.fixed_charge), 'Estimated cost if all home use came from the grid'),
          stat('Estimated benefit', m(tt.no_solar_cost + tt.fixed_charge - tt.net_cost), 'Without-solar cost minus net cost'),
        ),
        r.notes.length ? h('ul', { class: 'small muted' }, ...r.notes.map((n) => h('li', null, n))) : null,
      ),
      card('Daily cost', daily.el),
      card(
        `Rates today${t.now ? ` — now: ${t.now.period} (${t.now.season})` : ''}`,
        sched.length
          ? h(
              'div',
              { class: 'tou' },
              ...sched.map(([hr, period, imp, exp]) =>
                h('div', { class: `tou-h p-${period.replace(/[^a-z]/gi, '').toLowerCase()}`, title: `${hr}:00 ${period}: import ${m(imp)}/kWh, export ${m(exp)}/kWh` }, h('div', { class: 'tou-hour' }, String(hr)), h('div', { class: 'tou-p' }, period), h('div', { class: 'tou-r num' }, imp.toFixed(2))),
              ),
            )
          : h('div', { class: 'muted small' }, 'No rate schedule for today.'),
        h('p', { class: 'small muted' }, 'Import price per kWh by local hour. Hover for export credit.'),
      ),
    );
  };
  const stop = every(600000, () => load().catch((e) => replace(body, note('err', String(e)))));
  return () => {
    stop();
    charts.destroy();
  };
}
