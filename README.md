# NM Mail — Never Miss Mail

Client di posta leggero per Windows (Gmail, Outlook e qualsiasi server IMAP) che riceve
le mail **all'istante** grazie a IMAP IDLE, anche a finestra chiusa, con un servizio in
background di pochi MB.

| Componente | Tecnologia | Ruolo |
|---|---|---|
| `nmmaild.exe` | Rust | Demone sempre attivo: push IMAP, notifiche, tray, avvio automatico |
| `NMMail.exe` | WinUI 3, .NET 10 Native AOT | Interfaccia, aperta solo quando serve |
| `nmmail-core` | Rust, cross-platform | Sync, storage, OAuth, SMTP, MIME, protocollo IPC (pronto per macOS/Linux) |

## Perché non perde mai una mail

- **Una connessione IDLE dedicata per account** su Posta in arrivo: il server spinge la
  notifica appena la mail arriva (latenza misurata nei test: pochi millisecondi in locale).
- **Watchdog**: IDLE rinnovato ogni 8 minuti, risposta a `DONE` entro 10 s o riconnessione;
  keepalive TCP a 60 s per tenere vivi i NAT.
- **Catch-up per UID**: dopo una disconnessione si scaricano tutti gli UID mancanti, quindi
  nessun buco anche dopo ore offline.
- **Eventi di Windows**: ripresa da sospensione/Modern Standby (riconnessione immediata),
  cambi di rete, accensione schermo, sblocco sessione (verifica della connessione).
- **Notifiche toast** con anteprima e pulsanti *Segna come letto / Archivia / Elimina*;
  ritirate automaticamente se leggi la mail altrove.

## Leggerezza

- Demone: ~2,5 MB di memoria privata a riposo, 0% CPU (nessun polling), modalità
  *Efficienza* (EcoQoS) di Windows 11, trim periodico del working set.
- La finestra è un processo separato: chiusa = 0 MB.
- Database locale SQLite (WAL + ricerca full-text FTS5), cache degli ultimi 90 giorni.

## Documentazione

- [Piano di progetto e architettura](docs/PLAN.md)
- [Protocollo IPC demone ↔ UI](docs/IPC.md)
- [Configurare OAuth per Gmail e Outlook](docs/OAUTH_SETUP.md)

## Sviluppo

Requisiti: Windows 10 1809+ / 11, Rust stable, Visual Studio 2022/2026 con strumenti C++,
.NET 10 SDK.

```powershell
cargo test --workspace                 # test unitari + integrazione (server IMAP finto)
cargo run -p nmmail-testserver         # server IMAP 127.0.0.1:1143 + SMTP 127.0.0.1:1025
cargo run -p nmmail-daemon             # demone (build di debug: non si registra all'avvio)
dotnet build apps/windows/NMMail/NMMail.csproj -c Debug -p:Platform=x64
```

Il server di test crea l'utente `test@nmmail.local` / `test` con una casella di esempio;
`--auto-deliver 30` consegna una mail ogni 30 secondi per provare le notifiche. Nell'app:
*Aggiungi account → Altro (IMAP)*, host `127.0.0.1`, IMAP `1143`, SMTP `1025`, sicurezza
*nessuna*.

Variabili utili: `NMMAIL_DATA_DIR` (cartella dati alternativa), `NMMAIL_LOG`
(es. `debug`), `NMMAIL_UI` (percorso di `NMMail.exe` lanciato dal demone).

## Build e installazione

```powershell
.\scripts\build.ps1                    # dist\NMMail-x64 (demone + UI Native AOT)
.\scripts\install.ps1                  # installa per l'utente corrente, senza admin
.\scripts\uninstall.ps1 [-RemoveData]  # disinstalla
```

In alternativa `installer\NMMail.iss` produce un setup con Inno Setup 6.
