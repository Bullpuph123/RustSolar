// Tiny DOM helpers. No framework: pages build elements and re-render sections.

type Attrs = Record<string, string | number | boolean | null | undefined | EventListener>;
export type Child = Node | string | number | null | undefined | false | Child[];

export function h<K extends keyof HTMLElementTagNameMap>(tag: K, attrs?: Attrs | null, ...children: Child[]): HTMLElementTagNameMap[K] {
  const el = document.createElement(tag);
  if (attrs) setAttrs(el, attrs);
  append(el, children);
  return el;
}

function setAttrs(el: Element, attrs: Attrs) {
  for (const [k, v] of Object.entries(attrs)) {
    if (v === null || v === undefined || v === false) continue;
    if (k.startsWith('on') && typeof v === 'function') {
      el.addEventListener(k.slice(2), v as EventListener);
    } else if (k === 'value' && (el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement || el instanceof HTMLSelectElement)) {
      el.value = String(v);
    } else if (k === 'checked' && el instanceof HTMLInputElement) {
      el.checked = Boolean(v);
    } else if (v === true) {
      el.setAttribute(k, '');
    } else {
      el.setAttribute(k, String(v));
    }
  }
}

export function append(el: Node, children: Child[]) {
  for (const c of children) {
    if (c === null || c === undefined || c === false) continue;
    if (Array.isArray(c)) append(el, c);
    else el.appendChild(typeof c === 'string' || typeof c === 'number' ? document.createTextNode(String(c)) : c);
  }
}

const SVGNS = 'http://www.w3.org/2000/svg';
export function s(tag: string, attrs?: Record<string, string | number | null | undefined>, ...children: (Node | string | null | undefined)[]): SVGElement {
  const el = document.createElementNS(SVGNS, tag);
  if (attrs) for (const [k, v] of Object.entries(attrs)) if (v !== null && v !== undefined) el.setAttribute(k, String(v));
  for (const c of children) if (c !== null && c !== undefined) el.appendChild(typeof c === 'string' ? document.createTextNode(c) : c);
  return el;
}

export function clear(el: Element) {
  while (el.firstChild) el.removeChild(el.firstChild);
}

export function replace(el: Element, ...children: Child[]) {
  clear(el);
  append(el, children);
}

/** A card with an h2 heading. */
export function card(title: string | null, ...children: Child[]): HTMLElement {
  return h('section', { class: 'card' }, title ? h('h2', null, title) : null, ...children);
}

/** Label/value statistic. */
export function stat(label: string, value: Child, tip?: string, sub?: Child): HTMLElement {
  return h('div', { class: 'stat', title: tip ?? label }, h('div', { class: 'stat-label' }, label), h('div', { class: 'stat-value num' }, value), sub ? h('div', { class: 'stat-sub' }, sub) : null);
}

export function note(kind: 'warn' | 'err' | 'ok' | 'info', ...children: Child[]): HTMLElement {
  return h('div', { class: `note ${kind}` }, ...children);
}

export function empty(text: string, ...extra: Child[]): HTMLElement {
  return h('div', { class: 'empty-state' }, h('div', null, text), ...extra);
}

export function estimateTag(): HTMLElement {
  return h('span', { class: 'tag est', title: 'Forecasts and simulations are estimates, not measurements' }, 'Estimate');
}

let toastTimer: number | undefined;
export function toast(text: string, kind: 'ok' | 'error' = 'ok') {
  let el = document.getElementById('toast');
  if (!el) {
    el = h('div', { id: 'toast', role: 'status' });
    document.body.appendChild(el);
  }
  el.className = `toast ${kind === 'error' ? 'error' : ''}`;
  el.textContent = text;
  el.hidden = false;
  window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => {
    if (el) el.hidden = true;
  }, kind === 'error' ? 8000 : 3500);
}

/** Button that asks for an inline confirmation before running `run`. */
export function confirmButton(label: string, tip: string, question: string, run: () => void | Promise<void>, cls = ''): HTMLElement {
  const wrap = h('span', { class: 'confirm-wrap' });
  const show = () => {
    replace(
      wrap,
      h('span', { class: 'confirm-q' }, question),
      h('button', { class: 'danger', title: 'Yes, do it', onclick: async () => { await run(); idle(); } }, 'Yes'),
      h('button', { class: 'ghost', title: 'Cancel', onclick: () => idle() }, 'Cancel'),
    );
  };
  const idle = () => replace(wrap, h('button', { class: cls, title: tip, onclick: show }, label));
  idle();
  return wrap;
}
