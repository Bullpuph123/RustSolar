import { get } from '../api';
import { alertItem, every, recCard } from '../common';
import { card, empty, h, replace } from '../dom';
import { dateTime } from '../fmt';
import { store } from '../store';
import type { AlertRow } from '../types';

export function recommendationsPage(root: HTMLElement) {
  const recsEl = h('div', { class: 'recs' });
  const activeEl = h('div');
  const histEl = h('div');
  root.append(
    card('Recommendations', h('p', { class: 'small muted' }, 'Each suggestion shows why it was made, the supporting measurements and how confident it is. Suggestions never ask you to dip into your backup reserve.'), recsEl),
    h('div', { class: 'grid-2' }, card('Active alerts', activeEl), card('Alert history', histEl)),
  );
  const renderRecs = () => {
    const i = store.intel;
    const recs = i?.recommendations ?? [];
    replace(
      recsEl,
      recs.length ? recs.map(recCard) : empty('Nothing to suggest right now.', h('div', { class: 'small' }, 'Suggestions appear when forecasts, the battery outlook or live readings point to something useful.')),
      i ? h('div', { class: 'small muted' }, `Updated ${dateTime(i.computed_at)}`) : null,
    );
  };
  const loadAlerts = async () => {
    const [a, all] = await Promise.all([get<{ alerts: AlertRow[] }>('/alerts'), get<{ alerts: AlertRow[] }>('/alerts?all=true')]);
    replace(activeEl, a.alerts.length ? a.alerts.map((x) => alertItem(x, loadAlerts)) : h('div', { class: 'muted small' }, 'No active alerts.'));
    const past = all.alerts.filter((x) => x.cleared_at !== null).slice(0, 50);
    replace(histEl, past.length ? past.map((x) => alertItem(x, loadAlerts)) : h('div', { class: 'muted small' }, 'No past alerts.'));
  };
  const off = store.onIntel(() => {
    renderRecs();
    void loadAlerts().catch(() => {});
  });
  renderRecs();
  const stop = every(60000, () => loadAlerts().catch(() => {}));
  return () => {
    off();
    stop();
  };
}
