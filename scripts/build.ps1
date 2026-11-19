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
$root = Split-Path -Parent $PSScriptRoot
$dist = Join-Path $root "dist\NMMail-$Arch"
$cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
if (-not (Test-Path $cargo)) { $cargo = 'cargo' }

if (Test-Path $dist) { Remove-Item -Recurse -Force $dist }
New-Item -ItemType Directory -Force $dist | Out-Null

$target = if ($Arch -eq 'x64') { 'x86_64-pc-windows-msvc' } else { 'aarch64-pc-windows-msvc' }
Write-Host "==> nmmaild ($target)" -ForegroundColor Cyan
& $cargo build --release -p nmmail-daemon --target $target
if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
Copy-Item (Join-Path $root "target\$target\release\nmmaild.exe") $dist
Copy-Item (Join-Path $root 'assets\icon\nmmail.png') $dist -ErrorAction SilentlyContinue

if (-not $SkipUi) {
    Write-Host "==> NMMail.exe (win-$Arch, Native AOT)" -ForegroundColor Cyan
    dotnet publish (Join-Path $root 'apps\windows\NMMail\NMMail.csproj') -c Release -r "win-$Arch" `
        -p:Platform=$Arch -p:PublishAot=true -o $dist
    if ($LASTEXITCODE -ne 0) { throw 'dotnet publish failed' }
    # Debug symbols are not shipped.
    Get-ChildItem $dist -Filter *.pdb | Remove-Item -Force
}

$size = (Get-ChildItem $dist -Recurse | Measure-Object Length -Sum).Sum / 1MB
Write-Host ("==> {0}  ({1:N1} MB)" -f $dist, $size) -ForegroundColor Green
