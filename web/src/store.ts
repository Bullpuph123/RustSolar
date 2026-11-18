// Shared client state: live snapshot (SSE), intelligence results, connection status.

import { get } from './api';
import type { Intel, LiveMsg, LiveSnapshot } from './types';

type Listener = () => void;

class Store {
  live: LiveSnapshot | null = null;
  serverNow = 0;
  /** Server clock minus browser clock, seconds. */
  clockOffset = 0;
  intel: Intel | null = null;
  connected = false;
  demo = false;
  version = '';
  /** Config plausibility warnings from the server (e.g. a longitude with the wrong sign). */
  warnings: string[] = [];
  private liveL = new Set<Listener>();
  private intelL = new Set<Listener>();
  private connL = new Set<Listener>();
  private es: EventSource | null = null;
  private retry = 1000;

  onLive(f: Listener): () => void {
    this.liveL.add(f);
    return () => this.liveL.delete(f);
  }
  setWarnings(w: string[]) {
    this.warnings = w;
    this.connL.forEach((f) => f());
  }
  onIntel(f: Listener): () => void {
    this.intelL.add(f);
    return () => this.intelL.delete(f);
  }
  onConn(f: Listener): () => void {
    this.connL.add(f);
    return () => this.connL.delete(f);
  }

  setLive(m: LiveMsg) {
    this.live = m.live;
    this.serverNow = m.now;
    this.clockOffset = m.now - Date.now() / 1000;
    this.demo = m.live.demo;
    this.liveL.forEach((f) => f());
  }

  async refreshIntel() {
    try {
      const i = await get<Intel>('/intel');
      if (!i.pending) {
        this.intel = i;
        this.intelL.forEach((f) => f());
      }
    } catch {
      /* shown via connection state */
    }
  }

  connect() {
    this.es?.close();
    const es = new EventSource('/api/stream');
    this.es = es;
    es.addEventListener('live', (ev) => {
      this.retry = 1000;
      if (!this.connected) {
        this.connected = true;
        this.connL.forEach((f) => f());
      }
      try {
        this.setLive(JSON.parse((ev as MessageEvent).data) as LiveMsg);
      } catch {
        /* ignore malformed */
      }
    });
    es.addEventListener('intel', () => void this.refreshIntel());
    es.onerror = () => {
      es.close();
      if (this.connected) {
        this.connected = false;
        this.connL.forEach((f) => f());
      }
      window.setTimeout(() => this.connect(), this.retry);
      this.retry = Math.min(this.retry * 2, 30000);
    };
  }

  /** Server "now" estimate in Unix seconds. */
  now(): number {
    return Date.now() / 1000 + this.clockOffset;
  }
}

export const store = new Store();
