# NM Mail — note per lo sviluppo

Client mail "Never Miss Mail": demone Rust headless (push IMAP IDLE) + UI WinUI 3 separata.
Piano completo in `docs/PLAN.md`, contratto demone ↔ UI in `docs/IPC.md`.

## Layout
- `crates/nmmail-core` — logica cross-platform (storage SQLite, sync IMAP, OAuth, SMTP, MIME, IPC). Niente codice specifico di un OS qui, salvo dietro `cfg`.
- `crates/nmmail-daemon` — binario `nmmaild`; `src/platform/<os>` contiene tray, notifiche, autostart, eventi power/rete.
- `crates/nmmail-testserver` — server IMAP+SMTP in memoria per test e sviluppo.
- `apps/windows/NMMail` — UI WinUI 3 (C#, .NET 10, Native AOT). Parla solo con il demone via named pipe.

## Comandi
- Rust: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`
- UI: `dotnet build apps/windows/NMMail/NMMail.csproj -c Debug -p:Platform=x64`
- UI senza demone (dati finti): `NMMail.exe --demo`
- Prova completa locale: `.\scripts\dev-demo.ps1` (ferma con `-Stop`)
- Distribuzione: `.\scripts\build.ps1` → `dist\NMMail-x64`

## Variabili d'ambiente utili
- `NMMAIL_DATA_DIR` — cartella dati alternativa (test/demo). Con questa variabile il demone non si registra all'avvio di Windows.
- `NMMAIL_LOG` — filtro log del demone (es. `debug`, `info,nmmail_core=debug`).
- `NMMAIL_UI` / `NMMAIL_DAEMON` — percorso dell'altro eseguibile quando non stanno nella stessa cartella.
- `NMMAIL_PIPE` — nome pipe alternativo lato UI (test).
- `NMMAIL_SNAPSHOT_DIR` — solo build Debug della UI: scrivendo un tag in `<dir>\snap.req` la UI salva PNG delle sue finestre (utile a schermo bloccato).

## Regole
- Il demone deve restare minimale: niente thread/poll inutili, runtime tokio single-thread, nessuna dipendenza pesante solo per comodità.
- La connessione IDLE di un account non va mai usata per altre operazioni (usa la connessione worker); INBOX la sincronizza solo il watcher, che decide le notifiche.
- async-imap mette già tra virgolette i nomi delle cartelle: passare i nomi così come restituiti da LIST.
- Ogni modifica al protocollo IPC va riflessa in `docs/IPC.md`, nel core (`ipc.rs`) e nella UI (`Services/DaemonClient.cs`, `Models/IpcModels.cs`).
- UI: solo `x:Bind`, JSON source-generated, `LibraryImport` (deve restare compatibile Native AOT). Pacchetti `Microsoft.WindowsAppSDK.WinUI` + `.Foundation`, non il metapacchetto (+70 MB).
- Mai committare client secret OAuth, token o file `*.local.toml` / `.env`.
- Documentazione utente in italiano; codice e commenti in inglese.
