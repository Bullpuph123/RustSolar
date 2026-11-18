// Wall display: no sidebar, huge readouts and the flow diagram. Cursor hides when idle.

import { get } from '../api';
import { sunStrip } from '../common';
import { h, replace } from '../dom';
import { FlowDiagram } from '../flow';
import { ago, kwh, pct, pctv } from '../fmt';
import { store } from '../store';
import type { Overview } from '../types';

export function wallPage(root: HTMLElement) {
  const flow = new FlowDiagram(true);
  const clock = h('div', { class: 'wall-clock num' });
  const big = h('div', { class: 'wall-big' });
  const today = h('div', { class: 'wall-today' });
  const sun = h('div', { class: 'wall-sun' });
  const status = h('div', { class: 'wall-status' });
  const recs = h('div', { class: 'wall-recs' });
  root.append(
    h(
      'div',
      { class: 'wall-top' },
      h('div', { class: 'brand' }, h('img', { src: '/logo.png', alt: 'Rocket Salvo' }), h('span', null, 'Rust', h('em', null, 'Solar'))),
      status,
      h('div', { class: 'spacer' }),
      clock,
      h('a', { href: '#/overview', class: 'wall-exit', title: 'Leave the wall display' }, '✕'),
    ),
    h('div', { class: 'wall-main' }, h('div', { class: 'wall-flow' }, flow.el), h('div', { class: 'wall-side' }, big, today, sun, recs)),
  );

  const tile = (cls: string, label: string, value: string, sub: string) => h('div', { class: `wall-tile ${cls}` }, h('div', { class: 'wl' }, label), h('div', { class: 'wv num' }, value), h('div', { class: 'ws' }, sub));
  const onLive = () => {
    const l = store.live;
    flow.update(l);
    const s = l?.status;
    root.classList.toggle('stale', !!l?.warn || !s);
    replace(
      status,
      l?.demo ? h('span', { class: 'banner demo' }, 'DEMO — synthetic data') : null,
      l?.warn && s ? h('span', { class: 'banner warn' }, `Stale — last reading ${ago(l.received_at, store.now())}`) : null,
      !store.connected ? h('span', { class: 'banner err' }, 'Reconnecting…') : null,
    );
    if (!s) {
      replace(big, h('div', { class: 'muted' }, 'Waiting for data…'));
      return;
    }
    replace(
      big,
      tile('t-solar', 'Solar', `${(Math.max(0, s.pv_w) / 1000).toFixed(1)}`, 'kW'),
      tile('t-home', 'Home', `${(s.load_w / 1000).toFixed(1)}`, 'kW'),
      tile('t-battery', 'Battery', pctv(s.soc_pct), s.storage_w < -20 ? `charging ${(-s.storage_w / 1000).toFixed(1)} kW` : s.storage_w > 20 ? `discharging ${(s.storage_w / 1000).toFixed(1)} kW` : 'idle'),
      tile('t-grid', 'Grid', `${(Math.abs(s.grid_w) / 1000).toFixed(1)}`, s.grid_w > 20 ? 'kW importing' : s.grid_w < -20 ? 'kW exporting' : 'kW idle'),
    );
  };
  const onIntel = () => {
    const i = store.intel;
    replace(sun, sunStrip(i?.sun_today ?? null, store.now()));
    const r = i?.recommendations?.[0];
    replace(recs, r ? h('div', { class: 'wall-rec' }, h('strong', null, r.title), h('div', null, r.message)) : null, i?.solar?.tomorrow_wh != null ? h('div', { class: 'small muted' }, `Tomorrow (estimate): ${kwh(i.solar.tomorrow_wh)} kWh solar`) : null);
  };
  const loadToday = async () => {
    try {
      const o = await get<Overview>('/overview');
      const p = o.periods[0];
      if (p) replace(today, h('span', null, `Today ${kwh(p.energy.pv_wh)} kWh solar · ${kwh(p.energy.load_wh)} kWh used · ${pct(p.grid_independence)} grid-independent`));
    } catch {
      /* ignore */
    }
  };
  const tick = () => replace(clock, new Date().toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' }));
  let idle: number | undefined;
  const wake = () => {
    document.body.classList.remove('cursor-hidden');
    window.clearTimeout(idle);
    idle = window.setTimeout(() => document.body.classList.add('cursor-hidden'), 3000);
  };
  window.addEventListener('mousemove', wake);
  wake();
  const offs = [store.onLive(onLive), store.onIntel(onIntel), store.onConn(onLive)];
  onLive();
  onIntel();
  tick();
  void loadToday();
  const ivs = [window.setInterval(tick, 10000), window.setInterval(() => void loadToday(), 60000)];
  return () => {
    offs.forEach((f) => f());
    ivs.forEach((i) => window.clearInterval(i));
    window.removeEventListener('mousemove', wake);
    window.clearTimeout(idle);
    document.body.classList.remove('cursor-hidden');
  };
}
