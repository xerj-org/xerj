# =============================================================================
# xerj installer for Windows
#   powershell -ExecutionPolicy Bypass -c "irm https://xerj.org/get.ps1 | iex"
# =============================================================================
# Detects your CPU (x64 / ARM64), downloads the matching release from
# github.com/xerj-org/xerj/releases, verifies its SHA-256, and installs a
# single xerj.exe. No JVM, no dependencies - one static binary.
#
# Environment overrides:
#   XERJ_VERSION=v1.0.0     install a specific tag (default: latest release)
#   XERJ_INSTALL_DIR=path   install location (default: %LOCALAPPDATA%\Programs\xerj)
#   XERJ_REPO=owner/name    source repo (default: xerj-org/xerj)
#   XERJ_NO_PATH=1          do not add the install dir to the user PATH
#
# Works on Windows PowerShell 5.1 and PowerShell 7+.
#
# The whole body runs inside an anonymous script block so that, when piped
# into `iex` from an interactive session, our preference variables, helper
# functions and locals do not leak into (or rewire) the caller's shell.
# =============================================================================
& {
  $ErrorActionPreference = 'Stop'
  # PS 5.1: the default progress bar slows Invoke-WebRequest downloads by
  # 10-100x (redrawn per buffer read). Scoped to this block, so the caller's
  # setting is untouched.
  $ProgressPreference = 'SilentlyContinue'

  function Write-Step($msg)  { Write-Host "==> $msg" -ForegroundColor Yellow }
  function Write-Okay($msg)  { Write-Host " ok  $msg" -ForegroundColor Green }
  # throw, not exit: when the script is piped into `iex` from an interactive
  # shell, `exit` would close the user's whole terminal session.
  function Fail($msg)        { Write-Host "error: $msg" -ForegroundColor Red; throw "xerj install failed" }

  $Repo = if ($env:XERJ_REPO) { $env:XERJ_REPO } else { 'xerj-org/xerj' }

  # TLS 1.2 for Windows PowerShell 5.1 (7+ already defaults to it).
  try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
  } catch {}

  # --- detect CPU ------------------------------------------------------------
  # Machine scope reflects the real hardware even when this PowerShell runs
  # under emulation (x64 PS7 on Windows-on-ARM reports AMD64 in process scope).
  # PROCESSOR_ARCHITEW6432 covers 32-bit PowerShell on a 64-bit OS.
  $rawArch = [Environment]::GetEnvironmentVariable('PROCESSOR_ARCHITECTURE', 'Machine')
  if (-not $rawArch) {
    $rawArch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
  }
  switch ($rawArch) {
    'AMD64' { $cpu = 'x86_64' }
    'ARM64' { $cpu = 'aarch64' }
    default { Fail "unsupported CPU '$rawArch' - xerj ships Windows builds for x64 (AMD64) and ARM64" }
  }
  $target = "$cpu-pc-windows-msvc"

  # --- resolve version -------------------------------------------------------
  if ($env:XERJ_VERSION) {
    $tag = $env:XERJ_VERSION
    if ($tag -notmatch '^v') { $tag = "v$tag" }  # release tags are v-prefixed
  } else {
    Write-Step "resolving latest release of $Repo..."
    try {
      $rel = Invoke-RestMethod -UseBasicParsing -Uri "https://api.github.com/repos/$Repo/releases/latest"
      $tag = $rel.tag_name
    } catch {
      Fail "could not resolve the latest release ($($_.Exception.Message)) - set `$env:XERJ_VERSION='vX.Y.Z' (see https://github.com/$Repo/releases)"
    }
    if (-not $tag) { Fail "could not resolve the latest release - set `$env:XERJ_VERSION='vX.Y.Z'" }
  }
  $ver   = $tag.TrimStart('v')
  $stage = "xerj-$ver-$target"
  $asset = "$stage.zip"
  $base  = "https://github.com/$Repo/releases/download/$tag"
  Write-Step "installing xerj $tag for $target"

  # --- download ----------------------------------------------------------------
  $tmp = Join-Path ([IO.Path]::GetTempPath()) ("xerj-install-" + [IO.Path]::GetRandomFileName())
  New-Item -ItemType Directory -Force -Path $tmp | Out-Null
  try {
    Write-Step "downloading $asset..."
    try {
      Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset" -OutFile (Join-Path $tmp $asset)
    } catch {
      Fail "download failed for $target ($($_.Exception.Message)). Available assets: https://github.com/$Repo/releases/tag/$tag"
    }

    # --- verify checksum (fail closed - every release publishes the .sha256) --
    try {
      Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset.sha256" -OutFile (Join-Path $tmp "$asset.sha256")
    } catch {
      Fail "could not download checksum $asset.sha256 ($($_.Exception.Message)) - refusing to install an unverified binary"
    }
    $want = ((Get-Content (Join-Path $tmp "$asset.sha256") -Raw) -split '\s+')[0].ToLower()
    $got  = (Get-FileHash -Algorithm SHA256 (Join-Path $tmp $asset)).Hash.ToLower()
    if ($got -ne $want) { Fail "sha256 mismatch (want $want, got $got)" }
    Write-Okay "sha256 verified"

    # --- extract ---------------------------------------------------------------
    Write-Step "extracting..."
    Expand-Archive -Path (Join-Path $tmp $asset) -DestinationPath $tmp -Force
    $src = Join-Path (Join-Path $tmp $stage) 'xerj.exe'
    if (-not (Test-Path $src)) {
      $found = Get-ChildItem -Path $tmp -Recurse -Filter 'xerj.exe' | Select-Object -First 1
      if ($found) { $src = $found.FullName } else { Fail "xerj.exe not found in archive" }
    }

    # --- install ---------------------------------------------------------------
    $dir = if ($env:XERJ_INSTALL_DIR) { $env:XERJ_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\xerj' }
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $dest = Join-Path $dir 'xerj.exe'
    try {
      Move-Item -Force -Path $src -Destination $dest
    } catch {
      Fail "could not replace $dest - if xerj is running, stop it first (Stop-Process -Name xerj) and re-run this installer"
    }
    Write-Okay "installed $dest"

    # --- PATH (user scope, reversible; opt out with XERJ_NO_PATH=1) ------------
    if (-not $env:XERJ_NO_PATH) {
      $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
      if (-not $userPath) { $userPath = '' }
      $onPath = ($userPath -split ';' | Where-Object { $_ -eq $dir }).Count -gt 0
      if (-not $onPath) {
        $newPath = if ($userPath) { $userPath.TrimEnd(';') + ';' + $dir } else { $dir }
        $wrote = $false
        try {
          # Preserve REG_EXPAND_SZ (%USERPROFILE% entries) - the plain
          # SetEnvironmentVariable round-trip would expand and freeze them.
          $k = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
          if ($k) {
            $raw = $k.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
            $rawNew = if ($raw) { "$raw".TrimEnd(';') + ';' + $dir } else { $dir }
            $k.SetValue('Path', $rawNew, [Microsoft.Win32.RegistryValueKind]::ExpandString)
            $k.Close()
            $wrote = $true
          }
        } catch {}
        if (-not $wrote) { [Environment]::SetEnvironmentVariable('Path', $newPath, 'User') }
        Write-Okay "added $dir to your user PATH (new terminals pick it up automatically)"
      }
    }
    # Make `xerj` resolve in THIS session too, so the next-steps command works
    # immediately (the registry write above only affects new terminals).
    if (($env:Path -split ';') -notcontains $dir) {
      $env:Path = $env:Path.TrimEnd(';') + ';' + $dir
    }

    # --- next steps --------------------------------------------------------------
    Write-Host ""
    Write-Host "xerj $tag is ready." -ForegroundColor White
    Write-Host "  start it:               xerj --insecure --data-dir .\data"
    Write-Host "  then open the console:  http://localhost:9200/_xerj-console/"
    Write-Host "  docs: https://xerj.org/docs/  -  source: https://github.com/$Repo"
    Write-Host ""
  } finally {
    Remove-Item -Recurse -Force -Path $tmp -ErrorAction SilentlyContinue
  }
}
