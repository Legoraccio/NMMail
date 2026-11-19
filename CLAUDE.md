# NM Mail — note per lo sviluppo

Client mail "Never Miss Mail": demone Rust headless (push IMAP IDLE) + UI WinUI 3 separata.
Piano completo in `docs/PLAN.md`, contratto demone ↔ UI in `docs/IPC.md`.

## Layout
- `crates/nmmail-core` — logica cross-platform (storage SQLite, sync IMAP, OAuth, SMTP, MIME, IPC). Niente codice specifico di un OS qui, salvo dietro `cfg`.
- `crates/nmmail-daemon` — binario `nmmaild`; `src/platform/<os>` contiene tray, notifiche, autostart, eventi power/rete.
- `crates/nmmail-testserver` — server IMAP+SMTP in memoria per test e sviluppo.
- `apps/windows/NMMail` — UI WinUI 3 (C#, Native AOT). Parla solo con il demone via named pipe.

## Comandi
- Rust: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`
- UI: `dotnet build apps/windows/NMMail/NMMail.csproj -c Debug -p:Platform=x64`
- Server di test: `cargo run -p nmmail-testserver`

## Regole
- Il demone deve restare minimale: niente thread/poll inutili, runtime tokio single-thread, nessuna dipendenza pesante solo per comodità.
- La connessione IDLE di un account non va mai usata per altre operazioni (usa la connessione worker).
- Ogni modifica al protocollo IPC va riflessa in `docs/IPC.md`, nel core (`ipc::protocol`) e nella UI (`Services/DaemonClient`).
- Mai committare client secret OAuth, token o file `*.local.toml` / `.env`.
- Documentazione utente in italiano; codice e commenti in inglese.
