using System.Buffers.Binary;
using System.Globalization;
using System.IO.Compression;
using System.Net;
using System.Text;
using NMMail.Models;

namespace NMMail.Services.Demo;

/// <summary>A message of the demo store (detail + body source).</summary>
internal sealed class DemoMessage
{
    public required MessageDetail Detail { get; init; }
    public required string BodyHtml { get; set; }
    public bool RemoteImages { get; init; }
    public bool Important { get; init; }
    public bool TextOnly { get; init; }
}

/// <summary>Realistic Italian sample data for <c>--demo</c>.</summary>
internal static class DemoData
{
    public const long GmailId = 1;
    public const long OutlookId = 2;
    public const string GmailAddress = "marco.bianchi@example.com";
    public const string OutlookAddress = "marco.bianchi@example.org";
    public const string OwnName = "Marco Bianchi";

    private static readonly CultureInfo It = CultureInfo.GetCultureInfo("it-IT");

    public static List<Account> Accounts() =>
    [
        new Account
        {
            Id = GmailId, Provider = Providers.Gmail, Email = GmailAddress, DisplayName = OwnName,
            State = AccountStates.Online, Notify = true, NotifyFilter = NotifyFilterNames.All,
            LastSync = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds(),
        },
        new Account
        {
            Id = OutlookId, Provider = Providers.Outlook, Email = OutlookAddress, DisplayName = "Marco Bianchi (lavoro)",
            State = AccountStates.Connecting, Notify = true, NotifyFilter = NotifyFilterNames.All,
            LastSync = DateTimeOffset.UtcNow.AddMinutes(-3).ToUnixTimeMilliseconds(),
        },
    ];

    public static List<Folder> Folders() =>
    [
        F(101, GmailId, "INBOX", "Posta in arrivo", FolderRoles.Inbox),
        F(102, GmailId, "[Gmail]/Speciali", "Speciali", FolderRoles.Flagged),
        F(103, GmailId, "[Gmail]/Importanti", "Importanti", FolderRoles.Important),
        F(104, GmailId, "[Gmail]/Posta inviata", "Posta inviata", FolderRoles.Sent),
        F(105, GmailId, "[Gmail]/Bozze", "Bozze", FolderRoles.Drafts),
        F(106, GmailId, "[Gmail]/Tutti i messaggi", "Tutti i messaggi", FolderRoles.All),
        F(107, GmailId, "[Gmail]/Spam", "Spam", FolderRoles.Spam),
        F(108, GmailId, "[Gmail]/Cestino", "Cestino", FolderRoles.Trash),
        F(109, GmailId, "Lavoro", "Lavoro", null),
        F(110, GmailId, "Lavoro/Progetti", "Progetti", null, parent: 109, depth: 1, monitored: true),
        F(111, GmailId, "Viaggi", "Viaggi", null),

        F(201, OutlookId, "Inbox", "Posta in arrivo", FolderRoles.Inbox),
        F(202, OutlookId, "Sent", "Posta inviata", FolderRoles.Sent),
        F(203, OutlookId, "Drafts", "Bozze", FolderRoles.Drafts),
        F(204, OutlookId, "Archive", "Archivio", FolderRoles.Archive),
        F(205, OutlookId, "Junk", "Posta indesiderata", FolderRoles.Spam),
        F(206, OutlookId, "Deleted", "Posta eliminata", FolderRoles.Trash),
        F(207, OutlookId, "Fatture", "Fatture", null, monitored: true),
        F(208, OutlookId, "Fatture/2025", "2025", null, parent: 207, depth: 1),
    ];

    private static Folder F(long id, long account, string name, string display, string? role, long? parent = null, int depth = 0,
        bool monitored = false) =>
        new()
        {
            Id = id, AccountId = account, Name = name, DisplayName = display, Role = role, ParentId = parent, Depth = depth, Selectable = true,
            Monitored = monitored || role == FolderRoles.Inbox,
        };

    private sealed record Seed(
        long Account, long Folder, string FromName, string FromAddress, string Subject, string Body,
        double AgeHours, bool Seen = true, bool Flagged = false, string[]? Attachments = null,
        bool Remote = false, string? Html = null, bool Answered = false, bool Important = false,
        bool TextOnly = false, string? Cc = null, string? To = null);

    public static List<DemoMessage> Messages()
    {
        var prevMonth = DateTime.Today.AddMonths(-1);
        var prevMonthName = It.DateTimeFormat.GetMonthName(prevMonth.Month);
        var monthName = It.DateTimeFormat.GetMonthName(DateTime.Today.Month);
        var nextYear = DateTime.Today.Year + 1;

        var seeds = new List<Seed>
        {
            // ---- Gmail · Posta in arrivo
            new(GmailId, 101, "Giulia Romano", "giulia.romano@studioromano.example", "Riunione di progetto spostata a giovedì",
                "Ciao Marco,\n\nti scrivo per avvisarti che la riunione di avanzamento del progetto Aurora è stata spostata a giovedì alle 10:30, sempre in sala Verdi.\n\nPorta per favore la bozza del piano di rilascio: vorrei ripercorrere insieme le date di dicembre e capire se riusciamo ad anticipare la beta.\n\nGrazie e a presto,\nGiulia",
                0.4, Seen: false, Flagged: true, Important: true),
            new(GmailId, 101, "ShopItalia", "ordini@shopitalia.example", "Il tuo ordine #402-1187733 è stato spedito",
                "", 1.2, Seen: false, Remote: true, Html: OrderHtml()),
            new(GmailId, 101, "Luca Ferrari", "luca.ferrari@example.com", "Foto della gita al lago di Garda 📸",
                "Ehi Marco!\n\nEcco le foto di domenica: la luce sul lago era incredibile. Ne ho scelte tre, le altre te le passo su chiavetta quando ci vediamo.\n\nLa prossima volta saliamo fino a Malcesine con la funivia?\n\nLuca",
                3.5, Seen: false, Attachments: ["garda-01.png", "garda-02.png", "garda-03.png"]),
            new(GmailId, 101, "Banca Aurora", "notifiche@bancaaurora.example", $"Estratto conto di {prevMonthName} disponibile",
                $"Gentile cliente,\n\nl'estratto conto del mese di {prevMonthName} relativo al conto corrente n. ****4821 è disponibile in allegato e nella tua area riservata.\n\nTi ricordiamo che Banca Aurora non ti chiederà mai password o codici via e-mail.\n\nCordiali saluti,\nServizio Clienti Banca Aurora",
                5, Attachments: [$"estratto-conto-{prevMonthName}.pdf"]),
            new(GmailId, 101, "Il Gusto della Settimana", "newsletter@gustosettimana.example", "5 ricette d'autunno con la zucca 🎃",
                "", 7, Seen: false, Remote: true, Html: RecipesHtml()),
            new(GmailId, 101, "Sara Conti", "sara.conti@example.org", "Cena di sabato?",
                "Ciao Marco,\n\nsabato sera siamo da noi con Paolo e Francesca: ci sei? Pensavamo a una pizza fatta in casa verso le 20.\n\nFammi sapere entro giovedì così mi organizzo con l'impasto!\n\nSara",
                26, Answered: true),
            new(GmailId, 101, "TrenoRapido", "biglietti@trenorapido.example", "Il tuo biglietto: Milano Centrale → Roma Termini",
                "Grazie per aver viaggiato con TrenoRapido.\n\nPartenza: Milano Centrale, ore 07:10\nArrivo: Roma Termini, ore 10:20\nCarrozza 6, posto 8D\n\nIn allegato trovi il biglietto in PDF e l'evento da aggiungere al calendario. Presenta il codice QR a bordo.\n\nBuon viaggio!",
                28, Flagged: true, Attachments: ["biglietto-MI-RM.pdf", "viaggio.ics"], Important: true),
            new(GmailId, 101, "CodeHub", "notifications@codehub.example", "[nmmail] Nuova issue #42: notifiche duplicate dopo il resume",
                "@marco-bianchi ha aperto una nuova issue in nmmail/nmmail:\n\n> Dopo il resume da Modern Standby arrivano due notifiche per la stessa mail.\n> Passi per riprodurre:\n> 1. sospendere il portatile con una IDLE attiva\n> 2. inviare una mail all'account\n> 3. riattivare il PC\n\nRispondi a questa e-mail o visualizza la issue su CodeHub.",
                30, Seen: false, TextOnly: true),
            new(GmailId, 101, "Francesca Ricci", "francesca.ricci@example.com", "Compleanno di Anna 🎂 — regalo di gruppo",
                "Ciao a tutti!\n\nPer il compleanno di Anna pensavamo a un buono per il corso di ceramica che desidera da mesi. Sono 25 euro a testa.\n\nChi partecipa mi scrive entro venerdì? Poi raccolgo io le quote.\n\nUn abbraccio,\nFrancesca",
                50, Cc: "sara.conti@example.org, luca.ferrari@example.com"),
            new(GmailId, 101, "Agenzia Viaggi Orizzonti", "prenotazioni@orizzontiviaggi.example", "Conferma prenotazione: Hotel Miradouro, Lisbona",
                "Gentile Marco Bianchi,\n\nla tua prenotazione è confermata.\n\nHotel Miradouro ****, Lisbona\nCheck-in: venerdì dalle 14:00 — Check-out: lunedì entro le 11:00\nCamera doppia con vista sul Tago, colazione inclusa.\n\nIn allegato il voucher da presentare alla reception.\n\nBuon viaggio dallo staff di Orizzonti",
                75, Attachments: ["voucher-hotel-lisbona.pdf"]),
            new(GmailId, 101, "Palestra FitZone", "info@fitzone.example", "Il tuo abbonamento scade tra 7 giorni",
                "Ciao Marco,\n\nil tuo abbonamento annuale scade tra 7 giorni. Rinnova entro la scadenza per bloccare il prezzo attuale e ricevere un mese di corsi di gruppo in omaggio.\n\nTi aspettiamo in sala!\nLo staff FitZone",
                100),
            new(GmailId, 101, "Comune di Valfiorita", "tributi@comune.valfiorita.example", "Promemoria: scadenza seconda rata TARI",
                "Si ricorda che il 2 dicembre scade il pagamento della seconda rata della tassa sui rifiuti (TARI).\n\nIl modello di pagamento è disponibile nello sportello online del Comune.\n\nUfficio Tributi — Comune di Valfiorita",
                9 * 24),
            new(GmailId, 101, "Luca Ferrari", "luca.ferrari@example.com", "Re: Calcetto giovedì",
                "Ci sono! Porto io il pallone nuovo.\n\nGiovedì alle 21:00, campo 2.\n\n> Allora confermiamo per giovedì?\n> Marco",
                12 * 24, TextOnly: true),
            new(GmailId, 101, "Assicurazioni Sereno", "clienti@assicurazionisereno.example", "Rinnovo polizza auto: il tuo preventivo",
                "Gentile cliente,\n\nla polizza RC auto della tua vettura targata AB123CD scade il prossimo mese. In allegato trovi il preventivo di rinnovo con le nuove garanzie opzionali (cristalli e assistenza stradale).\n\nPer qualsiasi dubbio rispondi a questa e-mail.\n\nAssicurazioni Sereno",
                16 * 24, Attachments: ["preventivo-rc-auto.pdf"]),
            new(GmailId, 101, "LinkUp", "notifiche@linkup.example", "Hai 3 nuove richieste di collegamento",
                "Ciao Marco, hai 3 nuove richieste di collegamento in attesa, tra cui Chiara Lombardi (Product Manager presso Aurora Software).\n\nAccedi a LinkUp per rispondere.",
                40 * 24),
            new(GmailId, 101, "Fibra Veloce", "assistenza@fibraveloce.example", "Intervento tecnico programmato nella tua zona",
                "Gentile cliente,\n\nmartedì dalle 02:00 alle 05:00 effettueremo lavori di potenziamento della rete nella tua zona: il servizio internet potrebbe subire brevi interruzioni.\n\nCi scusiamo per il disagio.\nAssistenza Fibra Veloce",
                70 * 24),

            // ---- Gmail · other folders
            new(GmailId, 104, OwnName, GmailAddress, "Re: Cena di sabato?",
                "Ciao Sara,\n\ncontate su di me! Porto il dolce: tiramisù va bene?\n\nMarco", 25, To: "sara.conti@example.org"),
            new(GmailId, 104, OwnName, GmailAddress, "Calcetto giovedì",
                "Ragazzi, allora confermiamo per giovedì alle 21?\n\nMarco", 12.5 * 24, To: "luca.ferrari@example.com"),
            new(GmailId, 109, "Davide Moretti", "d.moretti@fornituregalli.example", $"Bozza contratto fornitura {nextYear}",
                $"Buongiorno Marco,\n\nin allegato la bozza del contratto di fornitura per il {nextYear}. Le modifiche rispetto all'anno scorso sono evidenziate in giallo (articoli 4 e 9).\n\nResto a disposizione per una call.\n\nCordiali saluti,\nDavide Moretti\nForniture Galli S.r.l.",
                6 * 24, Attachments: ["bozza-contratto.pdf"]),
            new(GmailId, 110, "Chiara Lombardi", "chiara.lombardi@aurorasoftware.example", "Slide per la presentazione di lunedì",
                "Ciao Marco,\n\nti giro le slide aggiornate per la presentazione di lunedì: ho aggiunto i grafici sulle prestazioni e tolto la parte sui costi.\n\nDammi un feedback quando riesci!\nChiara",
                3 * 24, Seen: false, Attachments: ["presentazione-aurora.pdf"]),
            new(GmailId, 111, "Volare Low Cost", "checkin@volarelowcost.example", "Carta d'imbarco: Milano Malpensa → Lisbona",
                "La tua carta d'imbarco è pronta.\n\nVolo VL 2381 — Milano Malpensa (T2) → Lisbona\nImbarco: ore 06:15 — Gate chiude alle 06:35\nPosto: 14C\n\nBuon viaggio!",
                20 * 24, Attachments: ["carta-imbarco.pdf"]),
            new(GmailId, 107, "Premi Esclusivi", "vincite@premi-esclusivi.example", "Congratulazioni! Hai vinto uno smartphone!!!",
                "Sei il nostro visitatore numero 1.000.000! Clicca subito per ritirare il tuo premio prima che scada.",
                2 * 24, Seen: false),
            new(GmailId, 108, "Offerte Lampo", "promo@offertelampo.example", "Solo oggi: -70% su tutto",
                "Saldi pazzi, solo per oggi! Approfitta degli sconti fino al 70% su migliaia di articoli.",
                4 * 24),

            // ---- Outlook · Posta in arrivo
            new(OutlookId, 201, "Elena Galli", "elena.galli@aurorasoftware.example", $"Piano ferie {nextYear}",
                $"Ciao a tutti,\n\nin allegato il file con il piano ferie {nextYear}. Vi chiedo di indicare i periodi preferiti entro la fine del mese, così possiamo garantire la copertura del supporto clienti.\n\nGrazie!\nElena — Risorse Umane",
                0.8, Seen: false, Attachments: [$"piano-ferie-{nextYear}.csv"]),
            new(OutlookId, 201, "IT Helpdesk", "helpdesk@aurorasoftware.example", "Manutenzione programmata server di posta — sabato ore 22:00",
                "Sabato dalle 22:00 alle 23:30 il server di posta aziendale sarà in manutenzione per l'aggiornamento di sicurezza mensile.\n\nDurante l'intervento l'invio e la ricezione potrebbero subire ritardi. Non è richiesta alcuna azione da parte vostra.\n\nIT Helpdesk",
                2.5, Seen: false),
            new(OutlookId, 201, "Roberto Esposito", "roberto.esposito@aurorasoftware.example", "Pranzo di team venerdì 🍕",
                "Ciao squadra!\n\nVenerdì festeggiamo il rilascio della 2.4 con un pranzo alla pizzeria qui sotto, ore 13. Offre l'azienda!\n\nRoberto",
                4),
            new(OutlookId, 201, "Sicurezza Account", "sicurezza@account-notify.example", "Nuovo accesso al tuo account da Windows",
                "Abbiamo rilevato un nuovo accesso al tuo account marco.bianchi@example.org.\n\nDispositivo: Windows 11 — Milano, Italia\n\nSe sei stato tu, puoi ignorare questo messaggio. In caso contrario, modifica subito la password.",
                24.5),
            new(OutlookId, 201, "Alessandro Marino", "a.marino@edilmarino.example", "Re: Preventivo ristrutturazione bagno",
                "Buongiorno Marco,\n\ncome d'accordo le invio il preventivo dettagliato per la ristrutturazione del bagno: rifacimento impianti, sostituzione sanitari e piastrelle effetto pietra.\n\nI lavori potrebbero partire a gennaio, con una durata stimata di tre settimane.\n\nCordiali saluti,\nAlessandro Marino\nEdil Marino",
                29, Seen: false, Attachments: ["preventivo-bagno.pdf"]),
            new(OutlookId, 201, "Ufficio Acquisti", "acquisti@aurorasoftware.example", "Ordine di acquisto PO-2291 approvato",
                "L'ordine di acquisto PO-2291 (3 monitor 27\" e 3 docking station) è stato approvato. Consegna prevista entro 10 giorni lavorativi.\n\nUfficio Acquisti",
                52),
            new(OutlookId, 201, "Tech Settimanale", "redazione@techsettimanale.example", "Windows, IA e privacy: le notizie della settimana",
                "", 80, Remote: true, Html: TechNewsHtml()),
            new(OutlookId, 201, "Chiara Lombardi", "chiara.lombardi@aurorasoftware.example", "Revisione documento requisiti v3",
                "Ciao Marco,\n\nho completato la revisione del documento dei requisiti (v3). Restano aperti due punti sulla sincronizzazione offline: li segno come commenti nel PDF.\n\nNe parliamo alla riunione di giovedì?\n\nChiara",
                6 * 24, Flagged: true, Attachments: ["requisiti-v3.pdf"], Cc: "giulia.romano@studioromano.example"),
            new(OutlookId, 201, "Elena Galli", "elena.galli@aurorasoftware.example", "Benvenuto in Aurora Software!",
                "Caro Marco,\n\nbenvenuto in Aurora Software! Lunedì alle 9:00 ti aspettiamo in reception per il badge e il giro degli uffici.\n\nA presto,\nElena",
                45 * 24),

            // ---- Outlook · other folders
            new(OutlookId, 207, "Energia Chiara", "bollette@energiachiara.example", $"La tua bolletta di {monthName}",
                $"Gentile cliente,\n\nè disponibile la bolletta di {monthName} per la fornitura di energia elettrica. Importo: 64,30 euro, addebito in conto il giorno 5.\n\nEnergia Chiara",
                3 * 24, Seen: false, Attachments: [$"bolletta-{monthName}.pdf"]),
            new(OutlookId, 207, "Davide Moretti", "d.moretti@fornituregalli.example", $"Fattura n. 118/{DateTime.Today.Year}",
                "Buongiorno,\n\nin allegato la fattura relativa alla fornitura di ottobre.\n\nCordiali saluti,\nAmministrazione Forniture Galli",
                13 * 24, Attachments: [$"fattura-118-{DateTime.Today.Year}.pdf"]),
            new(OutlookId, 208, "Energia Chiara", "bollette@energiachiara.example", "La tua bolletta di dicembre 2025",
                "È disponibile la bolletta di dicembre 2025. Importo: 71,90 euro.", 330 * 24, Attachments: ["bolletta-dicembre-2025.pdf"]),
            new(OutlookId, 204, "Roberto Esposito", "roberto.esposito@aurorasoftware.example", "Foto evento aziendale",
                "Ecco la foto di gruppo dell'evento di fine trimestre. Bellissima giornata!\n\nRoberto",
                25 * 24, Attachments: ["evento-aziendale.png"]),
            new(OutlookId, 204, "Elena Galli", "elena.galli@aurorasoftware.example", "Modulo rimborso spese",
                "Ciao Marco,\n\nper le trasferte usa il modulo in allegato e invialo all'amministrazione entro il 5 del mese successivo.\n\nElena",
                35 * 24, Attachments: ["modulo-rimborso.pdf"]),
            new(OutlookId, 202, OwnName, OutlookAddress, "Preventivo ristrutturazione bagno",
                "Buongiorno,\n\nsarei interessato a un preventivo per la ristrutturazione completa di un bagno di circa 6 mq.\n\nGrazie,\nMarco Bianchi",
                2 * 24, To: "a.marino@edilmarino.example"),
            new(OutlookId, 202, OwnName, OutlookAddress, "Re: Modulo rimborso spese",
                "Grazie Elena, ricevuto!\n\nMarco", 34 * 24, To: "elena.galli@aurorasoftware.example"),
            new(OutlookId, 203, OwnName, OutlookAddress, "Proposta nuova architettura notifiche",
                "Bozza: separare il demone di sincronizzazione dalla UI, con connessione IDLE dedicata per ogni account…", 24,
                To: "chiara.lombardi@aurorasoftware.example"),
            new(OutlookId, 205, "Corriere Express", "tracking@corriere-express.example", "Il tuo pacco è in giacenza: conferma i dati",
                "Il tuo pacco non è stato consegnato. Conferma i tuoi dati e paga 1,99 euro di spese di riconsegna entro 24 ore.",
                36, Seen: false),
        };

        var now = DateTimeOffset.Now;
        var result = new List<DemoMessage>(seeds.Count);
        long id = 1001;
        foreach (var seed in seeds)
        {
            result.Add(Build(id++, seed, now));
        }
        return result;
    }

    private static DemoMessage Build(long id, Seed s, DateTimeOffset now)
    {
        var own = s.Account == GmailId ? new Addr { Name = OwnName, Address = GmailAddress } : new Addr { Name = OwnName, Address = OutlookAddress };
        var to = s.To is null ? [own] : ParseList(s.To);
        var cc = s.Cc is null ? Array.Empty<Addr>() : ParseList(s.Cc);
        var body = s.Html ?? TextToHtml(s.Body);
        var text = s.Html is null ? s.Body : HtmlToText(s.Html);
        var attachments = (s.Attachments ?? []).Select((name, i) => new Attachment
        {
            PartId = i + 2,
            Filename = name,
            Mime = MimeFor(name),
            Size = SizeFor(name, id + i),
        }).ToArray();

        var detail = new MessageDetail
        {
            Id = id,
            AccountId = s.Account,
            FolderId = s.Folder,
            Subject = s.Subject,
            From = new Addr { Name = s.FromName, Address = s.FromAddress },
            To = to,
            Cc = cc,
            Date = now.AddHours(-s.AgeHours).ToUnixTimeMilliseconds(),
            Snippet = Snippet(text),
            Seen = s.Seen,
            Flagged = s.Flagged,
            Answered = s.Answered,
            HasAttachments = attachments.Length > 0,
            Size = 2400 + body.Length + attachments.Sum(a => a.Size),
            MessageIdHeader = $"<{id}.{s.Account}@demo.nmmail>",
            Text = text,
            Attachments = attachments,
        };
        return new DemoMessage { Detail = detail, BodyHtml = body, RemoteImages = s.Remote, Important = s.Important, TextOnly = s.TextOnly };
    }

    // ------------------------------------------------------------------ incoming mail pool (every ~30 s)

    private static readonly (long Account, string Name, string Address, string Subject, string Body)[] s_incoming =
    [
        (GmailId, "Sara Conti", "sara.conti@example.org", "Allora sabato confermato? 🍕", "Ciao Marco! Ho comprato tutto per l'impasto, ci vediamo sabato alle 20. Porta solo il buonumore!\n\nSara"),
        (OutlookId, "Chiara Lombardi", "chiara.lombardi@aurorasoftware.example", "Domanda veloce sui requisiti", "Ciao Marco,\n\nla sincronizzazione delle cartelle personalizzate rientra nella prima release o la spostiamo alla 2.5?\n\nGrazie,\nChiara"),
        (GmailId, "Luca Ferrari", "luca.ferrari@example.com", "Funivia Malcesine — orari", "Ho controllato: la prima corsa è alle 8:00, l'ultima discesa alle 17:45. Ci organizziamo per domenica prossima?"),
        (OutlookId, "IT Helpdesk", "helpdesk@aurorasoftware.example", "Manutenzione completata", "La manutenzione del server di posta è stata completata con successo. Tutti i servizi sono di nuovo disponibili.\n\nIT Helpdesk"),
        (GmailId, "ShopItalia", "ordini@shopitalia.example", "Il tuo pacco arriva oggi", "Il corriere consegnerà il tuo ordine #402-1187733 oggi tra le 14:00 e le 18:00."),
        (OutlookId, "Giulia Romano", "giulia.romano@studioromano.example", "Verbale riunione", "Ciao,\n\nin allegato trovate il verbale della riunione di oggi con le azioni concordate.\n\nGiulia"),
    ];

    public static DemoMessage Incoming(long id, int sequence, long folderId)
    {
        var (account, name, address, subject, body) = s_incoming[sequence % s_incoming.Length];
        return Build(id, new Seed(account, folderId, name, address, subject, body, 0, Seen: false,
            Attachments: subject == "Verbale riunione" ? ["verbale-riunione.pdf"] : null), DateTimeOffset.Now);
    }

    public static long IncomingAccount(int sequence) => s_incoming[sequence % s_incoming.Length].Account;

    // ------------------------------------------------------------------ "older than the cache" messages for messages.loadOlder

    public static List<DemoMessage> Older(long startId, long accountId, long folderId, long beforeDate)
    {
        var baseDate = DateTimeOffset.FromUnixTimeMilliseconds(beforeDate);
        var list = new List<DemoMessage>();
        string[] subjects =
        [
            "Newsletter di agosto: novità e promozioni",
            "Conferma iscrizione al corso di fotografia",
            "Ricevuta di pagamento n. 7781",
            "Auguri di buon Ferragosto!",
            "Aggiornamento termini di servizio",
        ];
        for (var i = 0; i < subjects.Length; i++)
        {
            var seed = new Seed(accountId, folderId, i % 2 == 0 ? "Servizio Clienti" : "Paolo Neri",
                i % 2 == 0 ? "info@servizi.example" : "paolo.neri@example.com", subjects[i],
                "Messaggio meno recente scaricato dal server su richiesta (oltre la finestra della cache locale).", 0);
            var m = Build(startId + i, seed, baseDate.AddDays(-(7 * (i + 1))));
            list.Add(m);
        }
        return list;
    }

    // ------------------------------------------------------------------ HTML helpers

    public static string Document(string bodyHtml, bool allowRemote)
    {
        var img = allowRemote ? "data: cid: https: http:" : "data: cid:";
        return "<!DOCTYPE html><html><head><meta charset=\"utf-8\">" +
               $"<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src {img}; style-src 'unsafe-inline'; font-src data:; form-action 'none'\">" +
               "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">" +
               "<style>body{font-family:'Segoe UI',Arial,sans-serif;font-size:14px;line-height:1.55;color:#1b1b1b;margin:0;padding:20px 24px;word-wrap:break-word}" +
               "a{color:#0f6cbd}img{max-width:100%;height:auto}p{margin:0 0 12px}" +
               "blockquote{margin:0 0 0 .8ex;border-left:2px solid #c8c8c8;padding-left:1ex;color:#555}pre{white-space:pre-wrap;font-family:Consolas,monospace;font-size:13px}</style>" +
               "</head><body>" + bodyHtml + "</body></html>";
    }

    public static string TextToHtml(string text)
    {
        var sb = new StringBuilder();
        foreach (var paragraph in text.Replace("\r\n", "\n").Split("\n\n"))
        {
            var encoded = WebUtility.HtmlEncode(paragraph).Replace("\n", "<br>");
            sb.Append(paragraph.StartsWith('>') ? "<blockquote>" + encoded.Replace("&gt; ", "") + "</blockquote>" : "<p>" + encoded + "</p>");
        }
        return sb.ToString();
    }

    public static string HtmlToText(string html)
    {
        var sb = new StringBuilder(html.Length / 2);
        var inTag = false;
        var inStyle = false;
        for (var i = 0; i < html.Length; i++)
        {
            var c = html[i];
            if (c == '<')
            {
                inTag = true;
                if (string.Compare(html, i, "<style", 0, 6, StringComparison.OrdinalIgnoreCase) == 0) inStyle = true;
                if (string.Compare(html, i, "</style", 0, 7, StringComparison.OrdinalIgnoreCase) == 0) inStyle = false;
                if (string.Compare(html, i, "<br", 0, 3, StringComparison.OrdinalIgnoreCase) == 0 ||
                    string.Compare(html, i, "</p", 0, 3, StringComparison.OrdinalIgnoreCase) == 0 ||
                    string.Compare(html, i, "</h", 0, 3, StringComparison.OrdinalIgnoreCase) == 0 ||
                    string.Compare(html, i, "</tr", 0, 4, StringComparison.OrdinalIgnoreCase) == 0)
                {
                    sb.Append('\n');
                }
                continue;
            }
            if (c == '>')
            {
                inTag = false;
                continue;
            }
            if (!inTag && !inStyle) sb.Append(c);
        }
        var text = WebUtility.HtmlDecode(sb.ToString());
        var lines = text.Split('\n').Select(l => l.Trim()).Where(l => l.Length > 0);
        return string.Join("\n", lines);
    }

    public static string Snippet(string text)
    {
        var flat = string.Join(' ', text.Split(['\n', '\r', '\t'], StringSplitOptions.RemoveEmptyEntries).Select(s => s.Trim()));
        return flat.Length <= 160 ? flat : flat[..160].TrimEnd() + "…";
    }

    public static Addr[] ParseList(string text) =>
        [.. text.Split([',', ';'], StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries).Select(Addr.Parse)];

    private static string OrderHtml() => """
        <table role="presentation" width="100%" cellpadding="0" cellspacing="0" style="max-width:600px;margin:0 auto;font-family:'Segoe UI',Arial,sans-serif">
          <tr><td style="background:#e8581c;padding:18px 24px;border-radius:8px 8px 0 0">
            <img src="https://shopitalia.example/logo-white.png" alt="ShopItalia" width="140" style="display:block;color:#fff;font-size:22px;font-weight:600">
          </td></tr>
          <tr><td style="background:#ffffff;padding:24px;border:1px solid #eee;border-top:0">
            <h2 style="margin:0 0 8px;font-size:20px;color:#222">Buone notizie, il tuo ordine è in viaggio! 🚚</h2>
            <p style="color:#555">Ordine <b>#402-1187733</b> &middot; consegna prevista <b>giovedì</b></p>
            <table width="100%" cellpadding="8" style="border-collapse:collapse;margin:16px 0">
              <tr style="border-bottom:1px solid #eee">
                <td width="64"><img src="https://shopitalia.example/img/cuffie.jpg" alt="Cuffie" width="56" height="56"></td>
                <td>Cuffie wireless con cancellazione del rumore<br><span style="color:#888;font-size:12px">Colore: nero opaco &middot; Quantità: 1</span></td>
                <td align="right"><b>129,90 €</b></td>
              </tr>
              <tr><td></td><td>Spedizione</td><td align="right">Gratuita</td></tr>
              <tr><td></td><td><b>Totale</b></td><td align="right"><b>129,90 €</b></td></tr>
            </table>
            <p><a href="https://shopitalia.example/tracking/402-1187733" style="display:inline-block;background:#e8581c;color:#fff;text-decoration:none;padding:10px 18px;border-radius:4px;font-weight:600">Traccia la spedizione</a></p>
            <p style="color:#888;font-size:12px;margin-top:24px">Hai ricevuto questa e-mail perché hai effettuato un acquisto su ShopItalia. <a href="https://shopitalia.example/privacy" style="color:#888">Informativa privacy</a></p>
          </td></tr>
        </table>
        """;

    private static string RecipesHtml() => """
        <div style="max-width:620px;margin:0 auto;font-family:Georgia,'Times New Roman',serif;color:#3a2e22">
          <div style="background:#f4ead8;padding:28px 24px;text-align:center;border-radius:10px 10px 0 0">
            <div style="font-size:12px;letter-spacing:3px;color:#a0522d">NEWSLETTER SETTIMANALE</div>
            <div style="font-size:30px;font-weight:bold;margin-top:6px">Il Gusto della Settimana</div>
          </div>
          <img src="https://gustosettimana.example/img/zucca-header.jpg" alt="Zucche d'autunno" width="620" style="display:block;width:100%">
          <div style="padding:20px 24px;background:#fffaf2">
            <h2 style="color:#a0522d;font-size:22px;margin:0 0 8px">5 ricette d'autunno con la zucca</h2>
            <p>La zucca è la regina della stagione: dolce, versatile e perfetta dall'antipasto al dolce. Ecco le nostre preferite.</p>
            <ol style="line-height:1.8">
              <li><b>Risotto zucca e salvia</b> — cremoso, con burro nocciola.</li>
              <li><b>Vellutata speziata</b> — con zenzero e latte di cocco.</li>
              <li><b>Gnocchi di zucca</b> — al burro e amaretti.</li>
              <li><b>Zucca al forno</b> — con rosmarino e scaglie di parmigiano.</li>
              <li><b>Torta soffice</b> — alla zucca e cannella.</li>
            </ol>
            <p style="text-align:center;margin:24px 0"><a href="https://gustosettimana.example/ricette/zucca" style="background:#a0522d;color:#fff;padding:12px 22px;border-radius:24px;text-decoration:none;font-family:'Segoe UI',Arial,sans-serif">Leggi tutte le ricette</a></p>
            <img src="https://gustosettimana.example/track/open.gif" width="1" height="1" alt="">
          </div>
          <p style="font-size:11px;color:#998;text-align:center;font-family:'Segoe UI',Arial,sans-serif">Ricevi questa newsletter perché sei iscritto a Il Gusto della Settimana. <a href="https://gustosettimana.example/unsubscribe" style="color:#998">Annulla iscrizione</a></p>
        </div>
        """;

    private static string TechNewsHtml() => """
        <div style="max-width:640px;margin:0 auto;font-family:'Segoe UI',Arial,sans-serif;color:#1f2328">
          <div style="background:#101827;color:#fff;padding:22px 24px;border-radius:8px 8px 0 0">
            <div style="font-size:24px;font-weight:600">Tech Settimanale</div>
            <div style="opacity:.75;font-size:13px">Le notizie che contano, ogni venerdì</div>
          </div>
          <div style="padding:8px 24px 24px;background:#fff;border:1px solid #e5e7eb;border-top:0">
            <h3 style="margin:20px 0 6px">💻 Windows: nuove funzioni per l'efficienza energetica</h3>
            <p style="margin:0;color:#444">Le app in background consumano meno grazie alla modalità efficienza: ecco come verificarlo dal Task Manager.</p>
            <img src="https://techsettimanale.example/img/windows.jpg" alt="Windows" width="592" style="margin:12px 0;border-radius:6px">
            <h3 style="margin:20px 0 6px">🤖 IA sul dispositivo: cosa cambia per la privacy</h3>
            <p style="margin:0;color:#444">Sempre più modelli girano in locale: meno dati in cloud, ma servono più memoria e NPU dedicate.</p>
            <h3 style="margin:20px 0 6px">🔒 Pixel di tracciamento nelle e-mail</h3>
            <p style="margin:0;color:#444">Perché i client moderni bloccano le immagini remote per impostazione predefinita, e come riconoscere i tracker.</p>
            <p style="margin-top:20px"><a href="https://techsettimanale.example/numero/212">Leggi il numero completo →</a></p>
          </div>
        </div>
        """;

    // ------------------------------------------------------------------ attachments

    public static string MimeFor(string name) => Path.GetExtension(name).ToLowerInvariant() switch
    {
        ".pdf" => "application/pdf",
        ".png" => "image/png",
        ".jpg" or ".jpeg" => "image/jpeg",
        ".csv" => "text/csv",
        ".ics" => "text/calendar",
        ".txt" => "text/plain",
        _ => "application/octet-stream",
    };

    private static long SizeFor(string name, long seed) => Path.GetExtension(name).ToLowerInvariant() switch
    {
        ".png" => 1_200_000 + seed % 7 * 113_000,
        ".pdf" => 85_000 + seed % 9 * 21_000,
        ".ics" => 1_800,
        ".csv" => 6_400,
        _ => 20_000,
    };

    /// <summary>Generates a small but valid file for an attachment (so "open" works in demo mode).</summary>
    public static byte[] AttachmentContent(DemoMessage message, Attachment attachment)
    {
        var ext = Path.GetExtension(attachment.Filename).ToLowerInvariant();
        return ext switch
        {
            ".png" => Png(640, 400, (int)(attachment.PartId + message.Detail.Id)),
            ".csv" => Encoding.UTF8.GetPreamble().Concat(Encoding.UTF8.GetBytes(
                "Dipendente;Gennaio;Febbraio;Marzo;Aprile;Maggio;Giugno;Luglio;Agosto\n" +
                "Marco Bianchi;;;;;;;2 settimane;1 settimana\nChiara Lombardi;;1 settimana;;;;;;3 settimane\n" +
                "Roberto Esposito;;;;1 settimana;;;3 settimane;\n")).ToArray(),
            ".ics" => Encoding.UTF8.GetBytes(Ics(message)),
            ".pdf" => Pdf(message.Detail.Subject, attachment.Filename),
            _ => Encoding.UTF8.GetBytes("NM Mail demo attachment\r\n"),
        };
    }

    private static string Ics(DemoMessage message)
    {
        var start = DateTime.UtcNow.Date.AddDays(3).AddHours(6).AddMinutes(10);
        return "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//NM Mail//Demo//IT\r\nBEGIN:VEVENT\r\n" +
               $"UID:{message.Detail.Id}@demo.nmmail\r\nDTSTAMP:{DateTime.UtcNow:yyyyMMddTHHmmssZ}\r\n" +
               $"DTSTART:{start:yyyyMMddTHHmmssZ}\r\nDTEND:{start.AddHours(3).AddMinutes(10):yyyyMMddTHHmmssZ}\r\n" +
               "SUMMARY:Treno Milano Centrale - Roma Termini\r\nLOCATION:Milano Centrale\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    }

    private static byte[] Pdf(string subject, string filename)
    {
        static string Esc(string s)
        {
            var sb = new StringBuilder();
            foreach (var c in s)
            {
                if (c is '(' or ')' or '\\') sb.Append('\\').Append(c);
                else if (c < 256) sb.Append(c);
                else sb.Append('?');
            }
            return sb.ToString();
        }

        var lines = new[]
        {
            "Documento dimostrativo generato da NM Mail (--demo).",
            "",
            "Oggetto del messaggio: " + subject,
            "Allegato: " + filename,
            "",
            "In modalita demo gli allegati sono generati localmente:",
            "nessun dato viene scaricato da Internet.",
        };
        var content = new StringBuilder();
        content.Append("0.06 0.42 0.74 rg 0 792 595 50 re f\n");
        content.Append("BT /F2 20 Tf 1 1 1 rg 40 808 Td (NM Mail) Tj ET\n");
        content.Append("BT /F2 16 Tf 0 0 0 rg 40 740 Td (").Append(Esc(Path.GetFileNameWithoutExtension(filename))).Append(") Tj ET\n");
        var y = 705;
        foreach (var line in lines)
        {
            content.Append("BT /F1 11 Tf 0.2 0.2 0.2 rg 40 ").Append(y).Append(" Td (").Append(Esc(line)).Append(") Tj ET\n");
            y -= 17;
        }

        var latin1 = Encoding.Latin1;
        var stream = latin1.GetBytes(content.ToString());
        var objects = new[]
        {
            "<< /Type /Catalog /Pages 2 0 R >>",
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 5 0 R /F2 6 0 R >> >> /Contents 4 0 R >>",
            null, // content stream
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>",
        };

        using var ms = new MemoryStream();
        void Write(string s) => ms.Write(latin1.GetBytes(s));
        Write("%PDF-1.4\n%âãÏÓ\n");
        var offsets = new long[objects.Length];
        for (var i = 0; i < objects.Length; i++)
        {
            offsets[i] = ms.Position;
            Write($"{i + 1} 0 obj\n");
            if (objects[i] is { } obj)
            {
                Write(obj + "\n");
            }
            else
            {
                Write($"<< /Length {stream.Length} >>\nstream\n");
                ms.Write(stream);
                Write("\nendstream\n");
            }
            Write("endobj\n");
        }
        var xref = ms.Position;
        Write($"xref\n0 {objects.Length + 1}\n0000000000 65535 f \n");
        foreach (var offset in offsets) Write($"{offset:D10} 00000 n \n");
        Write($"trailer\n<< /Size {objects.Length + 1} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n");
        return ms.ToArray();
    }

    /// <summary>A procedural "lake landscape" PNG.</summary>
    private static byte[] Png(int width, int height, int seed)
    {
        var raw = new byte[(width * 3 + 1) * height];
        var phase = seed % 7 * 0.9;
        var warm = seed % 3 == 0;
        for (var y = 0; y < height; y++)
        {
            var row = y * (width * 3 + 1);
            raw[row] = 0;
            for (var x = 0; x < width; x++)
            {
                var t = y / (double)height;
                var ridge = 0.42 + 0.08 * Math.Sin(x / 57.0 + phase) + 0.04 * Math.Sin(x / 19.0 + phase * 2);
                byte r, g, b;
                if (t < ridge)
                {
                    var k = t / ridge;
                    r = (byte)(warm ? 250 - 60 * k : 120 + 90 * k);
                    g = (byte)(warm ? 190 + 20 * k : 175 + 50 * k);
                    b = (byte)(warm ? 140 + 80 * k : 235 + 15 * k);
                }
                else if (t < 0.58)
                {
                    var k = (t - ridge) / Math.Max(0.01, 0.58 - ridge);
                    r = (byte)(60 - 25 * k);
                    g = (byte)(95 - 30 * k);
                    b = (byte)(80 - 20 * k);
                }
                else
                {
                    var k = (t - 0.58) / 0.42;
                    var ripple = 8 * Math.Sin(x / 6.0 + y / 3.0 + phase);
                    r = (byte)Math.Clamp(40 + 30 * k + ripple, 0, 255);
                    g = (byte)Math.Clamp(110 + 40 * k + ripple, 0, 255);
                    b = (byte)Math.Clamp(150 + 50 * k + ripple, 0, 255);
                }
                var p = row + 1 + x * 3;
                raw[p] = r;
                raw[p + 1] = g;
                raw[p + 2] = b;
            }
        }

        using var ms = new MemoryStream();
        ms.Write([0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
        var ihdr = new byte[13];
        BinaryPrimitives.WriteInt32BigEndian(ihdr, width);
        BinaryPrimitives.WriteInt32BigEndian(ihdr.AsSpan(4), height);
        ihdr[8] = 8; // bit depth
        ihdr[9] = 2; // truecolor
        Chunk(ms, "IHDR", ihdr);
        using (var compressed = new MemoryStream())
        {
            using (var z = new ZLibStream(compressed, CompressionLevel.Fastest, leaveOpen: true)) z.Write(raw);
            Chunk(ms, "IDAT", compressed.ToArray());
        }
        Chunk(ms, "IEND", []);
        return ms.ToArray();
    }

    private static void Chunk(Stream s, string type, byte[] data)
    {
        Span<byte> len = stackalloc byte[4];
        BinaryPrimitives.WriteInt32BigEndian(len, data.Length);
        s.Write(len);
        var typeBytes = Encoding.ASCII.GetBytes(type);
        s.Write(typeBytes);
        s.Write(data);
        var crc = Crc32(typeBytes, data);
        BinaryPrimitives.WriteUInt32BigEndian(len, crc);
        s.Write(len);
    }

    private static uint[]? s_crcTable;

    private static uint Crc32(byte[] a, byte[] b)
    {
        var table = s_crcTable ??= BuildCrcTable();
        var crc = 0xFFFFFFFFu;
        foreach (var x in a) crc = table[(crc ^ x) & 0xFF] ^ (crc >> 8);
        foreach (var x in b) crc = table[(crc ^ x) & 0xFF] ^ (crc >> 8);
        return crc ^ 0xFFFFFFFFu;
    }

    private static uint[] BuildCrcTable()
    {
        var table = new uint[256];
        for (uint n = 0; n < 256; n++)
        {
            var c = n;
            for (var k = 0; k < 8; k++) c = (c & 1) != 0 ? 0xEDB88320u ^ (c >> 1) : c >> 1;
            table[n] = c;
        }
        return table;
    }
}
