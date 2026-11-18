// RustSolar dashboard entry: shell, routing, login, status banners, theme.

import { get, post, setUnauthorizedHandler, errText } from './api';
import { h, replace, toast } from './dom';
import { ago, setUnits, time, units, type Units } from './fmt';
import { store } from './store';
import { helpButton, initHelp, openHelp, refreshHelp } from './help';
import { initTips } from './tips';
import type { SessionResp } from './types';
import { overviewPage } from './pages/overview';
import { solarPage } from './pages/solar';
import { batteryPage } from './pages/battery';
import { gridPage } from './pages/grid';
import { forecastPage } from './pages/forecast';
import { weatherPage } from './pages/weather';
import { financialPage } from './pages/financial';
import { recommendationsPage } from './pages/recommendations';
import { historyPage } from './pages/history';
import { settingsPage } from './pages/settings';
import { wallPage } from './pages/wall';

declare const __APP_VERSION__: string;

export type PageFn = (root: HTMLElement) => (() => void) | void;

interface NavItem {
  id: string;
  label: string;
  tip: string;
  page: PageFn;
}

const NAV: NavItem[] = [
  { id: 'overview', label: 'Overview', tip: 'Live power flow, today at a glance, recommendations', page: overviewPage },
  { id: 'solar', label: 'Solar', tip: 'Production today versus forecast, sun path, totals', page: solarPage },
  { id: 'battery', label: 'Batteries', tip: 'State of charge, outlook, outage planning, health', page: batteryPage },
  { id: 'grid', label: 'Grid & Household', tip: 'Imports, exports and household consumption', page: gridPage },
  { id: 'forecast', label: 'Forecast', tip: 'Solar and load forecasts with confidence and accuracy', page: forecastPage },
  { id: 'weather', label: 'Weather', tip: 'Hourly weather, irradiance, air quality, sunrise and sunset', page: weatherPage },
  { id: 'financial', label: 'Financial', tip: 'Estimated costs, export credits and savings from your rates', page: financialPage },
  { id: 'recommendations', label: 'Recommendations', tip: 'Suggestions and alerts, with the reasons behind them', page: recommendationsPage },
  { id: 'history', label: 'History', tip: 'Charts and exports for any date range', page: historyPage },
  { id: 'settings', label: 'Settings', tip: 'Setup, configuration and diagnostics', page: settingsPage },
];

const THEMES: [string, string][] = [
  ['system', 'System'],
  ['standard', 'Light'],
  ['dark', 'Dark'],
  ['sunlight', 'Sunlight'],
];

function storedTheme(): string {
  try {
    return localStorage.getItem('rs-theme') || 'system';
  } catch {
    return 'system';
  }
}

function applyTheme(t: string) {
  if (t === 'system') document.documentElement.removeAttribute('data-theme');
  else document.documentElement.setAttribute('data-theme', t);
  try {
    localStorage.setItem('rs-theme', t);
  } catch {
    /* private mode */
  }
  window.dispatchEvent(new Event('rs-theme'));
}

// Re-theme charts when the OS theme flips while on "System".
window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => window.dispatchEvent(new Event('rs-theme')));

const app = document.getElementById('app')!;
let cleanup: (() => void) | void;
let mainEl: HTMLElement;
let bannerEl: HTMLElement;
let titleEl: HTMLElement;
let statusEl: HTMLElement;
let navButtons = new Map<string, HTMLElement>();

function currentRoute(): string {
  const r = location.hash.replace(/^#\/?/, '').split('?')[0];
  return r || 'overview';
}

function buildShell() {
  navButtons = new Map();
  const side = h('nav', { class: 'side', id: 'side' });
  const brand = h('div', { class: 'brand' }, h('img', { src: '/logo.png', alt: 'Rocket Salvo' }), h('span', null, 'Rust', h('em', null, 'Solar')));
  side.appendChild(brand);
  for (const n of NAV) {
    const b = h('button', { class: 'nav', title: n.tip, onclick: () => go(n.id) }, n.label);
    navButtons.set(n.id, b);
    side.appendChild(b);
  }
  side.appendChild(h('button', { class: 'nav wall-btn', title: 'Full-screen wall display for a mounted tablet', onclick: () => go('wall') }, '⛶ Wall display'));
  const themeSel = h('select', { title: 'Colour theme (Sunlight = high contrast for bright rooms)', onchange: (e: Event) => applyTheme((e.target as HTMLSelectElement).value) }, ...THEMES.map(([v, l]) => h('option', { value: v }, l)));
  themeSel.value = storedTheme();
  const unitSel = h(
    'select',
    { title: 'Units for temperature, wind and rain (°F, mph, in or °C, m/s, mm)', onchange: (e: Event) => { setUnits((e.target as HTMLSelectElement).value as Units); route(); } },
    h('option', { value: 'imperial' }, 'Imperial'),
    h('option', { value: 'metric' }, 'Metric'),
  );
  unitSel.value = units();
  side.appendChild(
    h(
      'div',
      { class: 'prefs' },
      h('label', { title: 'Units for temperature, wind and rain' }, h('span', null, 'Units'), unitSel),
      h('label', { title: 'Colour theme' }, h('span', null, 'Theme'), themeSel),
    ),
  );
  side.appendChild(h('div', { class: 'side-foot' }, h('div', { class: 'wordmark' }, 'ROCKET', h('em', null, 'SALVO')), h('div', null, `RustSolar ${store.version || __APP_VERSION__}`), h('div', null, 'Free for noncommercial use')));

  titleEl = h('div', { class: 'top-title' });
  statusEl = h('div', { class: 'top-status', title: 'Live connection to the RustSolar server and age of the newest gateway reading' });
  const burger = h('button', { class: 'burger ghost', title: 'Menu', 'aria-label': 'Menu', onclick: () => side.classList.toggle('open') }, '☰');
  const top = h('header', { class: 'topbar' }, burger, titleEl, h('div', { class: 'spacer' }), statusEl, helpButton());
  bannerEl = h('div', { class: 'banners' });
  mainEl = h('div', { class: 'view' });
  const main = h('main', null, top, bannerEl, mainEl);
  replace(app, h('div', { class: 'shell' }, side, main));
  side.addEventListener('click', (e) => {
    if ((e.target as HTMLElement).classList.contains('nav')) side.classList.remove('open');
  });
}

function go(id: string) {
  if (currentRoute() === id) route();
  else location.hash = `#/${id}`;
}

function route() {
  if (typeof cleanup === 'function') cleanup();
  cleanup = undefined;
  const id = currentRoute();
  if (id === 'wall') {
    document.body.classList.add('wall-mode');
    const root = h('div', { class: 'wall-root' });
    replace(app, root);
    cleanup = wallPage(root);
    return;
  }
  if (document.body.classList.contains('wall-mode') || !document.getElementById('side')) {
    document.body.classList.remove('wall-mode');
    buildShell();
    renderStatus();
  }
  const item = NAV.find((n) => n.id === id) ?? NAV[0];
  navButtons.forEach((b, k) => b.classList.toggle('active', k === item.id));
  replace(titleEl, h('h1', null, item.label), h('span', { class: 'subtitle' }, 'Enphase Energy Intelligence'));
  document.title = `${item.label} · RustSolar`;
  mainEl.className = `view page-${item.id}`;
  replace(mainEl);
  mainEl.scrollTop = 0;
  cleanup = item.page(mainEl);
  // A link like #/battery?help opens the help panel for that page.
  if (/[?&]help\b/.test(location.hash)) openHelp();
  else refreshHelp();
}

const COLLECTOR_MSG: Record<string, [string, string]> = {
  no_host: ['warn', 'The gateway address is not set.'],
  no_token: ['warn', 'No gateway token yet — add it in Settings to start collecting data.'],
  unauthorized: ['err', 'The gateway rejected the token (it may have expired). Enter a new token in Settings.'],
  tls: ['err', 'Gateway certificate problem — check the pinned certificate in Settings.'],
  retrying: ['warn', 'Cannot reach the gateway; retrying with back-off.'],
  disabled: ['warn', 'Data collection is disabled in Settings.'],
};

function renderStatus() {
  if (!statusEl || !bannerEl) return;
  const live = store.live;
  const dot = h('span', { class: `dot ${store.connected ? (live?.warn ? 'warn' : 'ok') : 'off'}` });
  const last = live?.status ? `Reading ${time(live.status.last_update)}` : 'No reading yet';
  replace(statusEl, dot, h('span', null, store.connected ? last : 'Reconnecting…'));
  const banners: HTMLElement[] = [];
  if (live?.demo || store.demo) banners.push(h('div', { class: 'banner demo', title: 'Demo mode uses synthetic data in a separate database' }, 'DEMO MODE — synthetic data, not your system'));
  if (live && live.warn && live.status) {
    banners.push(h('div', { class: 'banner warn' }, `Data stale — last new reading ${ago(live.received_at, store.now())}. Values shown are the last known.`));
  }
  if (live && !live.demo) {
    const m = COLLECTOR_MSG[live.collector];
    // Brief connection hiccups stay quiet until the warning delay has passed.
    if (m && (live.collector !== 'retrying' || live.warn)) {
      banners.push(
        h(
          'div',
          { class: `banner ${m[0]}` },
          m[1],
          live.last_error && live.collector === 'retrying' ? h('span', { class: 'small' }, ` (${live.last_error})`) : null,
          ' ',
          h('a', { href: '#/settings', title: 'Open Settings' }, 'Settings →'),
        ),
      );
    }
  }
  for (const w of store.warnings) {
    banners.push(h('div', { class: 'banner warn' }, w, ' ', h('a', { href: '#/settings', title: 'Open Settings → Location' }, 'Settings →')));
  }
  if (!store.connected) banners.push(h('div', { class: 'banner err' }, 'Lost connection to the RustSolar server — reconnecting.'));
  replace(bannerEl, ...banners);
}

function loginScreen(msg?: string) {
  if (typeof cleanup === 'function') cleanup();
  const pw = h('input', { type: 'password', autocomplete: 'current-password', title: 'Dashboard password', placeholder: 'Password' });
  const err = h('div', { class: 'note err', hidden: !msg }, msg ?? '');
  const submit = async (e: Event) => {
    e.preventDefault();
    try {
      await post('/login', { password: pw.value });
      start();
    } catch (x) {
      err.textContent = errText(x);
      err.hidden = false;
    }
  };
  replace(
    app,
    h(
      'div',
      { class: 'login' },
      h(
        'form',
        { class: 'card login-card', onsubmit: submit },
        h('div', { class: 'brand dark' }, h('img', { src: '/logo.png', alt: 'Rocket Salvo' }), h('span', null, 'Rust', h('em', null, 'Solar'))),
        h('p', { class: 'muted' }, 'Enphase Energy Intelligence'),
        pw,
        err,
        h('button', { class: 'primary', type: 'submit', title: 'Sign in' }, 'Sign in'),
      ),
    ),
  );
  pw.focus();
}

let started = false;
async function start() {
  applyTheme(storedTheme());
  let sess: SessionResp;
  try {
    sess = await get<SessionResp>('/session');
  } catch (e) {
    replace(app, h('div', { class: 'login' }, h('div', { class: 'card' }, 'Cannot reach the RustSolar server: ', errText(e))));
    window.setTimeout(start, 5000);
    return;
  }
  store.demo = sess.demo;
  store.version = sess.version;
  store.warnings = sess.warnings ?? [];
  if (sess.password_required && !sess.authenticated) {
    loginScreen();
    return;
  }
  if (!started) {
    started = true;
    store.onLive(renderStatus);
    store.onConn(renderStatus);
    window.addEventListener('hashchange', route);
    initHelp(currentRoute);
    initTips();
    store.connect();
    window.setInterval(() => void store.refreshIntel(), 120000);
  }
  void store.refreshIntel();
  buildShell();
  renderStatus();
  route();
}

setUnauthorizedHandler(() => {
  toast('Please sign in again', 'error');
  loginScreen('Your session has ended.');
});

void start();

