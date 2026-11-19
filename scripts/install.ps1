<#
.SYNOPSIS
  Per-user install of NM Mail (no admin rights) from a build in dist\.
  Installs to %LOCALAPPDATA%\Programs\NMMail, adds a Start menu shortcut,
  registers the nmmail: protocol / notification identity and starts the daemon.
#>
param(
    [string] $Source = (Join-Path (Split-Path -Parent $PSScriptRoot) 'dist\NMMail-x64'),
    [switch] $NoStart
)
$ErrorActionPreference = 'Stop'
$dest = Join-Path $env:LOCALAPPDATA 'Programs\NMMail'
if (-not (Test-Path (Join-Path $Source 'nmmaild.exe'))) { throw "Build not found in $Source (run scripts\build.ps1)" }

# Stop running instances gracefully, then forcefully.
function Stop-NMMail {
    try {
        $pipe = New-Object System.IO.Pipes.NamedPipeClientStream('.', "nmmail-$env:USERNAME", [System.IO.Pipes.PipeDirection]::InOut)
        $pipe.Connect(500)
        $w = New-Object System.IO.StreamWriter($pipe); $w.AutoFlush = $true
        $w.WriteLine('{"id":1,"method":"daemon.shutdown"}')
        Start-Sleep -Milliseconds 800
        $pipe.Dispose()
    } catch { }
    Get-Process -Name nmmaild, NMMail -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
}
Stop-NMMail

New-Item -ItemType Directory -Force $dest | Out-Null
robocopy $Source $dest /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
if ($LASTEXITCODE -ge 8) { throw "robocopy failed ($LASTEXITCODE)" }

$programs = [Environment]::GetFolderPath('Programs')
$shell = New-Object -ComObject WScript.Shell
$ui = Join-Path $dest 'NMMail.exe'
$target = if (Test-Path $ui) { $ui } else { Join-Path $dest 'nmmaild.exe' }
$lnk = $shell.CreateShortcut((Join-Path $programs 'NM Mail.lnk'))
$lnk.TargetPath = $target
$lnk.WorkingDirectory = $dest
$lnk.IconLocation = "$target,0"
$lnk.Description = 'NM Mail — Never Miss Mail'
$lnk.Save()

& (Join-Path $dest 'nmmaild.exe') --register
if (-not $NoStart) {
    Start-Process (Join-Path $dest 'nmmaild.exe')
}
Write-Host "NM Mail installato in $dest" -ForegroundColor Green
