import { get, post, errText } from '../api';
import { COLORS, ChartSet } from '../charts';
import { every } from '../common';
import { card, empty, h, note, replace, stat, toast } from '../dom';
import { ago, dateTime, dayLabel, num, rain, temp, tempUnit, tempVal, wind } from '../fmt';
import { store } from '../store';
import type { WeatherHour, WeatherResp } from '../types';

export function wmoText(code: number | null | undefined): string {
  if (code === null || code === undefined) return '—';
  if (code === 0) return 'Clear';
  if (code === 1) return 'Mainly clear';
  if (code === 2) return 'Partly cloudy';
  if (code === 3) return 'Overcast';
  if (code === 45 || code === 48) return 'Fog';
  if (code >= 51 && code <= 55) return 'Drizzle';
  if (code === 56 || code === 57) return 'Freezing drizzle';
  if (code >= 61 && code <= 65) return 'Rain';
  if (code === 66 || code === 67) return 'Freezing rain';
  if (code >= 71 && code <= 77) return 'Snow';
  if (code >= 80 && code <= 82) return 'Rain showers';
  if (code === 85 || code === 86) return 'Snow showers';
  if (code === 95) return 'Thunderstorm';
  if (code === 96 || code === 99) return 'Thunderstorm with hail';
  return 'Unknown';
}

interface SunYear {
  year: { date: string; sunrise: number | null; sunset: number | null; noon: number; day_length_s: number }[];
  time_zone: string;
}

const localHour = (ts: number | null) => {
  if (!ts) return null;
  const d = new Date(ts * 1000);
  return d.getHours() + d.getMinutes() / 60;
};
const hm = (v: number | null) => (v === null ? '—' : `${Math.floor(v)}:${String(Math.round((v % 1) * 60)).padStart(2, '0')}`);

function dailySummary(hours: WeatherHour[]) {
  const by = new Map<string, WeatherHour[]>();
  for (const w of hours) {
    const d = new Date((w.ts - 1) * 1000);
    const key = `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
    if (!by.has(key)) by.set(key, []);
    by.get(key)!.push(w);
  }
  return [...by.entries()].map(([date, ws]) => {
    const temps = ws.map((w) => w.temp_c).filter((x): x is number => x !== null);
    const codes = ws.filter((w) => w.is_day === 1).map((w) => w.code ?? 0);
    return {
      date,
      tmin: temps.length ? Math.min(...temps) : null,
      tmax: temps.length ? Math.max(...temps) : null,
      precip: Math.max(0, ...ws.map((w) => w.precip_prob ?? 0)),
      rain: ws.reduce((a, w) => a + (w.precip_mm ?? 0), 0),
      ghi: ws.reduce((a, w) => a + (w.ghi ?? 0), 0) / 1000,
      code: codes.length ? codes.sort((a, b) => codes.filter((x) => x === b).length - codes.filter((x) => x === a).length)[0] : null,
      gust: Math.max(0, ...ws.map((w) => w.gust_ms ?? 0)),
      n: ws.length,
    };
  });
}

export function weatherPage(root: HTMLElement) {
  const charts = new ChartSet();
  const nowEl = h('div');
  const daysEl = h('div');
  const statusEl = h('div');
  const aqEl = h('div');
  const tempChart = charts.add({
    height: 240,
    yLabel: tempUnit(),
    y2Label: '%',
    y2Range: [0, 100],
    series: [
      { label: 'Temperature', color: COLORS.danger, width: 2, value: (v) => (v === null ? '—' : `${v.toFixed(1)} ${tempUnit()}`) },
      { label: 'Cloud cover', color: COLORS.muted, scale: 'y2', fill: 0.12, value: (v) => (v === null ? '—' : `${v.toFixed(0)} %`) },
      { label: 'Rain chance', color: COLORS.grid, scale: 'y2', dash: [3, 3], value: (v) => (v === null ? '—' : `${v.toFixed(0)} %`) },
    ],
    data: [[], [], [], []],
  });
  const irrChart = charts.add({
    height: 240,
    yLabel: 'W/m²',
    series: [
      { label: 'Global (GHI)', color: COLORS.solar, width: 2, fill: 0.15, value: (v) => (v === null ? '—' : `${v.toFixed(0)} W/m²`) },
      { label: 'Direct normal (DNI)', color: COLORS.home, value: (v) => (v === null ? '—' : `${v.toFixed(0)} W/m²`) },
      { label: 'Diffuse (DHI)', color: COLORS.grid, value: (v) => (v === null ? '—' : `${v.toFixed(0)} W/m²`) },
    ],
    data: [[], [], [], []],
  });
  const yearChart = charts.add({
    height: 240,
    yLabel: 'Local time (h)',
    xFormat: 'date',
    yRange: [0, 24],
    series: [
      { label: 'Sunrise', color: COLORS.solar, width: 2, value: hm },
      { label: 'Solar noon', color: COLORS.muted, dash: [4, 4], value: hm },
      { label: 'Sunset', color: COLORS.home, width: 2, value: hm },
    ],
    data: [[], [], [], []],
  });
  const refreshBtn = h(
    'button',
    {
      class: 'primary',
      title: 'Download a fresh forecast now (counts toward the daily request budget)',
      onclick: async () => {
        try {
          await post('/weather/refresh');
          toast('Weather updated');
          void load();
        } catch (e) {
          toast(errText(e), 'error');
        }
      },
    },
    'Refresh now',
  );
  root.append(
    h('div', { class: 'grid-2' }, card('Current conditions', nowEl), card('Provider', statusEl, h('div', { class: 'row' }, refreshBtn))),
    card('Daily outlook', daysEl),
    h('div', { class: 'grid-2' }, card('Temperature and clouds', tempChart.el), card('Solar irradiance (hourly mean)', irrChart.el)),
    h('div', { class: 'grid-2' }, card('Air quality and smoke', aqEl), card('Sunrise and sunset through the year', yearChart.el)),
  );

  const load = async () => {
    const r = await get<WeatherResp>('/weather');
    const st = r.status;
    const stale = st.last_success_at !== null && store.now() - st.last_success_at > 3 * 3600;
    replace(
      statusEl,
      h('p', null, h('a', { href: 'https://open-meteo.com/', target: '_blank', rel: 'noopener noreferrer', title: 'Open-Meteo website' }, 'Weather data by Open-Meteo.com'), ' (', h('a', { href: 'https://creativecommons.org/licenses/by/4.0/', target: '_blank', rel: 'noopener noreferrer', title: 'Licence' }, 'CC BY 4.0'), '). Free non-commercial API; no key, no cost.'),
      h(
        'div',
        { class: 'stats' },
        stat('Last update', st.last_success_at ? ago(st.last_success_at, store.now()) : 'never', 'Last successful forecast download'),
        stat('Requests today', `${st.requests_today} / ${st.daily_budget}`, 'Outgoing weather requests today versus the configured budget (free limit is 10,000/day)'),
        stat('Next refresh', st.next_refresh_at ? dateTime(st.next_refresh_at) : '—', 'Scheduled refresh time'),
      ),
      st.last_error ? note('warn', `Last attempt failed: ${st.last_error}`) : null,
      stale ? note('warn', 'Using the cached forecast — the latest download is more than 3 hours old.') : null,
    );
    if (!r.hours.length) {
      replace(nowEl, empty('No weather data yet.', h('div', { class: 'small' }, 'Set your location in Settings; weather is fetched hourly. (Disabled in demo mode.)')));
      replace(daysEl, empty('No forecast yet.'));
      replace(aqEl, h('div', { class: 'muted small' }, 'No data.'));
    } else {
      const c = r.current;
      replace(
        nowEl,
        c
          ? h(
              'div',
              { class: 'stats' },
              stat('Sky', wmoText(c.weather_code), 'WMO weather code'),
              stat('Temperature', temp(c.temperature_2m, 1), 'Air temperature at 2 m'),
              stat('Cloud cover', `${num(c.cloud_cover, 0)} %`, 'Total cloud cover'),
              stat('Wind', wind(c.wind_speed_10m, 1), 'Wind speed at 10 m'),
            )
          : h('div', { class: 'muted small' }, 'Current conditions not in the cached forecast.'),
      );
      const days = dailySummary(r.hours.filter((w) => w.ts > store.now() - 3600));
      replace(
        daysEl,
        h(
          'table',
          null,
          h('tr', null, h('th', null, 'Day'), h('th', null, 'Sky (daytime)'), h('th', { class: 'r' }, 'Low / high'), h('th', { class: 'r' }, 'Rain chance'), h('th', { class: 'r' }, 'Rain'), h('th', { class: 'r' }, 'Max gust'), h('th', { class: 'r' }, 'Sunlight (GHI)')),
          ...days
            .filter((d) => d.n >= 6)
            .map((d) =>
              h(
                'tr',
                null,
                h('td', null, dayLabel(d.date)),
                h('td', null, wmoText(d.code)),
                h('td', { class: 'r num' }, `${num(tempVal(d.tmin), 0)} / ${num(tempVal(d.tmax), 0)} ${tempUnit()}`),
                h('td', { class: 'r num' }, `${d.precip.toFixed(0)} %`),
                h('td', { class: 'r num' }, rain(d.rain)),
                h('td', { class: 'r num' }, wind(d.gust)),
                h('td', { class: 'r num' }, `${d.ghi.toFixed(1)} kWh/m²`),
              ),
            ),
        ),
      );
      const hs = r.hours;
      tempChart.update({ marker: Math.floor(store.now()), data: [hs.map((w) => w.ts), hs.map((w) => tempVal(w.temp_c)), hs.map((w) => w.cloud), hs.map((w) => w.precip_prob)] });
      irrChart.update({ marker: Math.floor(store.now()), data: [hs.map((w) => w.ts - 1800), hs.map((w) => w.ghi), hs.map((w) => w.dni), hs.map((w) => w.dhi)] });
      const next = hs.filter((w) => w.ts > store.now() && w.ts < store.now() + 86400);
      const aqi = next.map((w) => w.aqi).filter((x): x is number => x !== null);
      const pm = next.map((w) => w.pm25).filter((x): x is number => x !== null);
      const aod = next.map((w) => w.aod).filter((x): x is number => x !== null);
      replace(
        aqEl,
        aqi.length || pm.length
          ? h(
              'div',
              null,
              h(
                'div',
                { class: 'stats' },
                stat('US AQI (max 24 h)', aqi.length ? Math.max(...aqi).toFixed(0) : '—', 'US Air Quality Index forecast'),
                stat('PM2.5 (max 24 h)', pm.length ? `${Math.max(...pm).toFixed(0)} µg/m³` : '—', 'Fine particles; wildfire smoke raises this'),
                stat('Aerosol depth (max)', aod.length ? Math.max(...aod).toFixed(2) : '—', 'Aerosol optical depth; high values dim sunlight'),
              ),
              h('p', { class: 'small muted' }, 'Smoke and haze reduce solar output; the solar model does not yet adjust for aerosols.'),
            )
          : h('div', { class: 'muted small' }, 'Air-quality data not available (enable it in Settings → Weather).'),
      );
    }
  };
  const loadYear = async () => {
    try {
      const y = await get<SunYear>('/sun');
      yearChart.update({ marker: Math.floor(store.now()), data: [y.year.map((d) => d.noon), y.year.map((d) => localHour(d.sunrise)), y.year.map((d) => localHour(d.noon)), y.year.map((d) => localHour(d.sunset))] });
    } catch {
      /* location not set */
    }
  };
  const stops = [every(300000, () => load().catch((e) => replace(statusEl, note('err', errText(e))))), every(3600000, loadYear)];
  return () => {
    stops.forEach((f) => f());
    charts.destroy();
  };
}
