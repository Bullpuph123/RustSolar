# External services and costs

RustSolar has **no mandatory recurring cost** and needs no accounts with a credit card.

| Service | Used for | Cost / terms (checked 2026-10-25) | Our usage | Data sent |
|---|---|---|---|---|
| Enphase IQ Gateway (local, your LAN) | Live telemetry, inventory | Free; local API with a homeowner token | GET every 12 s + 3 GETs/hour | Bearer token (to the gateway only) |
| Open-Meteo Forecast API `api.open-meteo.com/v1/forecast` | Hourly weather & irradiance | Free for **non-commercial** use, no key. Limits: 600/min, 5,000/h, 10,000/day, 300,000/month. Data licence **CC BY 4.0** (attribution shown in the UI). <https://open-meteo.com/en/terms> | 1 call per 60 min (+1 retry per 10 min when failing) | Latitude/longitude rounded to 0.01° |
| Open-Meteo Air Quality API `air-quality-api.open-meteo.com` | PM2.5, US AQI, aerosol optical depth (smoke) | Same terms | 1 call per 60 min | Same |
| entrez.enphaseenergy.com | Creating the gateway token (manual, by you) | Free with your Enlighten login | Not called by RustSolar | — |

Safeguards in code:

- A daily request budget (`weather.daily_request_budget`, default 200, validated ≤ 5,000)
  counted in the database; when reached, cached forecasts are used.
- HTTP 429 is handled and `Retry-After` honoured; failures retry at most every 10 minutes.
- Only the free endpoints are referenced; there is no code path to paid "customer-" endpoints.
- Forecasts are cached in SQLite, so the dashboard keeps working during internet outages.

Not used: Enphase cloud developer API (paid), any cloud AI service, mapping services,
analytics, CDNs or web fonts. Optional webhook notifications go only to a URL you configure.

Not enabled (not verified): NOAA/NWS API (free, US-only, would need its own provider
implementation and User-Agent contact string) and Solcast/Forecast.Solar (free tiers have
small quotas and terms that change). They can be added behind the provider abstraction after
their current terms are checked.

Built-in reference data (no network calls at runtime): PG&E Net Billing export credit values
from PG&E's published files (<https://www.pge.com/eecvalues>, files dated 2024-12-18; calendar
year 2026 checked against the June 2026 price sheets at <https://www.pge.com/solarbilling>) and
the ACC Plus amounts from Schedule NBT. Regenerate with `tools/gen_pge_nbt.py` when PG&E
publishes new files.
