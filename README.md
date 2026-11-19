# NM Mail — Never Miss Mail

Client di posta leggero per Windows (Gmail, Outlook e IMAP) che riceve le mail **all'istante**
grazie a IMAP IDLE, anche a finestra chiusa, con un demone in background di pochi MB.

- **Demone** `nmmaild` (Rust): sync push, notifiche, tray, avvio automatico.
- **UI** `NMMail.exe` (WinUI 3, .NET Native AOT): si apre solo quando serve.
- **Core** cross-platform (Rust): pronto per macOS e Linux.

Documentazione:
- [Piano di progetto](docs/PLAN.md)
- [Protocollo IPC demone ↔ UI](docs/IPC.md)
- [Configurare OAuth per Gmail e Outlook](docs/OAUTH_SETUP.md)

## Requisiti di sviluppo

- Windows 10 1809+ / Windows 11
- Rust stable (`rustup`), Visual Studio 2022/2026 con C++ build tools
- .NET 10 SDK, Windows App SDK (via NuGet)

## Build rapida

```powershell
cargo build --release
dotnet build apps/windows/NMMail/NMMail.csproj -c Release
```

## Server di test locale

```powershell
cargo run -p nmmail-testserver
```

Avvia un server IMAP (127.0.0.1:1143) + SMTP (127.0.0.1:1025) in memoria con l'utente
`test@nmmail.local` / `test`, utile per provare il client senza account reali.
