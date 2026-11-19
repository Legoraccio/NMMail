<#
.SYNOPSIS
  Prova completa in locale, senza account reali: server IMAP/SMTP di test + demone + UI.

.DESCRIPTION
  - Compila server di test, demone e UI (Debug).
  - Avvia il server di test in una finestra separata (utente test@nmmail.local / test)
    che consegna una nuova mail ogni -AutoDeliver secondi.
  - Avvia il demone con una cartella dati temporanea (non tocca la tua installazione,
    non si registra all'avvio di Windows) e aggiunge l'account di test.
  - Apre la UI.
  Chiudi tutto con: .\scripts\dev-demo.ps1 -Stop
#>
param(
    [int] $AutoDeliver = 45,
    [switch] $Stop
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$data = Join-Path $env:TEMP 'NMMail-demo'
$cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
if (-not (Test-Path $cargo)) { $cargo = 'cargo' }

function Invoke-Daemon([string[]] $Requests) {
    $pipe = New-Object System.IO.Pipes.NamedPipeClientStream('.', "nmmail-$env:USERNAME", [System.IO.Pipes.PipeDirection]::InOut)
    $pipe.Connect(5000)
    $enc = New-Object System.Text.UTF8Encoding($false)
    $w = New-Object System.IO.StreamWriter($pipe, $enc); $w.AutoFlush = $true
    $r = New-Object System.IO.StreamReader($pipe, $enc)
    $out = @()
    foreach ($req in $Requests) {
        $id = ($req | ConvertFrom-Json).id
        $w.WriteLine($req)
        while ($true) {
            $line = $r.ReadLine()
            if ($null -eq $line) { break }
            if ($line -match ('^\{"id":' + $id + '[,}]')) { $out += $line; break }
        }
    }
    $pipe.Dispose()
    return $out
}

if ($Stop) {
    try { Invoke-Daemon @('{"id":1,"method":"daemon.shutdown"}') | Out-Null } catch { }
    Get-Process -Name NMMail, nmmail-testserver -ErrorAction SilentlyContinue | Stop-Process -Force
    Write-Host 'Demo fermata.' -ForegroundColor Green
    return
}

Write-Host '==> Build (Rust)' -ForegroundColor Cyan
& $cargo build -p nmmail-testserver -p nmmail-daemon
if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
Write-Host '==> Build (UI)' -ForegroundColor Cyan
dotnet build (Join-Path $root 'apps\windows\NMMail\NMMail.csproj') -c Debug -p:Platform=x64 -v q -nologo
if ($LASTEXITCODE -ne 0) { throw 'dotnet build failed' }
$ui = Get-ChildItem (Join-Path $root 'apps\windows\NMMail\bin') -Recurse -Filter NMMail.exe |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $ui) { throw 'NMMail.exe non trovato' }

Write-Host '==> Server di test' -ForegroundColor Cyan
if (-not (Get-Process -Name nmmail-testserver -ErrorAction SilentlyContinue)) {
    Start-Process (Join-Path $root 'target\debug\nmmail-testserver.exe') `
        -ArgumentList '--auto-deliver', $AutoDeliver
    Start-Sleep -Seconds 1
}

Write-Host '==> Demone' -ForegroundColor Cyan
$env:NMMAIL_DATA_DIR = $data
$env:NMMAIL_UI = $ui.FullName
$daemon = Join-Path $root 'target\debug\nmmaild.exe'
Start-Process $daemon -ArgumentList '--background' -WindowStyle Hidden
Start-Sleep -Seconds 1

$accounts = Invoke-Daemon @('{"id":1,"method":"accounts.list"}')
if ($accounts -notmatch 'test@nmmail.local') {
    $add = '{"id":2,"method":"accounts.addImap","params":{"email":"test@nmmail.local","displayName":"Account di prova","password":"test","imapHost":"127.0.0.1","imapPort":1143,"imapSecurity":"none","smtpHost":"127.0.0.1","smtpPort":1025,"smtpSecurity":"none"}}'
    Invoke-Daemon @($add) | Out-Null
}

Write-Host '==> UI' -ForegroundColor Cyan
Start-Process $ui.FullName
Write-Host "Pronto. Una nuova mail arriva ogni $AutoDeliver s (notifica toast). Ferma con: .\scripts\dev-demo.ps1 -Stop" -ForegroundColor Green
