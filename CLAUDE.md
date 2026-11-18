# RustSolar — project rules for Claude Code

Read `Enphase_Energy_Intelligence_Claude_Prompt.md` (the original brief) and `PROGRESS.md`
first, when present. Log every session in PROGRESS.md. Both are the owner's local working files:
they are git-ignored and must never be committed (they hold personal details).

- RocketSalvo project (owner: Geoffrey Huber), licensed under PolyForm Noncommercial 1.0.0
  (LICENSE.md; the About section says so): free for personal and other noncommercial use;
  commercial use, including distribution by solar installers, needs a license from the owner.
  Never publish crates/packages.
- The GitHub repository (Bullpuph123/RustSolar) is meant to be public. Never commit personal
  data: real coordinates, addresses, gateway serials, MAC addresses, certificate fingerprints,
  email addresses, tokens or the owner's config. Use placeholders (192.168.1.x, `<serial>`) in
  docs, examples and tests, and commit with the GitHub no-reply email.
- **Read-only with respect to the Enphase system.** Only HTTP GET to the gateway. Never add
  code that changes battery, grid, relay, charging or streaming settings, and never enable
  `/ivp/livedata/stream` unless the owner asks.
- Never sign up for, enable or call paid services. External services allowed today: the
  Open-Meteo free forecast and air-quality APIs (see docs/EXTERNAL-SERVICES.md). Check terms
  before adding any other provider.
- Never commit or log the gateway token. It lives in `<data-dir>/gateway.token` or
  `RUSTSOLAR_GATEWAY_TOKEN`; the browser never receives it.
- Never fabricate telemetry or weather. Synthetic data only in tests and `serve --demo`
  (separate `demo.db`, banner on every page).
- Don't guess hardware specs, coordinates or tariff rates: the owner enters them. Unsourced
  defaults are labelled "default estimate".
- Branding: colours/fonts from `brand/tokens.css` (copied from RustFlight/RustStand). Night
  sidebar with the Rocket Salvo logo and "Rust<em>Solar</em>"; "ROCKET<em>SALVO</em>" wordmark.
- UI rules (from RustFlight/RustCase): no window.alert/confirm/prompt; tooltip on every
  control; one primary button per page; no nested scroll boxes; validate every typed value
  on the server.
- Versioning: one version in `[workspace.package]` of Cargo.toml; bump on every change
  request (minor for features, patch for fixes).
- Release builds: Windows exe, plus Linux .deb packages (amd64/arm64) since the owner asked on
  2026-10-25 (see docs/DEPLOYMENT.md "Debian/Ubuntu package"). Older note: Linux/Pi builds
  are done natively on the target (docs/DEPLOYMENT.md).
- Before every commit: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`, and in `web/`: `npm run check` and `npm run build` (web/dist is
  embedded at compile time and checked in).
