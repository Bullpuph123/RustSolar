// uPlot wrapper: colours from CSS variables, responsive width, rebuilt on theme change.

import uPlot from 'uplot';
import { h } from './dom';

export function cssVar(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim() || '#888';
}

export function alpha(color: string, a: number): string {
  const c = color.trim();
  if (c.startsWith('#') && (c.length === 7 || c.length === 4)) {
    const full = c.length === 4 ? '#' + [...c.slice(1)].map((x) => x + x).join('') : c;
    const n = parseInt(full.slice(1), 16);
    return `rgba(${(n >> 16) & 255},${(n >> 8) & 255},${n & 255},${a})`;
  }
  return c;
}

export const COLORS = {
  solar: '--rs-state-armed',
  home: '--rs-series-2',
  battery: '--rs-series-3',
  grid: '--rs-series-1',
  violet: '--rs-series-4',
  muted: '--rs-color-text-muted',
  danger: '--rs-state-recording',
};

export interface SeriesDef {
  label: string;
  color: string; // CSS variable name or literal colour
  width?: number;
  dash?: number[];
  fill?: number; // fill alpha
  bars?: boolean;
  barAlign?: -1 | 0 | 1;
  barSize?: number;
  scale?: string;
  value?: (v: number | null) => string;
  points?: boolean;
  show?: boolean;
  span?: boolean; // connect across nulls (for series sampled on a different grid)
}

export interface ChartDef {
  series: SeriesDef[];
  data: uPlot.AlignedData;
  height?: number;
  yLabel?: string;
  y2Label?: string;
  /** Pairs of series indexes (1-based, data index) [upper, lower] to shade between. */
  bands?: { series: [number, number]; color: string }[];
  time?: boolean;
  xValue?: (v: number) => string;
  yRange?: [number | null, number | null];
  xRange?: [number, number];
  zero?: boolean; // y axis always includes 0 (bar charts)
  y2Range?: [number, number];
  marker?: number; // vertical line at this x (e.g. now)
  xFormat?: 'date' | 'hour';
}

const resolve = (c: string) => (c.startsWith('--') ? cssVar(c) : c);

export class Chart {
  readonly el: HTMLElement;
  private plot: uPlot | null = null;
  private ro: ResizeObserver;
  private onTheme = () => this.build();

  constructor(private def: ChartDef) {
    this.el = h('div', { class: 'chart' });
    this.ro = new ResizeObserver(() => {
      if (this.plot) this.plot.setSize({ width: this.width(), height: this.def.height ?? 260 });
      else this.build();
    });
    this.ro.observe(this.el);
    window.addEventListener('rs-theme', this.onTheme);
  }

  private width() {
    return Math.max(260, this.el.clientWidth);
  }

  update(def: Partial<ChartDef>) {
    this.def = { ...this.def, ...def };
    this.build();
  }

  setData(data: uPlot.AlignedData) {
    this.def.data = data;
    if (this.plot) this.plot.setData(data);
    else this.build();
  }

  private build() {
    if (!this.el.isConnected || this.el.clientWidth === 0) return;
    this.plot?.destroy();
    this.plot = null;
    const d = this.def;
    const text = cssVar('--rs-color-text-muted');
    const grid = cssVar('--rs-color-chart-grid');
    const axisFont = `11px ${cssVar('--rs-font-ui') || 'sans-serif'}`;
    const barsPath = uPlot.paths.bars ? uPlot.paths.bars({ size: [0.7, 40], align: 0 }) : undefined;
    const series: uPlot.Series[] = [
      d.time === false ? { label: 'x', value: d.xValue ? (_u, v) => d.xValue!(v) : undefined } : { label: 'Time', value: d.xFormat === 'date' ? '{MMM} {D}' : '{MMM} {D} {h}:{mm}{aa}' },
    ];
    for (const sd of d.series) {
      const color = resolve(sd.color);
      series.push({
        label: sd.label,
        stroke: color,
        width: sd.width ?? 1.6,
        dash: sd.dash,
        fill: sd.fill ? alpha(color, sd.fill) : sd.bars ? alpha(color, 0.75) : undefined,
        paths: sd.bars ? (sd.barAlign !== undefined || sd.barSize !== undefined ? uPlot.paths.bars!({ size: [sd.barSize ?? 0.7, 60], align: sd.barAlign ?? 0 }) : barsPath) : undefined,
        scale: sd.scale ?? 'y',
        points: { show: sd.points ?? false, size: 4 },
        value: sd.value ? (_u, v) => sd.value!(v) : undefined,
        show: sd.show ?? true,
        spanGaps: sd.span ?? false,
      });
    }
    const axes: uPlot.Axis[] = [
      { stroke: text, grid: { stroke: grid, width: 1 }, ticks: { stroke: grid }, font: axisFont, values: d.time === false && d.xValue ? (_u, vals) => vals.map((v) => d.xValue!(v)) : undefined },
      { stroke: text, grid: { stroke: grid, width: 1 }, ticks: { stroke: grid }, font: axisFont, label: d.yLabel, labelFont: axisFont, size: 52 },
    ];
    const xr = d.xRange;
    const scales: uPlot.Scales = { x: xr ? { time: d.time !== false, range: () => [xr[0], xr[1]] } : { time: d.time !== false }, y: d.yRange
        ? { range: (_u, min, max) => [d.yRange![0] ?? min, d.yRange![1] ?? max] }
        : d.zero
          ? { range: (_u, min, max) => { const lo = Math.min(0, min ?? 0); const hi = Math.max(0, max ?? 0); const pad = (hi - lo || 1) * 0.08; return [lo < 0 ? lo - pad : 0, hi + pad]; } }
          : {},
    };
    if (d.series.some((x) => x.scale === 'y2')) {
      axes.push({ side: 1, scale: 'y2', stroke: text, grid: { show: false }, ticks: { stroke: grid }, font: axisFont, label: d.y2Label, labelFont: axisFont, size: 48 });
      scales.y2 = d.y2Range ? { range: () => d.y2Range! } : {};
    }
    const marker = d.marker;
    const opts: uPlot.Options = {
      width: this.width(),
      height: d.height ?? 260,
      series,
      axes,
      scales,
      bands: d.bands
        ?.filter((b) => b.series.every((i) => (d.data[i] as (number | null | undefined)[] | undefined)?.some((v) => v !== null && v !== undefined)))
        .map((b) => ({ series: b.series, fill: alpha(resolve(b.color), 0.18) })),
      cursor: { drag: { x: true, y: false } },
      legend: { live: true },
      hooks: marker
        ? {
            draw: [
              (u: uPlot) => {
                const x = u.valToPos(marker, 'x', true);
                if (!isFinite(x)) return;
                const ctx = u.ctx;
                ctx.save();
                ctx.strokeStyle = cssVar('--rs-brand-ignition');
                ctx.setLineDash([4, 4]);
                ctx.lineWidth = 1.5 * devicePixelRatio;
                ctx.beginPath();
                ctx.moveTo(x, u.bbox.top);
                ctx.lineTo(x, u.bbox.top + u.bbox.height);
                ctx.stroke();
                ctx.restore();
              },
            ],
          }
        : {},
    };
    this.plot = new uPlot(opts, d.data, this.el);
  }

  destroy() {
    this.ro.disconnect();
    window.removeEventListener('rs-theme', this.onTheme);
    this.plot?.destroy();
    this.plot = null;
  }
}

/** Track charts created by a page so they're destroyed on navigation. */
export class ChartSet {
  private list: Chart[] = [];
  add(def: ChartDef): Chart {
    const c = new Chart(def);
    this.list.push(c);
    return c;
  }
  destroy() {
    this.list.forEach((c) => c.destroy());
    this.list = [];
  }
}

export const kwVal = (v: number | null) => (v === null || v === undefined ? "—" : `${v.toFixed(2)} kW`);
export const kwhVal = (v: number | null) => (v === null || v === undefined ? "—" : `${v.toFixed(2)} kWh`);
export const pctVal = (v: number | null) => (v === null || v === undefined ? '—' : `${v.toFixed(0)} %`);
