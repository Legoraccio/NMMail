<#
.SYNOPSIS
  Removes a per-user NM Mail installation.
  -RemoveData also deletes the local cache/database and stored credentials.
#>
param([switch] $RemoveData)
$ErrorActionPreference = 'Continue'
$dest = Join-Path $env:LOCALAPPDATA 'Programs\NMMail'

try {
    $pipe = New-Object System.IO.Pipes.NamedPipeClientStream('.', "nmmail-$env:USERNAME", [System.IO.Pipes.PipeDirection]::InOut)
    $pipe.Connect(500)
    $w = New-Object System.IO.StreamWriter($pipe); $w.AutoFlush = $true
    $w.WriteLine('{"id":1,"method":"daemon.shutdown"}')
    Start-Sleep -Milliseconds 800
    $pipe.Dispose()
} catch { }
Get-Process -Name nmmaild, NMMail -ErrorAction SilentlyContinue | Stop-Process -Force

$daemon = Join-Path $dest 'nmmaild.exe'
if (Test-Path $daemon) { & $daemon --unregister }
Remove-Item (Join-Path ([Environment]::GetFolderPath('Programs')) 'NM Mail.lnk') -ErrorAction SilentlyContinue
Remove-Item -Recurse -Force $dest -ErrorAction SilentlyContinue

if ($RemoveData) {
    Remove-Item -Recurse -Force (Join-Path $env:LOCALAPPDATA 'NMMail') -ErrorAction SilentlyContinue
    cmdkey /list | Select-String 'NMMail:' | ForEach-Object {
        $target = ($_ -split 'target=')[-1].Trim()
        if ($target) { cmdkey /delete:$target | Out-Null }
    }
}
Write-Host 'NM Mail rimosso.' -ForegroundColor Green
