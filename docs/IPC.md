# Protocollo IPC demone ↔ UI (v1)

Contratto tra `nmmaild` (Rust) e le shell UI native (WinUI 3 oggi; SwiftUI/GTK domani).
La UI non accede mai direttamente al database né alla rete: tutto passa da qui.

## Trasporto

| OS | Endpoint |
|---|---|
| Windows | named pipe `\\.\pipe\nmmail-<USERNAME>` (solo utente corrente, niente client remoti) |
| macOS | socket Unix `~/Library/Application Support/NMMail/nmmail.sock` |
| Linux | socket Unix `$XDG_RUNTIME_DIR/nmmail.sock` |

- Framing: **una riga JSON UTF-8 per messaggio**, terminata da `\n`.
- Campi in `camelCase`. Date = millisecondi Unix (`i64`). ID = `i64`.
- Più client possono essere connessi; gli eventi vengono inviati a tutti.

## Messaggi

Richiesta (client → demone):
```json
{"id": 7, "method": "messages.list", "params": {"folderId": 3, "limit": 50}}
```
Risposta (demone → client), stesso `id`:
```json
{"id": 7, "result": {"messages": []}}
{"id": 7, "error": {"code": "notFound", "message": "folder 3 not found"}}
```
Evento (demone → client, non sollecitato):
```json
{"event": "mail.new", "data": {"accountId": 1, "messageIds": [42]}}
```

Codici di errore: `badRequest`, `notFound`, `authRequired`, `offline`, `cancelled`,
`server`, `internal`.

Il primo messaggio del client deve essere `hello`. La UI usa `client: "ui"`: così il
demone sa che c'è una finestra a cui inviare `ui.show` invece di lanciarne una nuova.

## Tipi

```ts
type Addr = { name: string | null, address: string }

type AccountState = "connecting" | "online" | "offline" | "error" | "authRequired"

type Account = {
  id: number, provider: "gmail" | "outlook" | "imap",
  email: string, displayName: string,
  state: AccountState, error: string | null, lastSync: number | null,
  unread: number,               // non lette in INBOX
  notify: boolean,              // notifiche attive per l'account
  notifyFilter: "all" | "primary"   // "primary" ha effetto solo su Gmail
}

type FolderRole = "inbox" | "sent" | "drafts" | "trash" | "archive" | "spam"
                | "all" | "flagged" | "important" | null

type Folder = {
  id: number, accountId: number, name: string, displayName: string,
  role: FolderRole, parentId: number | null, depth: number,
  unread: number, total: number, selectable: boolean
}

type MessageSummary = {
  id: number, accountId: number, folderId: number,
  subject: string, from: Addr | null, to: Addr[],
  date: number, snippet: string,
  seen: boolean, flagged: boolean, answered: boolean,
  hasAttachments: boolean, size: number
}

type Attachment = {
  partId: number, filename: string, mime: string, size: number, inline: boolean
}

type MessageDetail = MessageSummary & {
  cc: Addr[], bcc: Addr[], replyTo: Addr[],
  messageIdHeader: string | null,
  html: string | null,          // documento HTML completo, sanitizzato, con CSP
  text: string | null,          // testo semplice (fallback)
  attachments: Attachment[],
  hasRemoteContent: boolean,    // contenuti remoti presenti e bloccati
  remoteAllowed: boolean
}

type Settings = {
  autostart: boolean,
  notificationsEnabled: boolean,
  notificationSound: boolean,
  showPreview: boolean,          // mittente/oggetto/anteprima nella notifica
  cacheDays: number,             // default 90
  theme: "system" | "light" | "dark",
  language: "system" | "it" | "en",
  quietUntil: number | null,     // notifiche in pausa fino a
  googleClientId: string, googleClientSecret: string,
  microsoftClientId: string
}
```

## Metodi

| Metodo | Parametri | Risultato |
|---|---|---|
| `hello` | `{client: string, version: string}` | `{daemonVersion: string, protocol: 1}` |
| `daemon.status` | – | `{version, pid, uptimeSec, memoryBytes, accounts: Account[]}` |
| `daemon.shutdown` | – | `{}` |
| `accounts.list` | – | `{accounts: Account[]}` |
| `accounts.addOAuth` | `{provider: "gmail" \| "outlook"}` | `{account: Account}` — apre il browser, può durare minuti |
| `accounts.cancelAuth` | – | `{}` |
| `accounts.addImap` | `{email, displayName, username?, password, imapHost, imapPort, imapSecurity, smtpHost, smtpPort, smtpSecurity}` (`security`: `"tls"\|"starttls"\|"none"`) | `{account: Account}` |
| `accounts.update` | `{id, displayName?, notify?, notifyFilter?}` | `{account: Account}` |
| `accounts.remove` | `{id}` | `{}` |
| `folders.list` | `{accountId?}` | `{folders: Folder[]}` |
| `folders.sync` | `{folderId}` | `{}` (poi evento `messages.changed`) |
| `messages.list` | `{folderId?, unified?: "inbox", beforeDate?, beforeId?, limit?, unreadOnly?}` | `{messages: MessageSummary[], hasMore: boolean}` |
| `messages.loadOlder` | `{folderId, beforeDate}` | `{messages: MessageSummary[], hasMore: boolean}` (dal server, oltre la cache) |
| `messages.get` | `{id, allowRemote?: boolean}` | `{message: MessageDetail}` |
| `messages.setFlags` | `{ids: number[], seen?: boolean, flagged?: boolean}` | `{}` |
| `messages.archive` | `{ids}` | `{}` |
| `messages.delete` | `{ids}` | `{}` (sposta nel Cestino; se già nel Cestino elimina) |
| `messages.move` | `{ids, folderId}` | `{}` |
| `messages.search` | `{query, accountId?, limit?}` | `{messages: MessageSummary[]}` |
| `attachments.save` | `{messageId, partId, path}` | `{path}` |
| `contacts.suggest` | `{prefix, limit?}` | `{contacts: Addr[]}` |
| `compose.prepare` | `{mode: "reply" \| "replyAll" \| "forward", messageId, lang?: "it" \| "en"}` | `{accountId, to, cc, subject, html, text, inReplyTo, references, attachments: {messageId, partId, filename, size}[]}` |
| `compose.send` | `{accountId, to, cc, bcc, subject, html, text, attachments: string[] (path), forwardAttachments?: {messageId, partId}[], inReplyTo?, references?, replyToMessageId?}` | `{outboxId, queued}` — invio immediato; se offline `queued: true` e il messaggio parte appena torna la rete (`outbox.sent`). Errori permanenti (indirizzo rifiutato…) arrivano come errore della richiesta |
| `settings.get` | – | `{settings: Settings}` |
| `settings.set` | `Partial<Settings>` | `{settings: Settings}` |
| `sync.now` | `{accountId?}` | `{}` |
| `notifications.pause` | `{until: number \| null}` | `{}` |
| `ui.show` | `{messageId?}` | `{}` — porta la UI in primo piano (avviandola se serve) |
| `activate` | `{uri}` | `{}` — uso interno: click sulle notifiche (`nmmail://open?msg=…`, `markread`, `archive`, `delete`) |

## Eventi

| Evento | Dati |
|---|---|
| `account.status` | `Account` |
| `accounts.changed` | `{}` |
| `folders.changed` | `{accountId}` |
| `messages.changed` | `{accountId, folderIds: number[]}` |
| `mail.new` | `{accountId, messageIds: number[]}` |
| `outbox.sent` | `{outboxId}` |
| `outbox.failed` | `{outboxId, error}` |
| `auth.progress` | `{stage: "browser" \| "exchanging" \| "connecting" \| "done", url?: string}` |
| `ui.show` | `{messageId: number \| null}` — la UI deve portarsi in primo piano |
| `settings.changed` | `Settings` |
