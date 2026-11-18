// Built-in help: a side panel with help for the current page, getting started, and a glossary.
// Open with the "? Help" button, F1 or "?"; close with Esc.

import { h, replace } from './dom';

interface PageHelp {
  title: string;
  intro: string;
  sections: [string, string][];
}

const MEASURED =
  'Numbers without an "Estimate" tag are measured by your Enphase gateway. Anything tagged "Estimate" (forecasts, simulations, costs) is calculated by RustSolar and can be wrong.';

const PAGES: Record<string, PageHelp> = {
  overview: {
    title: 'Overview',
    intro: 'What your system is doing right now and how today is going.',
    sections: [
      ['Live tiles', 'Solar, Home, Battery, Grid and Charge, read from the gateway every 12 seconds. Battery shows charging or discharging; Grid shows importing (buying) or exporting (sending power to PG&E). Tiles turn grey when the data is stale.'],
      ['Energy flow', 'The animated diagram shows where power is going right now. It is drawn only from measured values: solar goes to the home first, then the battery, then the grid.'],
      ['Today', 'Energy (kWh) since local midnight, added up from the 12-second readings. Self-consumption is the share of your solar used at home or stored in the battery instead of exported. Grid independence is the share of home use not bought from the grid.'],
      ['Sun', 'Sunrise, solar noon and sunset for your location, calculated locally. The bar shows night, twilight and daylight with a marker for now.'],
      ['Recommendations and alerts', 'Short suggestions (for example "sunny tomorrow, run heavy loads midday") and anything that needs attention. Each one explains why on the Recommendations page.'],
      ['Last 24 hours', 'Solar, home, battery and grid power over the past day. Hover the chart to read exact values.'],
    ],
  },
  solar: {
    title: 'Solar',
    intro: 'How much your panels produce compared with the forecast.',
    sections: [
      ['Production chart', 'Measured solar power today against the forecast for each hour. The shaded band is the likely range: real output falls inside it about 8 days in 10.'],
      ['Production totals', 'Energy produced today, this week, this month and over the system lifetime (lifetime comes from the gateway itself).'],
      ['Sun path', 'The sun’s height above the horizon through the day. Panels produce most when the sun is high and the sky is clear.'],
      ['Why production can be low', 'Clouds, smoke, haze, dirt on panels, shade, heat (panels lose about 0.3–0.4% of output per °C above 25 °C), and the microinverters’ maximum output (clipping) on very bright days.'],
    ],
  },
  battery: {
    title: 'Batteries',
    intro: 'Your IQ Battery charge level, what it will do next, and how long it would last in a power cut.',
    sections: [
      ['State of charge', 'How full the battery is (SOC, %). The backup reserve is the level Enphase keeps for outages; in normal operation the battery stops discharging there.'],
      ['Outlook', 'A simulation using the solar and household forecasts: when the battery should be full, what charge it will have at sunset and sunrise, and when it would reach the reserve. Estimates only.'],
      ['Outage planning', 'How many hours the stored energy would run the house during a grid outage at different usage levels, down to the outage floor set in Settings. Solar can recharge the battery during an outage in daylight, which is not included in the simple runtime figures.'],
      ['Battery health (calculated)', 'Charged and discharged energy, equivalent full cycles, and the round-trip efficiency seen over recent days. Implied capacity is stored energy ÷ SOC, which tracks the usable capacity the gateway reports over time.'],
      ['Read-only', 'RustSolar never changes battery settings. Change the reserve or mode in the Enphase app.'],
    ],
  },
  grid: {
    title: 'Grid & Household',
    intro: 'What you buy from PG&E, what you send back, and how much the house uses.',
    sections: [
      ['Right now', 'Current import or export and household use.'],
      ['Today', 'Imported and exported energy and where the home’s energy came from: solar, battery or grid.'],
      ['Daily grid energy', 'Import and export for each of the last 30 days.'],
      ['Household forecast', 'The expected household use is learned from your own history by hour of the day and day of the week, adjusted for temperature once enough data exists.'],
    ],
  },
  forecast: {
    title: 'Forecast',
    intro: 'Solar and household forecasts for today and the next days, and how accurate they have been.',
    sections: [
      ['How the solar forecast works', 'Open-Meteo’s hourly sunlight forecast is converted into the sunlight hitting each of your arrays (using tilt and direction), then into power using panel temperature and size. A learned correction then adjusts for what your system really produces under clear, partly cloudy and cloudy skies.'],
      ['Confidence band', 'The shaded range around the forecast. It starts as a generic default and narrows as RustSolar learns from your own history (after about 30 days in each sky type).'],
      ['Forecast accuracy', 'Compares past forecasts with what was measured. Average error is the typical miss per hour; bias shows whether the forecast is usually too high (+) or too low (−). Only forecasts made before each hour began are used, so the score is fair.'],
      ['Reset model', 'Settings → Data has a button to forget the learned correction, for example after adding panels or removing shade.'],
    ],
  },
  weather: {
    title: 'Weather',
    intro: 'Local weather from Open-Meteo (free service, refreshed about hourly), plus air quality and smoke.',
    sections: [
      ['Current conditions and outlook', 'Temperature, cloud cover, wind and rain. Change °F/°C in the sidebar (Units).'],
      ['Solar irradiance', 'Sunlight power in W/m²: GHI is all sunlight on flat ground, DNI is direct sunbeam, DHI is light scattered by the sky. Your panels see a mix depending on their angle.'],
      ['Air quality and smoke', 'US AQI, fine particles (PM2.5) and aerosol depth. Wildfire smoke can cut solar output noticeably even on cloudless days.'],
      ['Sunrise and sunset through the year', 'Day length for each day of the year at your location.'],
    ],
  },
  financial: {
    title: 'Financial',
    intro: 'Estimated costs and savings using the rates you entered. Your PG&E bill is authoritative.',
    sections: [
      ['Totals', 'Net cost = grid purchases + fixed charges + demand charges − export credits − baseline credits. "Without solar/battery" estimates what you would pay if every kWh the house used came from the grid at that hour’s price; the difference is the estimated benefit.'],
      ['Export credit', 'Under Net Billing (NEM 3) exported energy earns an hourly price set by PG&E for your application year. Choose "PG&E Net Billing" and your year in Settings → Electricity rates; RustSolar has PG&E’s official price tables built in. With no export rule, exports count as $0.'],
      ['Rates today', 'Your time-of-use price for each hour today. Hover an hour to see its export price.'],
      ['Reconciling with your bill', 'Download the cost CSV for hourly lines. Bills differ slightly because of taxes, true-up timing, rounding and the meter’s own readings.'],
    ],
  },
  recommendations: {
    title: 'Recommendations',
    intro: 'Suggestions and alerts, each with the reasoning and data behind it.',
    sections: [
      ['Recommendations', 'Advice based on the forecasts and your rates, such as when to run big loads. Each shows its confidence and the time window it applies to. RustSolar never acts on these by itself.'],
      ['Alerts', 'Problems such as the gateway being offline, the token expiring, low battery overnight or production well below forecast. Acknowledge an alert to stop it showing; it returns if the problem comes back later.'],
      ['Alert history', 'Every alert that was raised and when it cleared.'],
    ],
  },
  history: {
    title: 'History',
    intro: 'Charts and downloads for any date range.',
    sections: [
      ['Power', 'Detailed power readings (5-minute averages for longer ranges).'],
      ['Daily totals', 'Energy per day: solar, home, import, export, battery charge and discharge.'],
      ['Exports', 'Download CSV or JSON for spreadsheets or your own analysis.'],
    ],
  },
  settings: {
    title: 'Settings',
    intro: 'Setup steps in order, then configuration, backups and diagnostics.',
    sections: [
      ['1–3 Gateway', 'The IQ Gateway address on your network, its pinned certificate (proves RustSolar is talking to your gateway) and the access token from entrez.enphaseenergy.com. The token stays on the server and is never sent to the browser.'],
      ['4 Location', 'Latitude, longitude and time zone. In California longitude is negative (for example −122.42). RustSolar warns when a sign looks wrong for the time zone.'],
      ['5 Solar arrays', 'For each group of panels facing the same way: total panel watts (DC), microinverter limit (AC), tilt (0° flat, 90° vertical) and direction (180° south, 270° west). Tilt and direction come from your installer’s documents or a compass/phone app.'],
      ['6 Battery', 'Leave blank to use what the gateway reports. Outage floor is how low you would let the battery go in a power cut.'],
      ['7 Electricity rates', 'Export credits: pick PG&E Net Billing, your application year (vintage), whether PG&E supplies your generation, and ACC Plus. The heat map shows the export price for every month and hour. Import prices and the fixed charge are entered as JSON below it.'],
      ['9 Dashboard access', 'Listen address, password, and the easy address: with "Announce on the network" on, open http://rustsolar.local from any device at home (add "Also use port 80" to leave off the :port). It works with public DNS because devices find it directly on the local network.'],
      ['Diagnostics', 'Polling health, errors, clock difference between the gateway and this computer, database size and Open-Meteo usage.'],
    ],
  },
};

const START: [string, string][] = [
  ['Measured vs estimated', MEASURED],
  ['First-time setup', 'Open Settings and work through steps 1–7 in order. Data collection starts as soon as the gateway address, certificate and token are set; forecasts need the location and arrays; cost estimates need your rates.'],
  ['Learning', 'Forecasts improve over the first few weeks as RustSolar compares its predictions with what your system really did.'],
  ['Keyboard', 'F1 or ? opens this help, Esc closes it.'],
];

const GLOSSARY: [string, string][] = [
  ['kW (kilowatt)', 'Power: how fast energy flows right now. 1 kW = 1,000 W.'],
  ['kWh (kilowatt-hour)', 'Energy: power over time. 1 kW for one hour = 1 kWh. Your bill charges per kWh.'],
  ['Solar production', 'Power from your panels after the microinverters (AC).'],
  ['Home use (load)', 'Everything the house consumes, whatever the source.'],
  ['Import', 'Power bought from the grid.'],
  ['Export', 'Surplus solar sent to the grid.'],
  ['SOC (state of charge)', 'How full the battery is, as a percentage of usable capacity.'],
  ['Backup reserve', 'Battery charge kept for outages. Set in the Enphase app; RustSolar only reads it.'],
  ['Usable capacity', 'Energy the battery can deliver from full to empty (about 5 kWh per IQ Battery 5P).'],
  ['Outage floor', 'The lowest charge you would allow during a power cut; used for runtime estimates.'],
  ['Equivalent cycles', 'Total energy discharged ÷ usable capacity. Two half-discharges = one cycle.'],
  ['Round-trip efficiency', 'Energy you get back out of the battery ÷ energy put in. Typically about 90%.'],
  ['Implied capacity', 'Stored energy reported by the gateway ÷ SOC; shows the battery’s real capacity over time.'],
  ['Self-consumption', 'Share of solar energy used at home or stored, rather than exported.'],
  ['Grid independence', 'Share of home use not bought from the grid.'],
  ['Microinverter', 'A small inverter under each panel that converts its DC output to AC. Its AC limit caps output on bright days (clipping).'],
  ['Clipping', 'When panels could produce more than the microinverters can deliver, output is capped at the AC limit.'],
  ['IQ Gateway', 'The Enphase box that monitors and controls your system. RustSolar reads it over your home network, read-only.'],
  ['Token', 'The access key from Enphase that lets RustSolar read the gateway. Lasts about a year for owners.'],
  ['Certificate pin', 'A fingerprint of the gateway’s security certificate, so RustSolar only talks to your gateway.'],
  ['Stale data', 'No new reading for a while; the values shown are the last ones received. The warning waits 5 minutes by default (Settings → Weather and collection) so a single missed reading stays quiet.'],
  ['Time-of-use (TOU)', 'Prices that change by hour. On PG&E E-ELEC, peak is 4–9 pm, part-peak 3–4 pm and 9 pm–midnight, off-peak the rest.'],
  ['Seasons', 'PG&E summer is June–September with higher peak prices; winter is October–May.'],
  ['Base Services Charge', 'PG&E’s fixed daily charge, paid regardless of usage.'],
  ['Baseline allowance', 'An amount of energy at a lower price on tiered plans. E-ELEC has no baseline tiers.'],
  ['Medical Baseline', 'A PG&E programme for households with medical needs: extra lower-priced energy (on tiered plans) or a discount, plus extra notice before planned power shutoffs.'],
  ['Net Billing (NBT, "NEM 3")', 'California solar billing since April 2023. Imports are billed at your rate plan’s price; exports earn an hourly price (much lower most of the day, higher on summer evenings) locked for 9 years from your interconnection year.'],
  ['Export credit', 'What PG&E pays for exported energy. Credits reduce energy charges, not the fixed charge; unused credit carries forward.'],
  ['Vintage (application year)', 'The year your complete solar interconnection application was submitted. It fixes which Net Billing export price table applies to you for 9 years.'],
  ['ACC Plus', 'A bonus export credit for most residential Net Billing systems ($0.022/kWh for 2023 applications, falling 20% of that each year), fixed for 9 years from PTO.'],
  ['PTO (permission to operate)', 'The date PG&E allowed your system to switch on.'],
  ['CCA', 'Community choice aggregator: a local agency that supplies your generation instead of your utility (for example, a county or city energy agency). The utility still delivers the power.'],
  ['True-up', 'The yearly settlement of your solar billing account with PG&E.'],
  ['Irradiance (W/m²)', 'Sunlight power per square metre. Bright midday sun is about 1,000 W/m².'],
  ['GHI / DNI / DHI', 'Global horizontal (all light on flat ground), direct normal (direct sunbeam), diffuse horizontal (light from the sky).'],
  ['POA', 'Plane of array: sunlight landing on the tilted panel surface.'],
  ['Learned correction', 'An adjustment RustSolar learns by comparing forecasts with real production, separately for clear, partly cloudy and cloudy skies and sun height.'],
  ['Confidence band', 'The range real output should fall in about 80% of the time.'],
  ['Bias', 'Average forecast minus measured. Positive means the forecast tends to be too high.'],
  ['AQI / PM2.5', 'US Air Quality Index and fine particle concentration. High values often mean smoke, which reduces solar output.'],
  ['Aerosol optical depth', 'How much haze or smoke dims sunlight. Above about 0.3 is noticeably hazy.'],
  ['Civil twilight', 'The time before sunrise and after sunset when the sky is still light.'],
  ['rustsolar.local (mDNS)', 'Multicast DNS: devices on your home network ask each other for names ending in .local, so no DNS server is needed.'],
  ['Demo mode', 'Synthetic data for trying the app, in a separate database, with a banner on every page.'],
];

let panel: HTMLElement | null = null;
let currentPage = () => 'overview';

export function initHelp(getPage: () => string) {
  currentPage = getPage;
  document.addEventListener('keydown', (e) => {
    const t = e.target as HTMLElement;
    const typing = t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.tagName === 'SELECT' || t.isContentEditable);
    if (e.key === 'F1' || (e.key === '?' && !typing)) {
      e.preventDefault();
      toggleHelp();
    } else if (e.key === 'Escape' && panel && !panel.hidden) {
      closeHelp();
    }
  });
}

export function helpButton(): HTMLElement {
  return h('button', { class: 'ghost help-btn', title: 'Help for this page and a glossary of terms (F1)', 'aria-label': 'Help', onclick: () => toggleHelp() }, '? Help');
}

export function toggleHelp() {
  if (panel && !panel.hidden) closeHelp();
  else openHelp();
}

export function closeHelp() {
  if (panel) panel.hidden = true;
  document.body.classList.remove('help-open');
}

/** Re-render for the current page if the panel is open (called on navigation). */
export function refreshHelp() {
  if (panel && !panel.hidden) render('');
}

export function openHelp(term?: string) {
  if (!panel) {
    panel = h('aside', { class: 'help-panel', role: 'dialog', 'aria-label': 'Help' });
    document.body.appendChild(panel);
  }
  panel.hidden = false;
  document.body.classList.add('help-open');
  render(term ?? '');
}

function section(title: string, ...body: (HTMLElement | null)[]): HTMLElement {
  return h('section', { class: 'help-sec' }, h('h3', null, title), ...body);
}

function render(query: string) {
  if (!panel) return;
  const page = PAGES[currentPage()] ?? PAGES.overview;
  const search = h('input', { type: 'search', placeholder: 'Search help', value: query, title: 'Filter the help and glossary', 'aria-label': 'Search help' });
  const results = h('div', { class: 'help-body' });
  const fill = () => {
    const q = search.value.trim().toLowerCase();
    const match = (a: string, b: string) => !q || a.toLowerCase().includes(q) || b.toLowerCase().includes(q);
    const out: HTMLElement[] = [];
    if (!q) {
      out.push(section(`This page: ${page.title}`, h('p', { class: 'help-intro' }, page.intro), h('dl', null, ...page.sections.flatMap(([t, d]) => [h('dt', null, t), h('dd', null, d)]))));
      out.push(section('Getting started', h('dl', null, ...START.flatMap(([t, d]) => [h('dt', null, t), h('dd', null, d)]))));
    } else {
      const hits: HTMLElement[] = [];
      for (const p of Object.values(PAGES)) for (const [t, d] of p.sections) if (match(t, d)) hits.push(h('dt', null, `${p.title} · ${t}`), h('dd', null, d));
      for (const [t, d] of START) if (match(t, d)) hits.push(h('dt', null, t), h('dd', null, d));
      if (hits.length) out.push(section('Pages', h('dl', null, ...hits)));
    }
    const terms = GLOSSARY.filter(([t, d]) => match(t, d));
    if (terms.length) out.push(section('Glossary', h('dl', { class: 'glossary' }, ...terms.flatMap(([t, d]) => [h('dt', null, t), h('dd', null, d)]))));
    if (!out.length) out.push(h('p', { class: 'muted' }, 'Nothing matches that search.'));
    replace(results, ...out);
  };
  search.addEventListener('input', fill);
  replace(
    panel,
    h('header', { class: 'help-head' }, h('h2', null, 'Help'), h('div', { class: 'spacer' }), h('button', { class: 'ghost', title: 'Close help (Esc)', 'aria-label': 'Close help', onclick: closeHelp }, '✕')),
    search,
    results,
  );
  fill();
  if (query) search.focus();
}
