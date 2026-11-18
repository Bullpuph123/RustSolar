import { get, post, errText } from '../api';
import { card, confirmButton, h, note, replace, toast, type Child } from '../dom';
import { ago, dateTime, duration, num } from '../fmt';
import { store } from '../store';
import type { ArraySpec, Config, ConfigResp } from '../types';

declare const __APP_VERSION__: string;

// ---------- field helpers ----------

interface NumOpts {
  min?: number;
  max?: number;
  step?: number;
  optional?: boolean;
  unit?: string;
  scale?: number; // displayed = stored * scale
}

function numField(label: string, tip: string, getv: () => number | null, setv: (v: number | null) => void, o: NumOpts = {}): HTMLElement {
  const scale = o.scale ?? 1;
  const cur = getv();
  const input = h('input', { type: 'number', step: o.step ?? 'any', value: cur === null || cur === undefined ? '' : String(+(cur * scale).toFixed(6)), title: `${tip}${o.min !== undefined ? ` (allowed ${o.min}–${o.max})` : ''}` });
  const msg = h('div', { class: 'field-msg' });
  input.addEventListener('input', () => {
    const raw = input.value.trim();
    if (raw === '') {
      if (o.optional) {
        setv(null);
        input.classList.remove('invalid');
        msg.textContent = '';
      } else {
        input.classList.add('invalid');
        msg.textContent = 'Required';
      }
      return;
    }
    const v = Number(raw);
    if (!isFinite(v) || (o.min !== undefined && v < o.min) || (o.max !== undefined && v > o.max)) {
      input.classList.add('invalid');
      msg.textContent = `Must be ${o.min}–${o.max}`;
      return;
    }
    input.classList.remove('invalid');
    msg.textContent = '';
    setv(v / scale);
  });
  return h('label', { class: 'field', title: tip }, h('span', null, label, o.unit ? h('span', { class: 'muted' }, ` (${o.unit})`) : null), input, msg);
}

function textField(label: string, tip: string, getv: () => string | null, setv: (v: string | null) => void, opts: { placeholder?: string; optional?: boolean; list?: string } = {}): HTMLElement {
  const input = h('input', { type: 'text', value: getv() ?? '', title: tip, placeholder: opts.placeholder, list: opts.list });
  input.addEventListener('input', () => setv(input.value.trim() === '' && opts.optional ? null : input.value.trim()));
  return h('label', { class: 'field', title: tip }, h('span', null, label), input);
}

function checkField(label: string, tip: string, getv: () => boolean, setv: (v: boolean) => void): HTMLElement {
  const input = h('input', { type: 'checkbox', checked: getv(), title: tip });
  input.addEventListener('change', () => setv(input.checked));
  return h('label', { class: 'field check', title: tip }, input, h('span', null, label));
}

function selectField(label: string, tip: string, options: [string, string][], getv: () => string, setv: (v: string) => void): HTMLElement {
  const sel = h('select', { title: tip }, ...options.map(([v, l]) => h('option', { value: v }, l)));
  sel.value = getv();
  sel.addEventListener('change', () => setv(sel.value));
  return h('label', { class: 'field', title: tip }, h('span', null, label), sel);
}

interface ExportJson {
  type: string;
  vintage?: number;
  generation?: boolean;
  acc_plus?: boolean;
  pto_date?: string | null;
  [k: string]: unknown;
}
interface NbtResp {
  vintages: { vintage: number; acc_plus: number | null; years: number[] }[];
  table: { vintage: number; year: number; weekday: number[][]; weekend: number[][] } | null;
  source: string;
}
let nbtInfo: NbtResp | null = null;

const fields = (...c: Child[]) => h('div', { class: 'grid-fields' }, ...c);

const TZS = ['America/Los_Angeles', 'America/Denver', 'America/Phoenix', 'America/Chicago', 'America/New_York', 'America/Anchorage', 'Pacific/Honolulu', 'UTC'];

const EXAMPLE_TARIFF = {
  enabled: true,
  currency: 'USD',
  time_zone: 'America/Los_Angeles',
  holidays: ['2026-01-01', '2026-07-04', '2026-12-25'],
  plans: [
    {
      name: 'EXAMPLE ONLY — replace every price with your own bill rates',
      effective_from: '2024-01-01',
      fixed_daily: 0.5,
      export: { type: 'net_metering', non_bypassable_per_kwh: 0.02 },
      baseline: null,
      tiers: [],
      demand_per_kw: null,
      seasons: [
        {
          name: 'summer',
          months: [6, 7, 8, 9],
          periods: [
            { name: 'peak', days: 'all', start_hour: 16, end_hour: 21, import_per_kwh: 0.5 },
            { name: 'off-peak', days: 'all', start_hour: 21, end_hour: 16, import_per_kwh: 0.3 },
          ],
        },
        {
          name: 'winter',
          months: [1, 2, 3, 4, 5, 10, 11, 12],
          periods: [
            { name: 'peak', days: 'all', start_hour: 16, end_hour: 21, import_per_kwh: 0.4 },
            { name: 'off-peak', days: 'all', start_hour: 21, end_hour: 16, import_per_kwh: 0.2 },
          ],
        },
      ],
    },
  ],
};

// ---------- page ----------

interface Diag {
  version: string;
  demo: boolean;
  data_dir: string;
  db_bytes: number | null;
  collector: {
    polls: number;
    successes: number;
    failures: number;
    duplicates: number;
    gaps: number;
    unbalanced: number;
    failures_by_kind: Record<string, number>;
    last_latency_ms: number | null;
    avg_latency_ms: number | null;
    max_latency_ms: number | null;
    last_success_at: number | null;
    last_failure_at: number | null;
    last_error: string | null;
    last_update_interval_s: number | null;
    current_backoff_s: number;
    clock_skew_s: number | null;
    started_at: number;
  };
  token: { present: boolean; source?: string; expires_at?: number | null; days_left?: number | null; role?: string | null; error?: string };
  tls: { mode: string; pinned: string | null; seen: string | null };
  store: {
    endpoints: [string, string][];
    events: { ts: number; source: string; level: string; message: string }[];
    rows: { samples: number; hours: number; weather_hours: number };
    first_sample: number | null;
    residual_24h: { mean_abs_w: number | null; max_abs_w: number | null };
    enc_agg_energy_implied_capacity_wh: number | null;
    weather: { last_success_at: number | null; requests_today: number; daily_budget: number; last_error: string | null; attribution: string };
  };
  config_problems: string[];
}

export function settingsPage(root: HTMLElement) {
  let resp: ConfigResp | null = null;
  let cfg: Config;
  const body = h('div');
  root.append(body);
  let cancelled = false;

  const reload = async () => {
    try {
      resp = await get<ConfigResp>('/config');
      cfg = structuredClone(resp.config);
      store.setWarnings(resp.warnings ?? []);
      render();
    } catch (e) {
      replace(body, note('err', errText(e)));
    }
  };

  async function save(extra: { new_password?: string; clear_password?: boolean } = {}, quiet = false): Promise<boolean> {
    try {
      const r = await post<{ ok: boolean; restart_needed: boolean; saved: boolean }>('/config', { config: cfg, ...extra });
      if (!quiet) toast(r.saved ? 'Settings saved' : 'Applied (demo mode: not written to disk)');
      if (r.restart_needed) toast('Saved. Restart RustSolar for the new listen address to take effect.');
      await reload();
      return true;
    } catch (e) {
      errorsEl.hidden = false;
      replace(errorsEl, h('strong', null, 'Not saved: '), h('ul', null, ...errText(e).split('; ').map((x) => h('li', null, x))));
      errorsEl.scrollIntoView({ behavior: 'smooth', block: 'center' });
      return false;
    }
  }

  const errorsEl = h('div', { class: 'note err', hidden: true });

  function render() {
    if (!resp) return;
    const su = resp.setup;
    const pwNew = { v: '' };
    const saveBar = h(
      'div',
      { class: 'savebar' },
      h('button', { class: 'primary', title: 'Validate and save all settings below; collectors restart automatically', onclick: () => void save(pwNew.v ? { new_password: pwNew.v } : {}) }, 'Save settings'),
      h('span', { class: 'small muted' }, `Config file: ${resp.config_path}`),
    );
    replace(
      body,
      resp.problems.length ? note('warn', h('strong', null, 'Configuration needs attention:'), h('ul', null, ...resp.problems.map((p) => h('li', null, p)))) : null,
      errorsEl,
      setupProgress(su),
      gatewayCard(),
      tokenCard(),
      testCard(),
      locationCard(),
      arraysCard(),
      batteryCard(),
      tariffCard(),
      alertsCard(),
      serverCard(pwNew),
      weatherCard(),
      dataCard(),
      saveBar,
      diagnosticsCard(),
      aboutCard(),
    );
  }

  function setupProgress(su: ConfigResp['setup']) {
    const steps: [boolean, string][] = [
      [su.gateway_host, '1 Gateway address'],
      [su.cert_pinned, '1 Certificate pinned'],
      [su.token, '2 Token'],
      [su.location, '4 Location'],
      [su.arrays, '5 Solar arrays'],
      [su.tariff, '7 Rates'],
    ];
    return h('div', { class: 'steps' }, ...steps.map(([ok, l]) => h('span', { class: `step ${ok ? 'done' : ''}`, title: ok ? 'Done' : 'Not set yet' }, ok ? '✓ ' : '○ ', l)));
  }

  function gatewayCard() {
    const fpOut = h('div');
    const g = cfg.gateway;
    const readCert = async () => {
      replace(fpOut, h('div', { class: 'muted small' }, 'Contacting the gateway…'));
      try {
        const r = await post<{ fingerprint: string; note: string }>('/setup/pin', { host: g.host });
        replace(
          fpOut,
          note('info', h('div', null, 'The gateway presented this certificate (SHA-256):'), h('code', { class: 'fp' }, r.fingerprint), h('div', { class: 'small' }, r.note)),
          h(
            'button',
            {
              title: 'Pin this certificate: RustSolar will only talk to a gateway presenting exactly this certificate',
              onclick: async () => {
                cfg.gateway.cert_sha256 = r.fingerprint;
                cfg.gateway.tls = 'pin';
                if (await save({}, true)) toast('Certificate pinned');
              },
            },
            'Use this certificate',
          ),
        );
      } catch (e) {
        replace(fpOut, note('err', errText(e)));
      }
    };
    return card(
      '1 · Gateway',
      h('p', { class: 'small muted' }, 'Your Enphase IQ Gateway on the home network. RustSolar only reads from it (HTTP GET); it never changes battery, relay or grid settings.'),
      fields(
        textField('Address', 'IP address or host name of the gateway, e.g. 192.168.1.50', () => g.host, (v) => (g.host = v ?? ''), { placeholder: '192.168.x.x' }),
        selectField(
          'Certificate check',
          'How the gateway’s HTTPS certificate is checked. Pinning is recommended.',
          [
            ['pin', 'Pinned fingerprint (recommended)'],
            ['ca', 'CA certificate file'],
            ['insecure_dev', 'None — development only'],
          ],
          () => g.tls,
          (v) => (g.tls = v as Config['gateway']['tls']),
        ),
        textField('CA file (CA mode)', 'Path on the server to a PEM CA certificate', () => g.ca_cert_path, (v) => (g.ca_cert_path = v), { optional: true }),
        numField('Request timeout', 'Seconds before a gateway request is abandoned', () => g.request_timeout_s, (v) => (g.request_timeout_s = v ?? 10), { min: 1, max: 120, unit: 's' }),
        checkField('Keep raw responses', 'Store sanitized raw gateway responses for debugging (last 500)', () => g.store_raw, (v) => (g.store_raw = v)),
      ),
      g.tls === 'insecure_dev' ? note('warn', 'Certificate checking is OFF. Use only while setting up, then pin the certificate.') : null,
      h('div', { class: 'small' }, 'Pinned: ', g.cert_sha256 ? h('code', { class: 'fp' }, g.cert_sha256) : h('span', { class: 'muted' }, 'not yet')),
      h('div', { class: 'row' }, h('button', { title: 'Connect to the gateway and show its certificate fingerprint so you can pin it', onclick: readCert }, 'Read certificate')),
      fpOut,
    );
  }

  function tokenCard() {
    const input = h('input', { type: 'password', autocomplete: 'off', placeholder: 'eyJ…', title: 'Paste the long token text. It is stored on the server only and never shown again.' });
    const info = h('div');
    const btn = h(
      'button',
      {
        title: 'Store the token on the server (owner-only file) and start collecting',
        onclick: async () => {
          try {
            const r = await post<{ expires_at: number | null }>('/setup/token', { token: input.value });
            input.value = '';
            toast(r.expires_at ? `Token saved; expires ${dateTime(r.expires_at)}` : 'Token saved');
            void loadDiag();
          } catch (e) {
            toast(errText(e), 'error');
          }
        },
      },
      'Save token',
    );
    tokenInfoEl = info;
    return card(
      '2 · Gateway token',
      h('p', { class: 'small muted' }, 'Get a homeowner token by signing in at entrez.enphaseenergy.com (choose your system and gateway serial). Tokens expire; RustSolar warns two weeks ahead. The token is never sent to the browser or anywhere except your gateway.'),
      h('div', { class: 'row' }, input, btn),
      info,
    );
  }
  let tokenInfoEl: HTMLElement | null = null;

  function testCard() {
    const out = h('div');
    const test = async () => {
      replace(out, h('div', { class: 'muted small' }, 'Testing…'));
      try {
        const r = await post<{ ok: boolean; latency_ms?: number; error?: string; reading?: { pv_w: number; load_w: number; storage_w: number; grid_w: number; soc_pct: number | null } }>('/setup/test');
        replace(
          out,
          r.ok && r.reading
            ? note('ok', `Connected in ${r.latency_ms} ms — solar ${num(r.reading.pv_w / 1000, 2)} kW, home ${num(r.reading.load_w / 1000, 2)} kW, battery ${num(r.reading.storage_w / 1000, 2)} kW, grid ${num(r.reading.grid_w / 1000, 2)} kW, SOC ${num(r.reading.soc_pct, 0)} %.`)
            : note('err', r.error ?? 'Failed'),
        );
      } catch (e) {
        replace(out, note('err', errText(e)));
      }
    };
    const disc = h('div');
    const discover = async () => {
      replace(disc, h('div', { class: 'muted small' }, 'Probing endpoints (read-only GET)…'));
      try {
        const r = await post<{ results: { path: string; ok: boolean; status: string; latency_ms: number | null; top_level_keys: string[] }[] }>('/discover');
        replace(
          disc,
          h('table', null, h('tr', null, h('th', null, 'Endpoint'), h('th', null, 'Result'), h('th', null, 'Contents')), ...r.results.map((x) => h('tr', null, h('td', null, h('code', null, x.path)), h('td', { class: x.ok ? 'ok-text' : 'muted' }, x.ok ? `OK${x.latency_ms ? ` (${x.latency_ms} ms)` : ''}` : x.status), h('td', { class: 'small muted' }, x.top_level_keys.join(', '))))),
        );
      } catch (e) {
        replace(disc, note('err', errText(e)));
      }
    };
    return card('3 · Test connection', h('div', { class: 'row' }, h('button', { title: 'Read live data from the gateway once', onclick: test }, 'Test connection'), h('button', { title: 'Check which read-only gateway endpoints are available', onclick: discover }, 'Discover endpoints')), out, disc);
  }

  function locationCard() {
    const loc = cfg.location ?? { latitude: NaN, longitude: NaN, elevation_m: null, time_zone: Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC' };
    const sync = () => {
      cfg.location = isFinite(loc.latitude) && isFinite(loc.longitude) ? loc : null;
    };
    const tzField = textField('Time zone', 'IANA time zone, e.g. America/Los_Angeles', () => loc.time_zone, (v) => { loc.time_zone = v ?? 'UTC'; sync(); }, { list: 'tz-list' });
    return card(
      '4 · Location',
      h('p', { class: 'small muted' }, 'Used locally for sun position and sent (rounded to ~1 km) to Open-Meteo for the weather forecast. Enter your coordinates; RustSolar does not guess them.'),
      fields(
        numField('Latitude', 'Decimal degrees, north positive', () => (isFinite(loc.latitude) ? loc.latitude : null), (v) => { loc.latitude = v ?? NaN; sync(); }, { min: -90, max: 90, optional: true, unit: '°' }),
        numField('Longitude', 'Decimal degrees, east positive (California is negative)', () => (isFinite(loc.longitude) ? loc.longitude : null), (v) => { loc.longitude = v ?? NaN; sync(); }, { min: -180, max: 180, optional: true, unit: '°' }),
        numField('Elevation', 'Optional, metres above sea level', () => loc.elevation_m, (v) => { loc.elevation_m = v; sync(); }, { min: -500, max: 9000, optional: true, unit: 'm' }),
        tzField,
      ),
      ...(resp?.warnings ?? []).map((w) => note('warn', w)),
      h('datalist', { id: 'tz-list' }, ...TZS.map((t) => h('option', { value: t }))),
      h(
        'div',
        { class: 'row' },
        h(
          'button',
          {
            class: 'ghost',
            title: 'Fill in this browser’s time zone',
            onclick: () => {
              loc.time_zone = Intl.DateTimeFormat().resolvedOptions().timeZone;
              (tzField.querySelector('input') as HTMLInputElement).value = loc.time_zone;
              sync();
            },
          },
          'Use browser time zone',
        ),
      ),
    );
  }

  function arraysCard() {
    const list = h('div');
    const renderRows = () => {
      replace(
        list,
        cfg.arrays.length ? null : h('div', { class: 'muted small' }, 'No arrays yet. Without them, an empirical model is learned from your history instead.'),
        ...cfg.arrays.map((a: ArraySpec, i: number) =>
          h(
            'div',
            { class: 'array-row' },
            fields(
              textField('Name', 'A label for this roof face', () => a.name, (v) => (a.name = v ?? '')),
              numField('DC capacity', 'Panel nameplate total (STC), watts', () => a.dc_capacity_w, (v) => (a.dc_capacity_w = v ?? 0), { min: 1, max: 999999, unit: 'W' }),
              numField('Tilt', 'Roof pitch from horizontal', () => a.tilt_deg, (v) => (a.tilt_deg = v ?? 0), { min: 0, max: 90, unit: '°' }),
              numField('Azimuth', 'Direction the panels face, clockwise from north (180 = south)', () => a.azimuth_deg, (v) => (a.azimuth_deg = v ?? 180), { min: 0, max: 360, unit: '°' }),
              numField('AC limit', 'Sum of microinverter continuous AC ratings (optional clip point)', () => a.ac_limit_w, (v) => (a.ac_limit_w = v), { min: 1, max: 999999, unit: 'W', optional: true }),
              numField('Temp. coefficient', 'Power change per °C, from the panel datasheet (e.g. −0.35)', () => a.temp_coeff_per_c, (v) => (a.temp_coeff_per_c = v ?? -0.0037), { min: -1, max: 0, unit: '%/°C', scale: 100 }),
              numField('System losses', 'Wiring, soiling, mismatch etc. Default 14 % (PVWatts)', () => a.system_losses, (v) => (a.system_losses = v ?? 0.14), { min: 0, max: 50, unit: '%', scale: 100 }),
              numField('Inverter efficiency', 'Microinverter efficiency (datasheet CEC value)', () => a.inverter_efficiency, (v) => (a.inverter_efficiency = v ?? 0.97), { min: 80, max: 100, unit: '%', scale: 100 }),
              numField('Ground albedo', 'Ground reflectance, 0.2 typical', () => a.albedo, (v) => (a.albedo = v ?? 0.2), { min: 0, max: 1 }),
            ),
            h('button', { class: 'danger ghost', title: 'Remove this array', onclick: () => { cfg.arrays.splice(i, 1); renderRows(); } }, 'Remove'),
          ),
        ),
        h(
          'button',
          {
            title: 'Add a roof face with its own tilt and direction',
            onclick: () => {
              cfg.arrays.push({ name: `Array ${cfg.arrays.length + 1}`, dc_capacity_w: 0, tilt_deg: 20, azimuth_deg: 180, ac_limit_w: null, temp_coeff_per_c: -0.0037, system_losses: 0.14, inverter_efficiency: 0.97, albedo: 0.2, shading: [] });
              renderRows();
            },
          },
          '+ Add array',
        ),
      );
    };
    renderRows();
    return card('5 · Solar arrays', h('p', { class: 'small muted' }, 'From your installation documents or panel datasheets. Defaults marked in tooltips are generic estimates — replace them with your hardware’s values.'), list);
  }

  function batteryCard() {
    const b = cfg.battery;
    return card(
      '6 · Battery',
      h('p', { class: 'small muted' }, 'Leave capacity and power blank to use the gateway inventory (or the stored-energy estimate).'),
      fields(
        numField('Usable capacity', 'Total usable energy of all batteries (optional)', () => b.usable_kwh, (v) => (b.usable_kwh = v), { min: 0.5, max: 500, unit: 'kWh', optional: true }),
        numField('Max power', 'Combined continuous charge/discharge power (optional)', () => b.max_power_kw, (v) => (b.max_power_kw = v), { min: 0.1, max: 200, unit: 'kW', optional: true }),
        numField('Round-trip efficiency', 'AC to AC; replaced by the observed value as history builds', () => b.round_trip_eff, (v) => (b.round_trip_eff = v ?? 0.89), { min: 50, max: 100, unit: '%', scale: 100 }),
        numField('Outage floor', 'Lowest charge the battery reaches during an outage', () => b.outage_floor_pct, (v) => (b.outage_floor_pct = v ?? 0), { min: 0, max: 100, unit: '%' }),
        numField('Essential load', 'Your estimate of must-run loads during an outage', () => b.essential_load_kw, (v) => (b.essential_load_kw = v ?? 0.5), { min: 0, max: 50, unit: 'kW' }),
      ),
    );
  }

  function tariffCard() {
    const ta = h('textarea', { class: 'code', rows: 16, spellcheck: 'false', title: 'Rate plans as JSON. The server checks every hour and month is covered.' });
    const showJson = () => (ta.value = JSON.stringify(cfg.tariff, null, 2));
    showJson();
    const msg = h('div', { class: 'field-msg' });
    const exportBox = h('div');
    ta.addEventListener('input', () => {
      try {
        const v = JSON.parse(ta.value);
        cfg.tariff = v;
        msg.textContent = '';
        ta.classList.remove('invalid');
        renderExport();
      } catch (e) {
        msg.textContent = `JSON error: ${errText(e)}`;
        ta.classList.add('invalid');
      }
    });

    type Plan = { export?: ExportJson | null };
    const plans = () => (cfg.tariff.plans ?? []) as Plan[];
    const mode = (): string => {
      const ex = plans().map((p) => p.export?.type ?? 'none');
      if (!ex.length) return 'none';
      if (ex.every((x) => x === 'pge_nbt')) return 'pge_nbt';
      if (ex.every((x) => x === 'none')) return 'none';
      return 'custom';
    };
    const setExport = (e: ExportJson) => {
      for (const p of plans()) p.export = { ...e };
      showJson();
      renderExport();
    };

    function renderExport() {
      if (!plans().length) {
        replace(exportBox, h('p', { class: 'small muted' }, 'Add a rate plan in the JSON below first, then choose how exports are credited.'));
        return;
      }
      const m = mode();
      const cur = (plans().find((p) => p.export?.type === 'pge_nbt')?.export ?? { type: 'pge_nbt', vintage: 2024, generation: true, acc_plus: true, pto_date: null }) as ExportJson;
      const choices: [string, string][] = [
        ['none', 'None (exports earn $0)'],
        ['pge_nbt', 'PG&E Net Billing (NEM 3)'],
      ];
      if (m === 'custom') choices.push(['custom', 'Other (set in the JSON below)']);
      const parts: Child[] = [
        fields(selectField('Export credits', 'How PG&E credits the solar energy you send to the grid. Applies to every rate plan version below.', choices, () => m, (v) => {
          if (v === 'pge_nbt') setExport(cur);
          else if (v === 'none') setExport({ type: 'none' });
        })),
      ];
      if (m === 'pge_nbt') {
        const vint = nbtInfo?.vintages ?? [];
        const plus = vint.find((v) => v.vintage === cur.vintage)?.acc_plus;
        const upd = (patch: Partial<ExportJson>) => setExport({ ...cur, ...patch });
        const years = vint.length ? vint.map((v) => v.vintage) : [2023, 2024, 2025, 2026];
        parts.push(
          fields(
            selectField(
              'Application year (vintage)',
              'The calendar year your complete interconnection application was submitted (your installer or PG&E’s approval email has it). It can be earlier than the year the system was turned on. It locks your export prices for 9 years.',
              years.map((v) => [String(v), String(v)] as [string, string]),
              () => String(cur.vintage),
              (v) => upd({ vintage: Number(v) }),
            ),
            textField(
              'Permission to operate',
              'Date PG&E gave permission to operate (PTO), YYYY-MM-DD. The ACC Plus bonus ends 9 years after it. Optional.',
              () => cur.pto_date ?? null,
              (v) => upd({ pto_date: v && /^\d{4}-\d{2}-\d{2}$/.test(v) ? v : null }),
              { optional: true, placeholder: 'YYYY-MM-DD' },
            ),
          ),
          checkField(
            'PG&E supplies my generation',
            'Leave on while your bill shows PG&E generation charges. Turn off if a community choice provider (CCA) supplies your generation; then only the delivery part of the credit comes from PG&E.',
            () => cur.generation !== false,
            (v) => upd({ generation: v }),
          ),
          checkField(
            `ACC Plus bonus${plus ? ` (+$${plus}/kWh for ${cur.vintage})` : ''}`,
            'Extra export credit for most residential Net Billing systems, fixed for 9 years from PTO. Not available if you bought a home with an existing system or moved over from NEM 1 or 2. It shows as its own line on your bill.',
            () => !!cur.acc_plus,
            (v) => upd({ acc_plus: v }),
          ),
          checkField(
            'PG&E holidays count as weekends',
            'New Year’s, Presidents Day, Memorial Day, Independence Day, Labor Day, Veterans Day, Thanksgiving and Christmas (on their observed dates) use weekend prices, as PG&E does',
            () => !!cfg.tariff.pge_holidays,
            (v) => {
              cfg.tariff.pge_holidays = v;
              showJson();
            },
          ),
          nbtPreview(cur),
          h('p', { class: 'small muted' }, 'Prices from PG&E’s published Net Billing export tables (pge.com/eecvalues), built into RustSolar. Export credits can only reduce energy charges, not the fixed daily charge; unused credits carry forward to your true-up.'),
        );
      }
      replace(exportBox, ...parts);
    }

    function nbtPreview(e: ExportJson): HTMLElement {
      const box = h('div', { class: 'nbt-preview' }, h('div', { class: 'muted small' }, 'Loading export prices…'));
      let weekend = false;
      const months = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
      const hourLabel = (hr: number) => (hr === 0 ? '12a' : hr < 12 ? `${hr}a` : hr === 12 ? '12p' : `${hr - 12}p`);
      const lo = Math.log(0.005);
      const span = Math.log(2.6) - lo;
      const shade = (v: number) => Math.max(0.06, Math.min(1, (Math.log(Math.max(v, 0.005)) - lo) / span));
      const draw = (r: NbtResp) => {
        const tb = r.table;
        if (!tb) return;
        const grid = weekend ? tb.weekend : tb.weekday;
        const plus = e.acc_plus ? (r.vintages.find((v) => v.vintage === e.vintage)?.acc_plus ?? 0) : 0;
        const toggle = h(
          'select',
          {
            title: 'Weekday or weekend/holiday prices',
            onchange: (ev: Event) => {
              weekend = (ev.target as HTMLSelectElement).value === 'we';
              draw(r);
            },
          },
          h('option', { value: 'wd' }, 'Weekdays'),
          h('option', { value: 'we' }, 'Weekends and holidays'),
        );
        toggle.value = weekend ? 'we' : 'wd';
        const cells: HTMLElement[] = [h('div')];
        for (let hr = 0; hr < 24; hr++) cells.push(h('div', { class: 'heat-h' }, hr % 3 === 0 ? hourLabel(hr) : ''));
        grid.forEach((row, m) => {
          cells.push(h('div', { class: 'heat-m' }, months[m]));
          row.forEach((v, hr) => {
            const tip = `${months[m]} ${hourLabel(hr)}–${hourLabel((hr + 1) % 24)}: $${(v + plus).toFixed(4)}/kWh${plus ? ` (incl. ACC Plus $${plus})` : ''}`;
            cells.push(h('div', { class: 'heat-c', style: `--a:${shade(v).toFixed(3)}`, title: tip }));
          });
        });
        replace(
          box,
          h('div', { class: 'row' }, h('strong', null, `Export price by hour, ${tb.year}`), h('div', { class: 'spacer' }), toggle),
          h('div', { class: 'heat' }, ...cells),
          h('p', { class: 'small muted' }, 'Darker = higher price. Hover a cell for the exact price. Summer and early-autumn evenings (roughly 5–9 pm) are worth far more than midday, so energy exported in the evening earns the most.'),
        );
      };
      get<NbtResp>(`/tariff/pge-nbt?vintage=${e.vintage}&generation=${e.generation !== false}`)
        .then(draw)
        .catch((x) => replace(box, note('err', errText(x))));
      return box;
    }

    if (!nbtInfo) {
      get<NbtResp>('/tariff/pge-nbt')
        .then((r) => {
          nbtInfo = r;
          renderExport();
        })
        .catch(() => undefined);
    }
    renderExport();

    return card(
      '7 · Electricity rates',
      h('p', { class: 'small muted' }, 'RustSolar never assumes utility prices. Enter your plan’s time-of-use prices and fixed charge from your bill in the JSON below (docs/tariff-example.toml shows every option), and choose how exports are credited here.'),
      checkField('Use these rates for cost estimates', 'Turn cost estimates on or off (sets "enabled" in the JSON)', () => !!cfg.tariff.enabled, (v) => {
        cfg.tariff.enabled = v;
        showJson();
      }),
      exportBox,
      h('h3', null, 'Rate plans (JSON)'),
      h(
        'div',
        { class: 'row' },
        h('button', { title: 'Replace the editor contents with an example structure (placeholder prices, NOT real rates)', onclick: () => { ta.value = JSON.stringify(EXAMPLE_TARIFF, null, 2); ta.dispatchEvent(new Event('input')); } }, 'Insert example structure'),
        h('span', { class: 'small warn-text' }, 'Example prices are placeholders — replace them all.'),
      ),
      ta,
      msg,
    );
  }

  function alertsCard() {
    const a = cfg.alerts;
    return card(
      '8 · Alerts',
      fields(
        numField('Low battery', 'Alert when charge falls below this and the battery is not charging', () => a.low_soc_pct, (v) => (a.low_soc_pct = v ?? 25), { min: 0, max: 100, unit: '%' }),
        numField('Underperformance', 'Alert when clear-sky solar is this far below expectation', () => a.underperformance_frac, (v) => (a.underperformance_frac = v ?? 0.25), { min: 5, max: 90, unit: '%', scale: 100 }),
        numField('High load factor', 'Flag household use this many times above typical for the hour', () => a.high_load_factor, (v) => (a.high_load_factor = v ?? 2), { min: 1.1, max: 10, unit: '×' }),
        numField('Quiet hours start', 'Local hour when notifications pause (alerts are still recorded)', () => a.quiet_start_hour, (v) => (a.quiet_start_hour = v ?? 22), { min: 0, max: 23, step: 1, unit: 'h' }),
        numField('Quiet hours end', 'Local hour when notifications resume', () => a.quiet_end_hour, (v) => (a.quiet_end_hour = v ?? 7), { min: 0, max: 23, step: 1, unit: 'h' }),
        numField('Repeat after', 'Minimum minutes before the same alert can be raised again', () => a.repeat_after_min, (v) => (a.repeat_after_min = v ?? 240), { min: 1, max: 100000, step: 1, unit: 'min' }),
        textField('Webhook URL', 'Optional local URL that receives new alerts as JSON (e.g. Home Assistant). Off when blank.', () => a.webhook_url, (v) => (a.webhook_url = v), { optional: true, placeholder: 'http://homeassistant.local:8123/api/webhook/…' }),
      ),
    );
  }

  function serverCard(pwNew: { v: string }) {
    const s = cfg.server;
    const pw = h('input', { type: 'password', autocomplete: 'new-password', placeholder: s.password_set ? '(unchanged)' : 'none set', title: 'New dashboard password, at least 8 characters. Saved with “Save settings”.' });
    pw.addEventListener('input', () => (pwNew.v = pw.value));
    return card(
      '9 · Dashboard access',
      h('p', { class: 'small muted' }, 'Listening on a non-local address (e.g. 0.0.0.0:8080 for the home network) requires a password unless you explicitly accept the risk.'),
      fields(
        textField('Listen address', 'IP:port, e.g. 127.0.0.1:8080 (this PC only) or 0.0.0.0:8080 (home network). Needs a restart.', () => s.bind, (v) => (s.bind = v ?? '127.0.0.1:8080')),
        h('label', { class: 'field', title: 'New dashboard password' }, h('span', null, 'New password'), pw),
        numField('Login lasts', 'Days before you must sign in again', () => s.session_days, (v) => (s.session_days = v ?? 30), { min: 1, max: 365, step: 1, unit: 'days' }),
        checkField('Allow LAN without password', 'Not recommended: anyone on your network could view the dashboard', () => s.allow_unauthenticated_lan, (v) => (s.allow_unauthenticated_lan = v)),
      ),
      h('h3', null, 'Easy address on your home network'),
      fields(
        checkField('Announce on the network', 'Lets phones and computers on your home network find the dashboard by name (mDNS, the same way printers are found). Works with any DNS setting, including public DNS. Needs a restart.', () => s.mdns, (v) => (s.mdns = v)),
        textField('Name', 'The address will be http://<name>.local. Lowercase letters, digits and dashes. Needs a restart.', () => s.mdns_name, (v) => (s.mdns_name = (v ?? 'rustsolar').trim().toLowerCase())),
        checkField('Also use port 80', 'Also listen on the standard web port so the address needs no :port number. Skipped with a log warning if another program already uses port 80. Needs a restart.', () => s.port80, (v) => (s.port80 = v)),
      ),
      h('p', { class: 'small muted' }, `Address: http://${s.mdns_name || 'rustsolar'}.local${s.port80 ? '' : `:${s.bind.split(':').pop()}`} — works once RustSolar listens on the home network (not 127.0.0.1). A firewall on the server must allow the port${s.port80 ? ' (80)' : ''}.`),
      s.password_set ? h('div', { class: 'row' }, h('span', { class: 'small ok-text' }, 'A password is set.'), confirmButton('Remove password', 'Turn off the dashboard password', 'Remove the password?', async () => { await save({ clear_password: true }); }, 'danger')) : null,
    );
  }

  function weatherCard() {
    const w = cfg.weather;
    const c = cfg.collector;
    return card(
      '10 · Weather and collection',
      fields(
        checkField('Weather forecasts', 'Download forecasts from Open-Meteo (free, no key)', () => w.enabled, (v) => (w.enabled = v)),
        checkField('Air quality / smoke', 'Also fetch the air-quality forecast', () => w.air_quality, (v) => (w.air_quality = v)),
        numField('Refresh every', 'Minutes between forecast downloads', () => w.refresh_min, (v) => (w.refresh_min = v ?? 60), { min: 15, max: 1440, step: 1, unit: 'min' }),
        numField('Daily request budget', 'Hard cap on weather requests per day (free limit 10,000)', () => w.daily_request_budget, (v) => (w.daily_request_budget = v ?? 200), { min: 1, max: 5000, step: 1 }),
        numField('Forecast days', 'Days of forecast to fetch', () => w.forecast_days, (v) => (w.forecast_days = v ?? 7), { min: 1, max: 16, step: 1 }),
        checkField('Collect gateway data', 'Poll the gateway', () => c.enabled, (v) => (c.enabled = v)),
        numField('Poll interval', 'Seconds between gateway reads (the gateway updates about every 12 s)', () => c.interval_s, (v) => (c.interval_s = v ?? 12), { min: 5, max: 300, step: 1, unit: 's' }),
        selectField(
          'Warn about missing data after',
          'How long without a new gateway reading before the dashboard shows a warning, greys out the live values and raises an alert. A single missed reading is normal on Wi-Fi; 5 minutes keeps that quiet.',
          [['0', 'Immediately'], ['1', '1 minute'], ['2', '2 minutes'], ['5', '5 minutes (default)'], ['10', '10 minutes'], ['15', '15 minutes'], ['30', '30 minutes'], ['60', '1 hour']],
          () => String(c.warn_after_min ?? 5),
          (v) => (c.warn_after_min = Number(v)),
        ),
        numField('Stale after', 'Readings older than this are not used for live calculations such as recommendations (warnings use the setting above)', () => c.stale_after_s, (v) => (c.stale_after_s = v ?? 60), { min: 10, max: 3600, step: 1, unit: 's' }),
        numField('Max integration gap', 'Gaps longer than this are not counted as energy', () => c.max_integration_gap_s, (v) => (c.max_integration_gap_s = v ?? 120), { min: 10, max: 3600, step: 1, unit: 's' }),
      ),
    );
  }

  function dataCard() {
    const r = cfg.retention;
    const b = cfg.backup;
    return card(
      '11 · Data, backup and model',
      fields(
        numField('Keep raw readings', 'Days of individual readings (hourly totals are kept forever)', () => r.raw_days, (v) => (r.raw_days = v ?? 90), { min: 2, max: 3650, step: 1, unit: 'days' }),
        numField('Keep 5-minute data', 'Days of 5-minute averages', () => r.five_min_days, (v) => (r.five_min_days = v ?? 1095), { min: 2, max: 36500, step: 1, unit: 'days' }),
        numField('Keep diagnostics', 'Days of diagnostic events', () => r.diagnostics_days, (v) => (r.diagnostics_days = v ?? 30), { min: 1, max: 3650, step: 1, unit: 'days' }),
        checkField('Daily backup', 'Write a database backup once a day', () => b.enabled, (v) => (b.enabled = v)),
        numField('Backup hour', 'Local hour for the daily backup', () => b.hour, (v) => (b.hour = v ?? 3), { min: 0, max: 23, step: 1, unit: 'h' }),
        numField('Backups kept', 'Number of daily backups to keep', () => b.keep, (v) => (b.keep = v ?? 14), { min: 1, max: 365, step: 1 }),
        textField('Backup folder', 'Optional; default is the backups folder in the data directory', () => b.dir, (v) => (b.dir = v), { optional: true }),
      ),
      h(
        'div',
        { class: 'row' },
        h('button', { title: 'Write a backup now', onclick: async () => { try { const r2 = await post<{ path: string }>('/backup'); toast(`Backup written: ${r2.path}`); } catch (e) { toast(errText(e), 'error'); } } }, 'Back up now'),
        confirmButton('Reset learned forecast model', 'Forget learned solar corrections and start calibrating again', 'Forget all learned corrections?', async () => { try { await post('/model/reset'); toast('Forecast model reset'); } catch (e) { toast(errText(e), 'error'); } }, 'danger'),
      ),
    );
  }

  const diagEl = h('div');
  function diagnosticsCard() {
    return card('Diagnostics', diagEl);
  }

  async function loadDiag() {
    let d: Diag;
    try {
      d = await get<Diag>('/diagnostics');
    } catch (e) {
      replace(diagEl, note('err', errText(e)));
      return;
    }
    if (cancelled) return;
    const c = d.collector;
    const t = d.token;
    if (tokenInfoEl) {
      replace(
        tokenInfoEl,
        t.present
          ? note(t.days_left !== null && t.days_left !== undefined && t.days_left < 14 ? 'warn' : 'ok', `Token stored (${t.source}). ${t.expires_at ? `Expires ${dateTime(t.expires_at)} (${num(t.days_left ?? null, 0)} days).` : 'Expiry unknown.'}${t.role ? ` Role: ${t.role}.` : ''}`)
          : note('warn', t.error ?? 'No token stored yet.'),
      );
    }
    const kv = (rows: [string, Child][]) => h('table', { class: 'kv' }, ...rows.map(([k, v]) => h('tr', null, h('th', null, k), h('td', { class: 'num' }, v))));
    const tlsMatch = d.tls.pinned && d.tls.seen ? d.tls.pinned.replace(/:/g, '').toUpperCase() === d.tls.seen.replace(/:/g, '').toUpperCase() : null;
    replace(
      diagEl,
      h(
        'div',
        { class: 'grid-2' },
        h(
          'div',
          null,
          h('h3', null, 'Collector'),
          kv([
            ['State', store.live?.collector ?? '—'],
            ['Polls / OK / failed', `${c.polls} / ${c.successes} / ${c.failures}`],
            ['Repeated readings', String(c.duplicates)],
            ['Gaps not integrated', String(c.gaps)],
            ['Unbalanced readings', String(c.unbalanced)],
            ['Latency last / avg / max', `${c.last_latency_ms ?? '—'} / ${c.avg_latency_ms !== null ? c.avg_latency_ms.toFixed(0) : '—'} / ${c.max_latency_ms ?? '—'} ms`],
            ['Gateway update interval', c.last_update_interval_s !== null ? `${c.last_update_interval_s} s` : '—'],
            ['Gateway clock skew', c.clock_skew_s !== null ? `${c.clock_skew_s} s` : '—'],
            ['Current back-off', c.current_backoff_s ? `${c.current_backoff_s} s` : 'none'],
            ['Last success', ago(c.last_success_at, store.now())],
            ['Last error', c.last_error ?? '—'],
            ['Failures by kind', Object.entries(c.failures_by_kind).map(([k, v]) => `${k}: ${v}`).join(', ') || '—'],
            ['Running for', duration(store.now() - c.started_at)],
          ]),
          h('h3', null, 'Measurements'),
          kv([
            ['Balance error (24 h)', d.store.residual_24h.mean_abs_w !== null ? `mean ${d.store.residual_24h.mean_abs_w.toFixed(0)} W, max ${d.store.residual_24h.max_abs_w?.toFixed(0)} W` : '—'],
            ['enc_agg_energy ÷ SOC', d.store.enc_agg_energy_implied_capacity_wh ? `${(d.store.enc_agg_energy_implied_capacity_wh / 1000).toFixed(2)} kWh (implied capacity; check against inventory)` : '—'],
            ['First reading', dateTime(d.store.first_sample)],
          ]),
        ),
        h(
          'div',
          null,
          h('h3', null, 'Security'),
          kv([
            ['TLS mode', d.tls.mode],
            ['Pinned', d.tls.pinned ? h('code', { class: 'fp' }, d.tls.pinned) : '—'],
            ['Seen', d.tls.seen ? h('code', { class: 'fp' }, d.tls.seen) : '—'],
            ['Match', tlsMatch === null ? '—' : tlsMatch ? h('span', { class: 'ok-text' }, 'yes') : h('span', { class: 'err-text' }, 'NO')],
            ['Token', t.present ? `present (${t.source})` : 'missing'],
          ]),
          h('h3', null, 'Storage'),
          kv([
            ['Data folder', d.data_dir],
            ['Database size', d.db_bytes !== null ? `${(d.db_bytes / 1048576).toFixed(1)} MB` : '—'],
            ['Readings / hours / weather rows', `${d.store.rows.samples} / ${d.store.rows.hours} / ${d.store.rows.weather_hours}`],
          ]),
          h('h3', null, 'Weather provider'),
          kv([
            ['Provider', d.store.weather.attribution],
            ['Requests today', `${d.store.weather.requests_today} / ${d.store.weather.daily_budget}`],
            ['Last success', ago(d.store.weather.last_success_at, store.now())],
            ['Last error', d.store.weather.last_error ?? '—'],
          ]),
          h('h3', null, 'Gateway endpoints'),
          d.store.endpoints.length ? kv(d.store.endpoints.map(([p, s]) => [p, s])) : h('div', { class: 'muted small' }, 'Not probed yet (inventory runs hourly; or use Discover endpoints).'),
        ),
      ),
      h('h3', null, 'Recent events'),
      d.store.events.length
        ? h('table', { class: 'events' }, ...d.store.events.slice(0, 60).map((e) => h('tr', { class: `lvl-${e.level}` }, h('td', { class: 'small num' }, dateTime(e.ts)), h('td', { class: 'small' }, e.source), h('td', { class: 'small' }, e.level), h('td', { class: 'small' }, e.message))))
        : h('div', { class: 'muted small' }, 'No events.'),
    );
  }

  function aboutCard() {
    return card(
      'About',
      h('p', null, h('strong', null, 'RustSolar'), ` ${store.version || __APP_VERSION__} — Enphase Energy Intelligence.`),
      h('p', { class: 'small' }, 'Licensed under PolyForm Noncommercial 1.0.0: free for personal and other noncommercial use. Commercial use, including distribution by solar installers, requires a separate license. A RocketSalvo project by Geoffrey Huber.'),
      h('p', { class: 'small muted' }, 'Read-only with respect to the Enphase system: it never changes battery, relay, grid or charging settings. Forecasts, costs and battery runtimes are estimates. No subscriptions, cloud accounts, analytics or advertising. Weather data by Open-Meteo.com (CC BY 4.0). Enphase and IQ are trademarks of Enphase Energy, Inc.; this project is not affiliated with Enphase.'),
    );
  }

  void reload().then(() => loadDiag());
  const iv = window.setInterval(() => void loadDiag(), 15000);
  return () => {
    cancelled = true;
    window.clearInterval(iv);
  };
}
