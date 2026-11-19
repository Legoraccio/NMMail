# NM Mail (Never Miss Mail) — Piano di progetto

## 1. Obiettivo

Client di posta per Windows (Gmail + Outlook, più IMAP generico) che **sa immediatamente**
quando arriva una mail, anche con la finestra chiusa, consumando il minimo indispensabile
di RAM/CPU/batteria in background e all'avvio. In futuro: macOS e Linux, riusando tutto
il codice non-UI.

### Principi
1. **Never miss**: push IMAP IDLE su connessione dedicata, watchdog, riconnessione
   immediata su resume/cambio rete, catch-up per UID: nessuna mail può "sfuggire".
2. **Leggerezza**: in background gira solo un demone Rust headless (obiettivo < 10 MB RAM,
   0% CPU a riposo). La UI è un processo separato: finestra chiusa = processo terminato.
3. **Velocità**: UI WinUI 3 compilata Native AOT, dati serviti da cache SQLite locale,
   corpi delle mail recenti già scaricati quando arriva la notifica.
4. **Riuso cross-platform**: tutta la logica (sync, auth, storage, MIME, protocollo IPC)
   vive nel core Rust; per ogni OS cambiano solo lo strato piattaforma del demone
   (tray, notifiche, autostart, eventi di power/rete) e la shell UI nativa.

## 2. Decisioni prese con l'utente

| Tema | Decisione |
|---|---|
| UI Windows | WinUI 3 nativo (C# / .NET 10, Windows App SDK 2.x, Native AOT) |
| Motore | Rust (core + demone), riusabile su macOS/Linux |
| OAuth | Guida passo-passo; client ID inseriti dall'utente (impostazioni o file locale). Sviluppo e test su server IMAP/SMTP locale finto |
| Notifiche Gmail | Tutta la Posta in arrivo (filtro "solo Principale" disponibile come opzione per account) |
| Cache locale | Ultimi 90 giorni (configurabile); messaggi più vecchi caricati on-demand |
| Lingua | Italiano + Inglese, segue la lingua di Windows (default italiano) |
| Git | Commit a ogni fase + push su repo GitHub privato |

## 3. Architettura

```
┌──────────────────────────── Windows ────────────────────────────┐
│                                                                  │
│  NMMail.exe (WinUI 3, C#, Native AOT)  ← aperto solo su richiesta │
│     │  JSON-lines su named pipe \\.\pipe\nmmail-<utente>          │
│     ▼                                                            │
│  nmmaild.exe (Rust, sempre attivo, ~5-10 MB)                      │
│   ├─ nmmail-core (cross-platform)                                │
│   │   ├─ sync engine: 1 connessione IDLE per account (INBOX)      │
│   │   │               + 1 connessione "worker" on-demand          │
│   │   ├─ auth: OAuth2 PKCE loopback (Google, Microsoft), password │
│   │   ├─ smtp: invio + outbox offline                            │
│   │   ├─ storage: SQLite WAL + FTS5                              │
│   │   ├─ mime: parsing (mail-parser), sanitizzazione HTML         │
│   │   └─ ipc: protocollo + server (named pipe / unix socket)     │
│   └─ platform/windows                                            │
│       ├─ tray icon (Win32 puro)                                  │
│       ├─ toast (WinRT ToastNotificationManager + AUMID registry)  │
│       ├─ autostart (HKCU\…\Run), protocollo nmmail:              │
│       ├─ eventi resume/sleep/display + cambio rete               │
│       └─ EcoQoS (Efficiency mode), single instance               │
└──────────────────────────────────────────────────────────────────┘
```

### Processi
- **nmmaild.exe** — avviato al login (`--background`), unico proprietario di rete e
  database. Mostra tray e notifiche. Registra in modo idempotente (per utente, senza
  admin) autostart, protocollo `nmmail:` e AppUserModelID per le notifiche.
- **NMMail.exe** — UI. Si collega al demone via pipe (lo avvia se non è attivo). Chiudere
  la finestra termina il processo; la tray resta del demone.
- **nmmaild.exe --activate <uri>** — mini-processo lanciato dal click su una notifica
  (`nmmail://open?msg=…`, `nmmail://markread?msg=…`): inoltra al demone e termina.

### Perché IMAP IDLE per entrambi
- Gmail API e Microsoft Graph offrono push solo verso webhook pubblici (Pub/Sub,
  change notifications): inadatti a un client desktop senza server.
- IMAP IDLE con XOAUTH2 è supportato da Gmail e Outlook.com/Microsoft 365, è push reale
  (il server scrive sulla socket appena la mail arriva) e funziona uguale su ogni OS.
- EWS è in dismissione su Exchange Online; ActiveSync non è un'opzione praticabile.

## 4. Strategia "never miss"

1. **Connessione IDLE dedicata per account** su INBOX, mai usata per altro: le azioni
   dell'utente passano dalla connessione worker, quindi l'IDLE non viene mai interrotto.
2. **Catch-up per UID**: per ogni cartella si salvano `UIDVALIDITY` e massimo UID visto.
   A ogni (ri)connessione: `UID FETCH <ultimo+1>:*` → nessun buco anche dopo ore offline.
   Se `UIDVALIDITY` cambia: risincronizzazione della finestra di cache.
3. **Rinnovo IDLE ogni 8 minuti** (Gmail chiude dopo 29, alcuni NAT molto prima) con
   watchdog: se il server non risponde a `DONE`/`NOOP` entro 10 s → riconnessione.
4. **TCP keepalive** (60 s) sulle socket per tenere vivi i NAT e scoprire peer morti.
5. **Eventi di sistema** (resume da sleep/Modern Standby, accensione display, cambio
   rete, sblocco sessione): health-check immediato di tutte le connessioni e catch-up.
6. **Rilevamento salti di orologio** (tick monotono vs wall clock): fallback
   cross-platform per scoprire uno sleep anche senza eventi dell'OS.
7. **Backoff di riconnessione** 1-2-5-10-30-60 s, azzerato da eventi di rete/resume.
8. **Notifica solo se**: UID nuovo, non `\Seen`, data interna recente (< 24 h), non
   inviata da me, account con notifiche attive (e, se scelto, Gmail categoria Principale
   via `X-GM-RAW`). Più di 3 mail insieme → una notifica riassuntiva.
9. **Prefetch**: per le mail nuove si scarica subito il messaggio intero (se < 2 MB):
   anteprima nella notifica e apertura istantanea.

## 5. Budget risorse (obiettivi misurati in fase 6)

| Metrica | Obiettivo |
|---|---|
| RAM demone (private working set, 2 account in IDLE) | < 10 MB |
| CPU demone a riposo | ~0% (solo timer di rinnovo IDLE / keepalive) |
| Latenza consegna server → toast | < 2 s |
| Riconnessione dopo resume/rete disponibile | < 3 s |
| Avvio a freddo UI (primo frame) | < 1 s |
| RAM con finestra chiusa | solo il demone |
| Binario demone | < 8 MB |

Accorgimenti: runtime tokio single-thread, SQLite con cache piccola, rustls con
verificatore di piattaforma, profilo release LTO + strip, EcoQoS su Windows 11,
nessun polling.

## 6. Struttura del repository

```
NMMail/
├─ Cargo.toml                 workspace Rust
├─ crates/
│  ├─ nmmail-core/            logica cross-platform (lib)
│  ├─ nmmail-daemon/          binario nmmaild (+ platform/windows, stub macos/linux)
│  └─ nmmail-testserver/      server IMAP+SMTP finto per test e sviluppo
├─ apps/
│  └─ windows/NMMail/         UI WinUI 3 (C#)
│  (in futuro: apps/macos SwiftUI, apps/linux GTK4 — stesso protocollo IPC)
├─ docs/
│  ├─ PLAN.md                 questo documento
│  ├─ IPC.md                  contratto demone ↔ UI
│  └─ OAUTH_SETUP.md          guida registrazione Google/Microsoft
└─ scripts/                   build, publish, packaging
```

## 7. Fasi

Ogni fase si chiude con build verde, test, commit e push.

### Fase 0 — Fondamenta
- Repo Git + GitHub privato, workspace Cargo, `.gitignore`, README, CLAUDE.md.
- Specifica IPC (`docs/IPC.md`).
- CI GitHub Actions (Windows): `cargo fmt/clippy/test`, build UI.

### Fase 1 — Core: modello dati e storage
- Tipi di dominio (account, cartelle, messaggi, flag, ruoli SPECIAL-USE).
- SQLite (rusqlite bundled, WAL, migrazioni, FTS5), finestra di cache 90 gg.
- Parsing MIME (mail-parser), snippet, allegati, sanitizzazione HTML (ammonia),
  risoluzione immagini `cid:` → `data:`, blocco contenuti remoti.
- Test unitari.

### Fase 2 — Core: motore IMAP
- Connessione TLS (rustls + platform verifier) / STARTTLS / plain (solo test).
- Autenticazione XOAUTH2, PLAIN/LOGIN.
- Loop IDLE con rinnovo, watchdog, keepalive, catch-up per UID, rilevamento
  `UIDVALIDITY`, flag ed expunge nella finestra di cache.
- Connessione worker: sync cartelle on-demand, corpi, flag, move/archivia/elimina
  (SPECIAL-USE: Archive/Trash/Junk; Gmail: All Mail).
- Server di test `nmmail-testserver` (IMAP+SMTP in memoria, consegna → IDLE).
- Test di integrazione: latenza push < 1 s su localhost, riconnessione, catch-up.

### Fase 3 — Core: autenticazione e invio
- OAuth2 Authorization Code + PKCE con redirect loopback, browser di sistema;
  provider Google e Microsoft (account personali e di lavoro); refresh automatico.
- Segreti nel Credential Manager di Windows (crate `keyring`, cross-platform).
- Account IMAP generici con password / password per app.
- Composizione MIME (risposta, rispondi a tutti, inoltra con citazione e allegati),
  invio SMTP (lettre) con XOAUTH2, outbox persistente per l'invio offline.

### Fase 4 — Demone Windows
- Server IPC su named pipe (solo utente corrente), eventi push verso la UI.
- Tray (Win32): apri, controlla ora, pausa notifiche, esci; badge non lette.
- Toast WinRT con AUMID registrato, anteprima, pulsanti "Segna come letto"/"Archivia",
  attivazione via protocollo `nmmail:`.
- Autostart HKCU Run, single instance, eventi power/rete, EcoQoS, log a rotazione.

### Fase 5 — UI WinUI 3
- Shell Mica con barra del titolo personalizzata, tema chiaro/scuro di sistema.
- Barra laterale: Posta in arrivo unificata, account e cartelle con contatori.
- Lista messaggi virtualizzata, raggruppata per data, caricamento progressivo.
- Lettore WebView2 (script disabilitati, CSP, immagini remote bloccate con pulsante
  "Mostra immagini", link aperti nel browser), allegati apri/salva.
- Azioni: archivia, elimina, letto/non letto, contrassegna, sposta; scorciatoie.
- Composizione in finestra separata (editor HTML leggero), destinatari con
  completamento dagli indirizzi noti, allegati.
- Ricerca istantanea locale (FTS5).
- Impostazioni: account (aggiungi con OAuth/password, rimuovi), notifiche, avvio
  automatico, cache, client ID OAuth, aspetto. Localizzazione it-IT / en-US.

### Fase 6 — Packaging e rifinitura
- Script di publish (Rust release + .NET Native AOT self-contained, x64 e arm64).
- Installazione per utente in `%LOCALAPPDATA%\Programs\NMMail` (script + Inno Setup),
  collegamento nel menu Start, registrazione `mailto:` opzionale.
- Misurazione del budget risorse e ottimizzazioni.
- `docs/OAUTH_SETUP.md`, README finale.

## 8. Rischi e mitigazioni

| Rischio | Mitigazione |
|---|---|
| Scope Gmail `https://mail.google.com/` è "restricted": in modalità Testing i refresh token scadono dopo 7 giorni | Guida: pubblicare l'app OAuth "In production" senza verifica (uso personale, avviso "app non verificata" una sola volta) |
| NAT/firewall che chiudono connessioni inattive in silenzio | Keepalive TCP + rinnovo IDLE a 8 min + watchdog |
| Modern Standby sospende la rete | Catch-up immediato al resume; nessuna mail persa, al più ritardata durante lo sleep |
| Native AOT con WinUI 3 | Binding compilati (`x:Bind`), JSON source-generated; fallback ReadyToRun |
| Toast per app non pacchettizzate | Registrazione AUMID in HKCU + attivazione via protocollo |
| Outlook.com: limiti/disconnessioni IMAP | Riconnessione con backoff, una sola connessione IDLE per account |

## 9. Roadmap oltre la v1
- macOS: shell SwiftUI + `platform/macos` (UNUserNotificationCenter, LaunchAgent,
  NSStatusItem, NSWorkspace sleep/wake) sullo stesso demone e protocollo.
- Linux: shell GTK4/libadwaita + `platform/linux` (notifiche D-Bus, autostart XDG,
  StatusNotifierItem, logind PrepareForSleep, NetworkManager).
- Vista a conversazioni, regole/filtri locali, PGP/S-MIME.

## 10. Stato di avanzamento

**Dopo la v1**: cartelle monitorate. Le mail che le regole di Outlook o i filtri di Gmail
spostano fuori dalla Posta in arrivo possono essere notificate all'istante: per ogni cartella
scelta il demone tiene un watcher IDLE dedicato (stessa robustezza dell'INBOX). Niente doppioni:
per Gmail si scartano i messaggi con etichetta `\Inbox`/`\Sent`/`\Draft` (es. "Tutti i
messaggi") e ogni Message-ID viene notificato una sola volta; la notifica indica la cartella.

| Fase | Stato |
|---|---|
| 0 — Fondamenta | ✅ repo privato, workspace, CI GitHub Actions |
| 1 — Storage e MIME | ✅ SQLite WAL + FTS5, parsing MIME, HTML sanitizzato con CSP |
| 2 — Motore IMAP | ✅ IDLE con watchdog, catch-up per UID, CONDSTORE, worker on-demand, server di test |
| 3 — Auth e invio | ✅ OAuth2 PKCE (Google/Microsoft), Credential Manager, SMTP + outbox offline |
| 4 — Demone Windows | ✅ tray, toast con azioni, protocollo `nmmail:`, power/rete, EcoQoS, `mailto:` |
| 5 — UI WinUI 3 | ✅ tre riquadri Fluent/Mica, lettore WebView2 isolato, composizione, ricerca, impostazioni, it/en, Native AOT |
| 6 — Packaging | ✅ `scripts/build.ps1` (dist 78 MB), install/uninstall per utente, Inno Setup; verifica end-to-end UI + demone + notifiche |

### Misure (build release, Windows 11)

| Metrica | Obiettivo | Misurato |
|---|---|---|
| Latenza consegna → notifica (server locale) | < 2 s | **1,2 ms** (test `initial_sync_does_not_notify_and_push_is_instant`) |
| Ripresa dopo resume con link "mezzo aperto" | < 3 s | < 1 s (test `resume_reconnects_from_half_open_link`) |
| Link morto dopo cambio rete | rilevato | ≤ 10 s + backoff 1 s (test `network_change_detects_dead_link`) |
| RAM privata demone, 2 account in IDLE | < 10 MB | **3,4 MB** (working set 3,5 MB dopo il trim) |
| CPU a riposo, 2 account in IDLE | ~0% | **16 ms in 110 s** (≈0,015%) |
| Binario demone | < 8 MB | **6,9 MB** |
| Click su notifica → azione eseguita | – | 24 ms |
| Apertura messaggio in cache (IPC + render) | – | ~10 ms |
| CPU demone in 1 ora con 2 account in IDLE | ~0% | **0,16 s** (0,0044%), 2 thread attivi |
| Avvio demone → IPC pronto | – | **146 ms** a freddo, ~25 ms a caldo |
| Avvio UI Native AOT (pronta all'input) | < 1 s | **114 ms** (primo frame ~300 ms, a freddo ~530 ms) |
| RAM UI con finestra aperta | – | ~140 MB + processi WebView2 (0 a finestra chiusa) |

### Limiti noti della v1
- Durante la sospensione (Modern Standby) Windows toglie la rete alle app desktop: le mail
  arrivate nel frattempo vengono recuperate e notificate subito al risveglio.
- Le cartelle diverse dalla Posta in arrivo (mail spostate da regole/filtri del server) sono
  notificate solo se attivate dall'utente (menu della cartella → "Notifica subito…"), fino a
  5 per account: ognuna usa una connessione IDLE dedicata.
- Bozze non salvate sul server; contatori delle cartelle calcolati sulla cache locale (90 giorni).
- Build ARM64 della UI: richiede gli strumenti MSVC ARM64 (non installati su questa macchina).
