# Deployment

RustSolar is one executable plus a data directory. It is light (one HTTPS request every
12 s, SQLite writes in small transactions) and runs comfortably on a Raspberry Pi 5.

## Windows 11

```powershell
cargo build --release
New-Item -ItemType Directory -Force "C:\Program Files\RustSolar"
Copy-Item target\release\rustsolar.exe "C:\Program Files\RustSolar\"
& "C:\Program Files\RustSolar\rustsolar.exe" pin-cert --host 192.168.1.50 --save
& "C:\Program Files\RustSolar\rustsolar.exe" set-token
# Start automatically at boot (elevated PowerShell):
.\deploy\install-windows-task.ps1 -Exe "C:\Program Files\RustSolar\rustsolar.exe"
```

The scheduled task restarts RustSolar if it exits and runs without a signed-in user.
Logs: `%LOCALAPPDATA%\RustSolar\logs`. To stop: `Stop-ScheduledTask RustSolar`.

To use the dashboard from other devices, set in `config.toml`:

```toml
[server]
bind = "0.0.0.0:8080"
password_hash = "$argon2id$..."   # from: rustsolar hash-password
```

and allow TCP 8080 on the **Private** network profile only:
`New-NetFirewallRule -DisplayName RustSolar -Direction Inbound -Protocol TCP -LocalPort 8080 -Profile Private -Action Allow`

## Linux x86-64 and Raspberry Pi 5 (64-bit Raspberry Pi OS)

Build on the Pi itself (simplest; ~10 minutes the first time) or on any Linux machine of
the same architecture:

```bash
sudo apt install -y build-essential pkg-config git
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh     # Rust stable
git clone https://github.com/Bullpuph123/RustSolar.git rustsolar && cd rustsolar
cargo build --release          # web UI is pre-built in web/dist; Node is not needed
sudo install -m 0755 target/release/rustsolar /usr/local/bin/rustsolar
sudo useradd --system --home /var/lib/rustsolar --shell /usr/sbin/nologin rustsolar
sudo install -d -o rustsolar -g rustsolar -m 0750 /var/lib/rustsolar
sudo -u rustsolar RUSTSOLAR_DATA_DIR=/var/lib/rustsolar rustsolar pin-cert --host 192.168.1.50 --save
sudo -u rustsolar RUSTSOLAR_DATA_DIR=/var/lib/rustsolar rustsolar set-token
sudo install -m 0644 deploy/rustsolar.service /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl enable --now rustsolar
journalctl -u rustsolar -f
```

The unit is hardened (read-only system, private /tmp, only `/var/lib/rustsolar` writable,
umask 077 so the token file stays private). It starts after the network and time sync and
restarts automatically, including after power cuts.

**Clock:** energy integration uses gateway timestamps, but forecasts, day boundaries and
backups use the host clock. The Pi has no battery-backed clock, so keep `systemd-timesyncd`
enabled. The diagnostics page shows gateway-vs-host clock skew.

**SD cards:** WAL mode and small transactions keep writes modest (~20 MB/day of raw samples
before retention). For years of unattended use prefer an SSD or a high-endurance card, and
keep `backup.dir` on a different disk or a network share.

## Backups

- Automatic daily backup at `backup.hour` (local time) into `<data-dir>/backups`, keeping the
  newest `backup.keep` (default 14). Uses SQLite `VACUUM INTO`, which is consistent while
  running.
- Manual: Settings → "Back up now", or `rustsolar backup --out FILE`.
- Restore: stop the service, then `rustsolar restore FILE --force` (the current database is
  kept as `rustsolar.before-restore-*.db`).
- Also back up `config.toml`. The token file can be regenerated at entrez.enphaseenergy.com.

## Updating

Debian/Ubuntu: `sudo apt install ./rustsolar_<version>-1_amd64.deb` (config, token and history
are kept; the service restarts). Otherwise stop the service, replace the executable, start it. Database migrations run automatically
at start-up and are transactional.

## Debian/Ubuntu package (.deb)

Install on the server (x86-64: `amd64`; Raspberry Pi / ARM: `arm64`):

```bash
sudo apt install ./rustsolar_0.6.0-1_amd64.deb
```

This installs `/usr/bin/rustsolar`, a hardened systemd service (enabled; started once
`/var/lib/rustsolar/config.toml` exists), and a
`rustsolar` system user. Data lives in `/var/lib/rustsolar` (config.toml, gateway.token,
database, logs, backups). Removing or purging the package keeps that data.

Move your Windows setup to the server (stop RustSolar on Windows first):

```powershell
.\deploy\copy-to-linux.ps1 -Server you@server-ip                 # config + token
.\deploy\copy-to-linux.ps1 -Server you@server-ip -IncludeDatabase  # also history
```

or by hand: copy `%LOCALAPPDATA%\RustSolar\config.toml` and `gateway.token` to
`/var/lib/rustsolar/`, then `sudo chown -R rustsolar:rustsolar /var/lib/rustsolar &&
sudo chmod 600 /var/lib/rustsolar/gateway.token && sudo systemctl restart rustsolar`.

**Easy address:** RustSolar announces itself on the home network (mDNS), so other devices can
open `http://rustsolar.local:<port>`; with `[server] port80 = true` it also listens on port 80
and the address is just `http://rustsolar.local`. This needs no DNS server (works with public
DNS). The systemd unit grants only `CAP_NET_BIND_SERVICE` for port 80. With ufw:
`sudo ufw allow from 192.168.1.0/24 to any port 80 proto tcp` (mDNS on UDP 5353 is allowed
by Ubuntu's default ufw rules). Change the name with `mdns_name`, or turn it off with
`mdns = false`.

**Sharing port 80 with another dashboard:** RustSolar can relay requests for another host
name to a dashboard on a different port of the same machine, so that one needs no port either:

```toml
[[server.forward]]
host = "rustsprinkler.local"   # the name the browser uses
to = "127.0.0.1:8086"          # where that dashboard listens
```

Requests whose Host header is `rustsprinkler.local` are passed through unchanged (including
live event streams); everything else is RustSolar. The other program must announce or
otherwise resolve its own name to this machine (RustSprinkler does, over mDNS). Forwarded
requests do not go through RustSolar's login; the other dashboard applies its own rules.
Takes effect when saved, no restart needed.

To reach the dashboard from other devices set `[server] bind = "0.0.0.0:8080"` and a
password: `rustsolar hash-password` → `password_hash = "..."` (the service refuses a network
address without one unless `allow_unauthenticated_lan = true`).

Building the packages on Windows (needs `cargo-zigbuild`, `cargo-deb` and zig — `pip install ziglang`):

```powershell
$env:CARGO_ZIGBUILD_ZIG_PATH = "<python>\Lib\site-packages\ziglang\zig.exe"
rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu
cargo zigbuild --release -p rustsolar --target x86_64-unknown-linux-gnu.2.28
cargo zigbuild --release -p rustsolar --target aarch64-unknown-linux-gnu.2.28
cargo deb -p rustsolar --no-build --no-strip --target x86_64-unknown-linux-gnu
cargo deb -p rustsolar --no-build --no-strip --target aarch64-unknown-linux-gnu
```

Packages land in `target/debian/`. Binaries need glibc ≥ 2.28 (Debian 10+, Ubuntu 18.10+,
Raspberry Pi OS Buster+). The amd64 package was tested on Ubuntu 24.04 (install, service start,
live gateway readings, purge).
