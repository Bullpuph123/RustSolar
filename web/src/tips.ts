// Styled tooltips for every element with a title. The native title is moved into
// data-tip on first hover (so the browser's plain tooltip doesn't also appear) and kept as
// aria-description for screen readers. Touch: tapping a non-interactive element with a tip
// (a tile, a statistic, an hour in the rate strip) shows it for a few seconds.

let tipEl: HTMLElement | null = null;
let showTimer: number | undefined;
let hideTimer: number | undefined;
let owner: Element | null = null;

function tipFor(target: EventTarget | null): Element | null {
  return target instanceof Element ? target.closest('[title],[data-tip]') : null;
}

function textOf(el: Element): string {
  const t = el.getAttribute('title');
  if (t !== null) {
    el.removeAttribute('title');
    if (t) {
      el.setAttribute('data-tip', t);
      if (!el.hasAttribute('aria-description')) el.setAttribute('aria-description', t);
    }
    return t;
  }
  return el.getAttribute('data-tip') ?? '';
}

function show(el: Element) {
  const text = textOf(el);
  if (!text) return;
  if (!tipEl) {
    tipEl = document.createElement('div');
    tipEl.className = 'rs-tip';
    tipEl.setAttribute('role', 'tooltip');
    document.body.appendChild(tipEl);
  }
  tipEl.textContent = text;
  tipEl.hidden = false;
  owner = el;
  const r = el.getBoundingClientRect();
  const t = tipEl.getBoundingClientRect();
  const gap = 8;
  let top = r.top - t.height - gap;
  if (top < 4) top = r.bottom + gap;
  let left = r.left + r.width / 2 - t.width / 2;
  left = Math.max(6, Math.min(left, window.innerWidth - t.width - 6));
  tipEl.style.top = `${Math.max(4, top)}px`;
  tipEl.style.left = `${left}px`;
}

function hide() {
  window.clearTimeout(showTimer);
  window.clearTimeout(hideTimer);
  owner = null;
  if (tipEl) tipEl.hidden = true;
}

const INTERACTIVE = 'button, a, input, select, textarea, label, summary';

export function initTips() {
  if (window.matchMedia('(hover: hover)').matches) {
    document.addEventListener('mouseover', (e) => {
      const el = tipFor(e.target);
      if (el === owner) return;
      hide();
      if (!el) return;
      // Pull the title off immediately so the native tooltip never appears.
      textOf(el);
      showTimer = window.setTimeout(() => show(el), 350);
    });
    document.addEventListener('mouseout', (e) => {
      const to = (e as MouseEvent).relatedTarget;
      if (owner && to instanceof Node && owner.contains(to)) return;
      if (tipFor(to) !== owner) hide();
    });
  }
  document.addEventListener('focusin', (e) => {
    const el = e.target instanceof Element ? e.target : null;
    if (el && el.matches(':focus-visible') && (el.hasAttribute('title') || el.hasAttribute('data-tip'))) show(el);
  });
  document.addEventListener('focusout', hide);
  document.addEventListener('pointerdown', (e) => {
    hide();
    if (e.pointerType !== 'touch') return;
    const el = tipFor(e.target);
    if (!el || (e.target as Element).closest(INTERACTIVE)) return;
    show(el);
    hideTimer = window.setTimeout(hide, 4000);
  });
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape') hide();
  });
  window.addEventListener('scroll', hide, true);
  window.addEventListener('hashchange', hide);
}
