import { get } from '../api';
import { ChartSet } from '../charts';
import { alertItem, every, kpiTiles, powerChart, powerData, recCard, sunStrip } from '../common';
import { card, h, replace, stat } from '../dom';
import { FlowDiagram } from '../flow';
import { kw, kwh, pct, time } from '../fmt';
import { store } from '../store';
import type { AlertRow, ConfigResp, Overview, StoredSample } from '../types';

export function overviewPage(root: HTMLElement) {
  const charts = new ChartSet();
  const tiles = kpiTiles();
  const flow = new FlowDiagram();
  const lastEl = h('div', { class: 'small muted flow-foot' });
  const todayEl = h('div');
  const sunEl = h('div');
  const recsEl = h('div', { class: 'recs' });
  const alertsEl = h('div');
  const setupEl = h('div');
  const chart = powerChart(charts, 260);

  root.append(
    tiles.el,
    h('div', { class: 'grid-2 wide-left' }, card('Energy flow', flow.el, lastEl), h('div', { class: 'stack' }, card('Today', todayEl), card('Sun', sunEl))),
    setupEl,
    h('div', { class: 'grid-2' }, card('Recommendations', recsEl), card('Active alerts', alertsEl)),
    card('Last 24 hours', chart.el),
  );

  const onLive = () => {
    const l = store.live;
    tiles.update(l);
    flow.update(l);
    const s = l?.status;
    replace(
      lastEl,
      s ? `Gateway reading at ${time(s.last_update)}` : 'Waiting for the first gateway reading',
      l?.residual_w !== null && l?.residual_w !== undefined && s ? ` · balance ${l.residual_w.toFixed(0)} W` : '',
      l && l.flows.unexplained_w > 50 ? ` · ${kw(l.flows.unexplained_w)} kW unattributed (metering timing)` : '',
    );
  };
  const onIntel = () => {
    const i = store.intel;
    replace(sunEl, sunStrip(i?.sun_today ?? null, store.now()));
    const recs = i?.recommendations ?? [];
    replace(recsEl, recs.length ? recs.slice(0, 3).map(recCard) : h('div', { class: 'muted small' }, 'No suggestions right now.'), recs.length > 3 ? h('a', { href: '#/recommendations', title: 'See all recommendations' }, `All ${recs.length} →`) : null);
    void renderSetup();
  };

  async function renderSetup() {
    try {
      const c = await get<ConfigResp>('/config');
      const items: [boolean, string][] = [
        [c.setup.gateway_host, 'Gateway address'],
        [c.setup.cert_pinned, 'Gateway certificate pinned'],
        [c.setup.token, 'Gateway token'],
        [c.setup.location, 'Location (sun times, weather, forecasts)'],
        [c.setup.arrays, 'Solar array specs (physical forecast)'],
        [c.setup.tariff, 'Electricity rates (cost estimates)'],
      ];
      const needs = store.intel?.needs ?? [];
      if (items.every((x) => x[0]) && !needs.length) {
        replace(setupEl);
        return;
      }
      replace(
        setupEl,
        card(
          'Setup',
          h('ul', { class: 'checklist' }, ...items.map(([ok, label]) => h('li', { class: ok ? 'done' : '' }, ok ? '✓ ' : '○ ', label))),
          needs.length ? h('ul', { class: 'small muted' }, ...needs.map((n) => h('li', null, n))) : null,
          h('a', { href: '#/settings', title: 'Open the setup wizard' }, 'Open Settings →'),
        ),
      );
    } catch {
      /* ignore */
    }
  }

  const loadToday = async () => {
    const o = await get<Overview>('/overview');
    const p = o.periods[0];
    if (!p) return;
    const e = p.energy;
    replace(
      todayEl,
      h(
        'div',
        { class: 'stats' },
        stat('Solar', `${kwh(e.pv_wh)} kWh`, 'Solar energy produced today (measured)'),
        stat('Home', `${kwh(e.load_wh)} kWh`, 'Household energy used today (measured)'),
        stat('Imported', `${kwh(e.import_wh)} kWh`, 'Energy bought from the grid today'),
        stat('Exported', `${kwh(e.export_wh)} kWh`, 'Energy sent to the grid today'),
        stat('Self-consumption', pct(p.self_consumption), 'Share of solar used at home or stored in the battery'),
        stat('Grid independence', pct(p.grid_independence), 'Share of household use not supplied by the grid'),
        stat('Battery share', pct(p.battery_share), 'Share of household use supplied by the battery'),
        stat('Peak solar', `${kw(o.today_peak_pv_w)} kW`, 'Highest solar reading today'),
        stat('Peak demand', `${kw(o.today_peak_load_w)} kW`, 'Highest household reading today'),
      ),
      store.intel?.solar?.rest_of_today_wh !== undefined && store.intel?.solar?.rest_of_today_wh !== null
        ? h('div', { class: 'small muted' }, `Estimate: about ${kwh(store.intel.solar.rest_of_today_wh)} kWh more solar expected today.`)
        : null,
    );
  };
  const loadChart = async () => {
    const r = await get<{ series: StoredSample[] }>('/history?range=24h&points=400');
    chart.setData(powerData(r.series));
  };
  const loadAlerts = async () => {
    const r = await get<{ alerts: AlertRow[] }>('/alerts');
    replace(alertsEl, r.alerts.length ? r.alerts.slice(0, 4).map((a) => alertItem(a, loadAlerts)) : h('div', { class: 'muted small' }, 'No active alerts.'));
  };

  const offs = [store.onLive(onLive), store.onIntel(onIntel)];
  onLive();
  onIntel();
  const stops = [every(60000, () => loadToday().catch(() => {})), every(60000, () => loadChart().catch(() => {})), every(60000, () => loadAlerts().catch(() => {}))];
  return () => {
    offs.forEach((f) => f());
    stops.forEach((f) => f());
    charts.destroy();
  };
}
