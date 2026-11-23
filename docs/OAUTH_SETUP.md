# Accesso a Gmail e Outlook (OAuth)

NM Mail usa **OAuth 2.0**, il metodo ufficiale: l'utente accede nel browser con il proprio
account e l'app non vede mai la password.

Per farlo, Google e Microsoft devono conoscere l'**app** NM Mail tramite un *client ID*.
Il client ID identifica l'applicazione, non l'utente: **lo registra una sola volta chi
pubblica NM Mail** e viene incluso nei programmi al momento della compilazione. Chi usa una
versione compilata con le credenziali non deve configurare nulla: *Aggiungi account* →
Gmail o Outlook → accesso nel browser.

| Chi sei | Cosa fare |
|---|---|
| Usi una versione con le credenziali incluse | Niente: aggiungi l'account e accedi |
| Pubblichi NM Mail (o la compili da te) | Registra l'app su Google e Microsoft (sotto) e includi gli ID nella build |
| Vuoi solo usare il tuo Gmail, senza registrare nulla | [Password per le app](#gmail-con-la-password-per-le-app) |

---

## 1. Microsoft (Outlook.com, Hotmail, Live, Microsoft 365)

Gratuito, circa 10 minuti, non servono né dominio né informativa privacy.

1. Apri <https://entra.microsoft.com/#view/Microsoft_AAD_RegisteredApps/ApplicationsListBlade>
   e accedi con un account Microsoft. Potrebbe chiedere la verifica in due passaggi.
   - Se l'account personale non ha accesso, il portale propone di creare un account Azure:
     è gratuito, ma chiede telefono e carta di credito per verificare l'identità. In
     alternativa si può usare qualunque altro account Microsoft, anche di lavoro: la
     registrazione vale per tutti gli utenti.
2. **Nuova registrazione**:
   - Nome: `NM Mail`
   - Tipi di account supportati: **qualsiasi directory organizzativa e account Microsoft
     personali**
   - URI di reindirizzamento: piattaforma **Client pubblico/nativo (per dispositivi mobili e
     desktop)**, valore `http://localhost`
   - **Registra**.
3. Dalla pagina *Panoramica* copia **ID applicazione (client)**. Non è un segreto.

Gli account personali funzionano subito. Con gli account **di lavoro o scuola**
l'amministratore può dover approvare l'app (le app di editori non verificati spesso
richiedono il suo consenso) e deve avere abilitato **SMTP AUTH** per la casella, altrimenti
la ricezione funziona ma l'invio no.

## 2. Google (Gmail)

Gratuito, circa 10 minuti per la registrazione.

1. <https://console.cloud.google.com/projectcreate>: crea il progetto `NM Mail` e tienilo
   selezionato.
2. <https://console.cloud.google.com/auth/overview> → **Inizia**: nome app `NM Mail`,
   email di assistenza (visibile a chi accede: meglio un indirizzo dedicato), pubblico
   **Esterno**, email di contatto.
   - **Logo: lascialo vuoto.** Dopo aver caricato un logo Google pretende la verifica
     dell'app (tranne in modalità Test). Il file pronto per quando servirà è
     `assets/icon/nmmail-120.png` (120×120 px).
3. <https://console.cloud.google.com/auth/scopes>: aggiungi manualmente l'ambito
   `https://mail.google.com/` (più `openid`, email e profilo) e salva.
4. <https://console.cloud.google.com/auth/clients> → **Crea client** → tipo **App desktop** →
   copia subito **Client ID** e **Client secret**, che potrebbe essere mostrato solo in quel
   momento (in caso scarica il JSON). Per le app desktop Google stesso considera il secret
   non riservato, ma non va comunque messo nel repository.

### Test o produzione

L'accesso completo alla posta (`https://mail.google.com/`) per Google è un ambito
**riservato**:

- **Modalità Test** (subito disponibile): funziona per gli utenti di test che aggiungi in
  *Pubblico* (massimo 100), ma **l'accesso scade ogni 7 giorni** e NM Mail chiederà di
  accedere di nuovo. Va bene per provare, non per l'uso quotidiano.
- **Pubblicazione** (*Pubblico* → **Pubblica app**): Google chiede **home page** e
  **informativa privacy** su un dominio verificato di tua proprietà (si possono ospitare
  gratis su GitHub Pages, verificando il dominio con Google Search Console). Finché l'app non
  supera la verifica di Google, chi accede vede l'avviso *"app non verificata"*
  (**Avanzate** → **Vai a NM Mail**) e gli utenti sono limitati a 100. Oltre serve la
  verifica completa: settimane di revisione e, per gli ambiti della posta, possibilmente una
  valutazione di sicurezza esterna.

Per il **solo uso personale** di Gmail la strada più semplice resta la password per le app.

## 3. Includere gli ID nella build

1. Copia `oauth.example.toml` in **`oauth.local.toml`** nella cartella del progetto.
   Il file è escluso da git: non finisce mai nel repository pubblico.
2. Incolla gli identificativi:
   ```toml
   google_client_id = "…apps.googleusercontent.com"
   google_client_secret = "…"
   microsoft_client_id = "00000000-0000-0000-0000-000000000000"
   ```
   I campi vuoti vengono ignorati: puoi includere anche solo Microsoft.
3. Ricompila con `.\scripts\build.ps1`: lo script indica quali credenziali sono incluse,
   senza mai mostrarne i valori.

In alternativa (es. GitHub Actions) usa le variabili d'ambiente `NMMAIL_GOOGLE_CLIENT_ID`,
`NMMAIL_GOOGLE_CLIENT_SECRET`, `NMMAIL_MICROSOFT_CLIENT_ID`, che hanno la precedenza sul file.

In NM Mail, **Impostazioni → OAuth** conferma le credenziali incluse; *Usa credenziali
personalizzate (avanzato)* permette di sostituirle, ad esempio in una build compilata senza.

## Gmail con la password per le app

Nessuna registrazione, nessuna scadenza. Serve la verifica in due passaggi attiva.

1. Crea una password su <https://myaccount.google.com/apppasswords> (nome: `NM Mail`).
2. In NM Mail: **Aggiungi account → Altro (IMAP)…**, con il tuo indirizzo Gmail e la
   password di 16 lettere, senza spazi. I server si compilano da soli: IMAP `imap.gmail.com`
   porta 993 e SMTP `smtp.gmail.com` porta 465, sicurezza SSL/TLS.

La ricezione istantanea (IMAP IDLE) funziona esattamente come con OAuth. Outlook invece
richiede OAuth.

## Dove finiscono le credenziali

- Token di accesso e rinnovo e password stanno nel **Gestione credenziali di Windows**
  (voci `NMMail:account-…`), mai in chiaro su disco.
- Le credenziali personalizzate (Impostazioni → OAuth) stanno nel database locale di NM Mail
  (`%LOCALAPPDATA%\NMMail`); quelle incluse nella build restano nel programma.
- Rimuovendo un account dall'app vengono eliminati anche i suoi segreti.
