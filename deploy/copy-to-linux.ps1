<#
  Copies your RustSolar config (and token, and optionally the history database) from this
  Windows PC to a Linux server where the rustsolar .deb is installed.

  Usage (PowerShell):
      .\deploy\copy-to-linux.ps1 -Server user@192.168.1.20
      .\deploy\copy-to-linux.ps1 -Server user@myserver -IncludeDatabase   # also move history

  Needs the OpenSSH client (built into Windows 11: ssh / scp) and sudo on the server.
  Stop RustSolar on Windows first when using -IncludeDatabase, and afterwards keep only one
  copy running so the gateway is not polled twice.
#>
param(
    [Parameter(Mandatory = $true)][string]$Server,
    [string]$DataDir = "$env:LOCALAPPDATA\RustSolar",
    [switch]$IncludeDatabase
)
$ErrorActionPreference = 'Stop'
$files = @("$DataDir\config.toml")
if (Test-Path "$DataDir\gateway.token") { $files += "$DataDir\gateway.token" } else { Write-Warning "No gateway.token found; set it on the server with 'rustsolar set-token'." }
if ($IncludeDatabase) {
    if (Get-Process rustsolar -ErrorAction SilentlyContinue) { throw "RustSolar is running on this PC. Close it first so the database is complete." }
    $files += "$DataDir\rustsolar.db"
    foreach ($x in 'rustsolar.db-wal', 'rustsolar.db-shm') { if (Test-Path "$DataDir\$x") { $files += "$DataDir\$x" } }
}
foreach ($f in $files) { if (-not (Test-Path $f)) { throw "Missing: $f" } }

Write-Host "Copying $($files.Count) file(s) to ${Server}:~/rustsolar-import/ ..."
ssh $Server "mkdir -p ~/rustsolar-import && chmod 700 ~/rustsolar-import"
scp @files "${Server}:~/rustsolar-import/"

$remote = @'
set -e
sudo systemctl stop rustsolar 2>/dev/null || true
cd ~/rustsolar-import
for f in *; do sudo install -o rustsolar -g rustsolar -m 600 "$f" /var/lib/rustsolar/"$f"; done
cd ~ && rm -rf ~/rustsolar-import
sudo -u rustsolar RUSTSOLAR_DATA_DIR=/var/lib/rustsolar rustsolar check-config || true
sudo systemctl start rustsolar
sudo systemctl --no-pager status rustsolar | head -5
'@
ssh -t $Server $remote
Write-Host "Done. If you want to open the dashboard from other devices, edit [server] bind in /var/lib/rustsolar/config.toml (see README)."
