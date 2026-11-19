# Configurare l'accesso a Gmail e Outlook

NM Mail usa **OAuth 2.0** (il metodo ufficiale e sicuro: la password non viene mai vista
dall'app). Per farlo servono due "client ID", uno per Google e uno per Microsoft, che
registri una volta sola con il tuo account. Costa zero e richiede circa 10 minuti.

I client ID si inseriscono in **NM Mail → Impostazioni → OAuth**. In alternativa, chi
compila l'app può impostarli a build time con le variabili d'ambiente
`NMMAIL_GOOGLE_CLIENT_ID`, `NMMAIL_GOOGLE_CLIENT_SECRET`, `NMMAIL_MICROSOFT_CLIENT_ID`.

> Vuoi provare subito Gmail senza Google Cloud? Vai in fondo: **password per le app**.

---

## 1. Google (Gmail)

1. Apri <https://console.cloud.google.com/> e crea un progetto (es. `NM Mail`).
2. **API e servizi → Libreria**: cerca **Gmail API** e premi *Abilita*.
3. **Google Auth Platform** (ex "Schermata consenso OAuth"):
   - *Branding*: nome app `NM Mail`, email di assistenza = la tua.
   - *Pubblico* (Audience): tipo **Esterno**. Aggiungi il tuo indirizzo Gmail tra gli
     *Utenti di test*.
   - *Accesso ai dati* (Data access): **Aggiungi o rimuovi ambiti** → inserisci
     manualmente `https://mail.google.com/` e seleziona anche `openid`, `.../auth/userinfo.email`,
     `.../auth/userinfo.profile`.
4. **Client** → **Crea client** → Tipo di applicazione **App desktop** → nome
   `NM Mail Desktop` → *Crea*. Copia **ID client** e **Client secret**
   (per le app desktop il "secret" non è davvero segreto: Google lo richiede comunque).
5. **Importante — token che durano**: finché l'app è in stato *Test*, Google fa scadere
   l'accesso ogni **7 giorni**. Per un uso personale continuativo vai su *Pubblico* →
   **Pubblica app** (stato *In produzione*). Non serve la verifica di Google: al primo
   accesso vedrai l'avviso *"Google non ha verificato questa app"* → **Avanzate** →
   **Vai a NM Mail**. (Limite: 100 utenti, più che sufficiente per uso personale.)
6. In NM Mail: **Impostazioni → OAuth** → incolla *Google client ID* e *Google client
   secret* → **Aggiungi account → Gmail**. Si apre il browser: accedi e consenti.

## 2. Microsoft (Outlook.com, Hotmail, Live, Microsoft 365)

1. Apri <https://entra.microsoft.com/> (o <https://portal.azure.com/> → *Microsoft Entra ID*)
   con il tuo account Microsoft. Gli account personali hanno una directory gratuita.
2. **Registrazioni app → Nuova registrazione**:
   - Nome: `NM Mail`
   - Tipi di account supportati: **Account in qualsiasi directory organizzativa e
     account Microsoft personali**
   - URI di reindirizzamento: piattaforma **Client pubblico/nativo (per dispositivi mobili e
     desktop)**, valore `http://localhost`
   - *Registra*.
3. Nella pagina *Panoramica* copia **ID applicazione (client)**.
4. (Facoltativo) **Autorizzazioni API → Aggiungi**: *APIs my organization uses* →
   *Office 365 Exchange Online* → *Delegate* → `IMAP.AccessAsUser.All`, `SMTP.Send`.
   Non è obbligatorio: NM Mail richiede questi permessi al momento dell'accesso.
5. In NM Mail: **Impostazioni → OAuth** → incolla *Microsoft client ID* →
   **Aggiungi account → Outlook**.

**Account di lavoro/scuola (Microsoft 365)**: l'amministratore potrebbe dover approvare
l'app e deve avere **SMTP AUTH** abilitato per la casella (molti tenant lo disattivano),
altrimenti la ricezione funziona ma l'invio no.

## 3. Alternativa rapida per Gmail: password per le app

Se hai la verifica in due passaggi attiva puoi saltare Google Cloud:

1. Crea una password su <https://myaccount.google.com/apppasswords> (nome: `NM Mail`).
2. In NM Mail: **Aggiungi account → Altro (IMAP)** con:
   - Email e nome utente: il tuo indirizzo Gmail; password: quella generata (16 lettere)
   - IMAP `imap.gmail.com`, porta `993`, sicurezza **TLS**
   - SMTP `smtp.gmail.com`, porta `465`, sicurezza **TLS**

La ricezione istantanea (IMAP IDLE) funziona esattamente come con OAuth.
Outlook.com non supporta più le password per le app con IMAP: serve OAuth.

## Dove finiscono le credenziali

- I token di accesso/rinnovo e le password sono salvati nel **Gestione credenziali di
  Windows** (voci `NMMail:account-…`), mai in chiaro su disco.
- I client ID stanno nel database locale di NM Mail (`%LOCALAPPDATA%\NMMail`).
- Rimuovendo un account dall'app vengono eliminati anche i suoi segreti.
