// Animated energy-flow diagram. Edges come only from the API's attributed flows
// (live.flows); an edge is drawn only when its measured share exceeds MIN_W.

import { s } from './dom';
import type { LiveSnapshot } from './types';

const MIN_W = 20;

type NodeKey = 'solar' | 'grid' | 'home' | 'battery';
const POS: Record<NodeKey, [number, number]> = { solar: [200, 58], grid: [62, 170], home: [338, 170], battery: [200, 282] };
const R = 36;

interface EdgeDef {
  key: 'pv_to_load' | 'pv_to_batt' | 'pv_to_grid' | 'batt_to_load' | 'batt_to_grid' | 'grid_to_load' | 'grid_to_batt';
  d: string;
  color: string;
  label: [number, number];
  title: string;
}

const EDGES: EdgeDef[] = [
  { key: 'pv_to_load', d: 'M226 82 Q300 82 318 140', color: 'var(--flow-solar)', label: [290, 98], title: 'Solar → Home' },
  { key: 'pv_to_grid', d: 'M174 82 Q100 82 82 140', color: 'var(--flow-solar)', label: [110, 98], title: 'Solar → Grid (export)' },
  { key: 'pv_to_batt', d: 'M200 94 Q186 170 200 246', color: 'var(--flow-solar)', label: [168, 200], title: 'Solar → Battery' },
  { key: 'grid_to_load', d: 'M98 170 Q200 154 302 170', color: 'var(--flow-grid)', label: [250, 152], title: 'Grid → Home (import)' },
  { key: 'batt_to_load', d: 'M226 258 Q300 258 318 200', color: 'var(--flow-battery)', label: [290, 250], title: 'Battery → Home' },
  { key: 'batt_to_grid', d: 'M174 258 Q100 258 82 200', color: 'var(--flow-battery)', label: [110, 250], title: 'Battery → Grid (export)' },
  { key: 'grid_to_batt', d: 'M82 200 Q100 258 174 258', color: 'var(--flow-grid)', label: [110, 270], title: 'Grid → Battery' },
];

const ICONS: Record<NodeKey, string> = {
  solar: 'M0 -9 a9 9 0 1 0 0.01 0 M0 -17 v4 M0 13 v4 M-17 0 h4 M13 0 h4 M-12 -12 l3 3 M9 9 l3 3 M12 -12 l-3 3 M-9 9 l-3 3',
  home: 'M-13 1 L0 -12 L13 1 M-9 -2 V12 H9 V-2 M-3 12 V5 H3 V12',
  battery: 'M-9 -11 H9 V13 H-9 Z M-4 -14 H4 M-5 5 H5 M-5 0 H5',
  grid: 'M0 -15 L-9 14 M0 -15 L9 14 M-11 -6 H11 M-8 3 H8 M-5 -6 L6 3 M5 -6 L-6 3',
};
const NODE_COLOR: Record<NodeKey, string> = { solar: 'var(--flow-solar)', grid: 'var(--flow-grid)', home: 'var(--flow-home)', battery: 'var(--flow-battery)' };
const NODE_NAME: Record<NodeKey, string> = { solar: 'Solar', grid: 'Grid', home: 'Home', battery: 'Battery' };

export class FlowDiagram {
  readonly el: SVGElement;
  private edges = new Map<string, { path: SVGElement; label: SVGElement; group: SVGElement }>();
  private values = new Map<NodeKey, { value: SVGElement; sub: SVGElement }>();
  private socRing: SVGElement;
  private reserveTick: SVGElement;
  private socText: SVGElement;

  constructor(big = false) {
    this.el = s('svg', { viewBox: '0 0 400 330', class: `flow${big ? ' big' : ''}`, role: 'img', 'aria-label': 'Energy flow between solar, home, battery and grid' });
    const edgeLayer = s('g', { class: 'edges' });
    for (const e of EDGES) {
      const base = s('path', { d: e.d, class: 'edge-base' });
      const path = s('path', { d: e.d, class: 'edge-flow', stroke: e.color });
      const label = s('text', { x: e.label[0], y: e.label[1], class: 'edge-label', 'text-anchor': 'middle' });
      const group = s('g', { class: 'edge' }, s('title', {}, e.title), base, path, label);
      edgeLayer.appendChild(group);
      this.edges.set(e.key, { path, label, group });
    }
    this.el.appendChild(edgeLayer);
    const circ = 2 * Math.PI * (R + 6);
    this.socRing = s('circle', { cx: POS.battery[0], cy: POS.battery[1], r: R + 6, class: 'soc-ring', 'stroke-dasharray': `0 ${circ}`, transform: `rotate(-90 ${POS.battery[0]} ${POS.battery[1]})` });
    this.reserveTick = s('line', { class: 'reserve-tick', x1: 0, y1: 0, x2: 0, y2: 0 });
    this.socText = s('text', { x: POS.battery[0] - R - 16, y: POS.battery[1] + 16, class: 'soc-text', 'text-anchor': 'end' });
    this.el.appendChild(s('circle', { cx: POS.battery[0], cy: POS.battery[1], r: R + 6, class: 'soc-track' }));
    this.el.appendChild(this.socRing);
    this.el.appendChild(this.reserveTick);
    for (const k of Object.keys(POS) as NodeKey[]) {
      const [x, y] = POS[k];
      // Solar and battery carry their readout to the right so it never sits on an edge.
      const side = k === 'solar' || k === 'battery';
      const value = side ? s('text', { x: x + R + 16, y: y + 6, class: 'node-value', 'text-anchor': 'start' }) : s('text', { x, y: y + R + 18, class: 'node-value', 'text-anchor': 'middle' });
      const sub = side ? s('text', { x: x + R + 16, y: y + 22, class: 'node-sub', 'text-anchor': 'start' }) : s('text', { x, y: y + R + 32, class: 'node-sub', 'text-anchor': 'middle' });
      const yLabel = k === 'solar' ? y - R - 8 : y - R - 6;
      const g = s(
        'g',
        { class: `node node-${k}` },
        s('circle', { cx: x, cy: y, r: R, class: 'node-circle', stroke: NODE_COLOR[k] }),
        s('path', { d: ICONS[k], transform: `translate(${x} ${y})`, class: 'node-icon', stroke: NODE_COLOR[k] }),
        k === 'battery' || k === 'solar' ? null : s('text', { x, y: yLabel, class: 'node-name', 'text-anchor': 'middle' }, NODE_NAME[k]),
        value,
        sub,
      );
      if (k === 'solar') g.appendChild(s('text', { x: x + R + 16, y: y - 14, class: 'node-name', 'text-anchor': 'start' }, NODE_NAME[k]));
      if (k === 'battery') g.appendChild(s('text', { x: x - R - 16, y: y - 6, class: 'node-name', 'text-anchor': 'end' }, NODE_NAME[k]));
      this.el.appendChild(g);
      this.values.set(k, { value, sub });
    }
    this.el.appendChild(this.socText);
  }

  update(live: LiveSnapshot | null) {
    const f = live?.flows;
    const st = live?.status ?? null;
    this.el.classList.toggle('stale', !!live?.warn || !st);
    for (const e of EDGES) {
      const parts = this.edges.get(e.key)!;
      const w = f ? f[e.key] : 0;
      const on = st !== null && w > MIN_W;
      parts.group.classList.toggle('on', on);
      if (on) {
        const kwv = w / 1000;
        parts.path.setAttribute('stroke-width', String(2 + Math.min(6, kwv * 1.1)));
        (parts.path as SVGElement & { style: CSSStyleDeclaration }).style.animationDuration = `${Math.max(0.35, 2.4 / (0.6 + kwv)).toFixed(2)}s`;
        parts.label.textContent = `${kwv.toFixed(kwv < 10 ? 2 : 1)} kW`;
      } else {
        parts.label.textContent = '';
      }
    }
    const set = (k: NodeKey, v: string, sub: string) => {
      const n = this.values.get(k)!;
      n.value.textContent = v;
      n.sub.textContent = sub;
    };
    if (!st) {
      (['solar', 'home', 'grid', 'battery'] as NodeKey[]).forEach((k) => set(k, '—', ''));
      return;
    }
    set('solar', `${(Math.max(0, st.pv_w) / 1000).toFixed(2)} kW`, st.pv_w > 20 ? 'producing' : 'idle');
    set('home', `${(st.load_w / 1000).toFixed(2)} kW`, 'using');
    const g = st.grid_w;
    set('grid', `${(Math.abs(g) / 1000).toFixed(2)} kW`, g > 20 ? 'importing' : g < -20 ? 'exporting' : 'idle');
    const b = st.storage_w;
    set('battery', `${(Math.abs(b) / 1000).toFixed(2)} kW`, b < -20 ? 'charging' : b > 20 ? 'discharging' : 'idle');
    const soc = st.soc_pct;
    const circ = 2 * Math.PI * (R + 6);
    if (soc !== null) {
      this.socRing.setAttribute('stroke-dasharray', `${(circ * soc) / 100} ${circ}`);
      this.socText.textContent = `${soc.toFixed(0)}%`;
    } else {
      this.socText.textContent = '';
    }
    const res = st.backup_soc_pct;
    if (res !== null) {
      const a = (res / 100) * 2 * Math.PI - Math.PI / 2;
      const [cx, cy] = POS.battery;
      const r1 = R + 1;
      const r2 = R + 12;
      this.reserveTick.setAttribute('x1', String(cx + r1 * Math.cos(a)));
      this.reserveTick.setAttribute('y1', String(cy + r1 * Math.sin(a)));
      this.reserveTick.setAttribute('x2', String(cx + r2 * Math.cos(a)));
      this.reserveTick.setAttribute('y2', String(cy + r2 * Math.sin(a)));
      this.reserveTick.innerHTML = '';
      this.reserveTick.appendChild(s('title', {}, `Backup reserve ${res.toFixed(0)} %`));
    }
  }
}
