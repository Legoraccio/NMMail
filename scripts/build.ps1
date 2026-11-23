<#
.SYNOPSIS
  Builds NM Mail for release into dist\NMMail-<arch>.
.EXAMPLE
  .\scripts\build.ps1            # x64, daemon + UI (Native AOT)
  .\scripts\build.ps1 -SkipUi    # daemon only
#>
param(
    [ValidateSet('x64', 'arm64')] [string] $Arch = 'x64',
    [switch] $SkipUi
)
$ErrorActionPreference = 'Stop'

# Runs a native command; PowerShell 5.1 must not turn its stderr progress into errors.
function Invoke-Native([scriptblock] $Command, [string] $What) {
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try { & $Command 2>&1 | ForEach-Object { "$_" } } finally { $ErrorActionPreference = $previous }
    if ($LASTEXITCODE -ne 0) { throw "$What failed (exit code $LASTEXITCODE)" }
}

$root = Split-Path -Parent $PSScriptRoot
$dist = Join-Path $root "dist\NMMail-$Arch"
$cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
if (-not (Test-Path $cargo)) { $cargo = 'cargo' }
# Some shells set this and it breaks the Visual Studio environment used by the AOT linker.
Remove-Item Env:NoDefaultCurrentDirectoryInExePath -ErrorAction SilentlyContinue

# Built-in OAuth clients (see docs/OAUTH_SETUP.md): environment variables win
# (CI secrets), otherwise oauth.local.toml (git-ignored). Values are never printed.
$oauthVars = [ordered]@{
    google_client_id     = 'NMMAIL_GOOGLE_CLIENT_ID'
    google_client_secret = 'NMMAIL_GOOGLE_CLIENT_SECRET'
    microsoft_client_id  = 'NMMAIL_MICROSOFT_CLIENT_ID'
}
$oauthFile = Join-Path $root 'oauth.local.toml'
if (Test-Path $oauthFile) {
    foreach ($line in Get-Content $oauthFile) {
        if ($line -match '^\s*([a-z_]+)\s*=\s*"([^"]*)"') {
            $var = $oauthVars[$Matches[1]]
            if ($var -and $Matches[2].Trim() -and -not [Environment]::GetEnvironmentVariable($var)) {
                Set-Item -Path "Env:$var" -Value $Matches[2].Trim()
            }
        }
    }
}
$hasGoogle = [bool]$env:NMMAIL_GOOGLE_CLIENT_ID
$hasMicrosoft = [bool]$env:NMMAIL_MICROSOFT_CLIENT_ID
# ASCII only: Windows PowerShell 5.1 reads BOM-less scripts with the ANSI code page.
$state = { param($v) if ($v) { 'incluse' } else { 'assenti (gli utenti dovranno inserirle a mano)' } }
Write-Host ("==> Credenziali OAuth - Google: {0}; Microsoft: {1}" -f (& $state $hasGoogle), (& $state $hasMicrosoft)) -ForegroundColor Cyan

if (Test-Path $dist) { Remove-Item -Recurse -Force $dist }
New-Item -ItemType Directory -Force $dist | Out-Null

$target = if ($Arch -eq 'x64') { 'x86_64-pc-windows-msvc' } else { 'aarch64-pc-windows-msvc' }
Write-Host "==> nmmaild ($target)" -ForegroundColor Cyan
Invoke-Native { & $cargo build --release -p nmmail-daemon --target $target } 'cargo build'
Copy-Item (Join-Path $root "target\$target\release\nmmaild.exe") $dist
Copy-Item (Join-Path $root 'assets\icon\nmmail.png') $dist -ErrorAction SilentlyContinue

if (-not $SkipUi) {
    Write-Host "==> NMMail.exe (win-$Arch, Native AOT)" -ForegroundColor Cyan
    $csproj = Join-Path $root 'apps\windows\NMMail\NMMail.csproj'
    Invoke-Native {
        dotnet publish $csproj -c Release -r "win-$Arch" -p:Platform=$Arch -p:PublishAot=true -o $dist -nologo
    } 'dotnet publish'
    # Debug symbols are not shipped.
    Get-ChildItem $dist -Filter *.pdb | Remove-Item -Force
}

$size = (Get-ChildItem $dist -Recurse | Measure-Object Length -Sum).Sum / 1MB
Write-Host ("==> {0}  ({1:N1} MB)" -f $dist, $size) -ForegroundColor Green
