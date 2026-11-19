//! Realistic Italian demo messages for manual development of the app.
//!
//! [`TestServer::seed_demo_messages`](crate::TestServer::seed_demo_messages) fills a
//! user's INBOX with about 25 messages (plain text, HTML newsletters, a PDF attachment,
//! an inline `cid:` image, a remote `https://` image, a conversation thread, Gmail
//! categories via `X-Test-Category`) spread over the last 60 days, plus a few messages
//! in Sent. [`demo_message`] generates the messages used by `--auto-deliver`.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use chrono::{DateTime, FixedOffset, Local, TimeDelta, Timelike};

use crate::store::{SpecialUse, Store, normalize_flags};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Folder {
    Inbox,
    Sent,
}

struct Demo {
    folder: Folder,
    date: DateTime<FixedOffset>,
    flags: &'static [&'static str],
    raw: Vec<u8>,
}

/// RFC 2047 encoding of a header phrase (only when it is not plain ASCII).
fn encode_word(text: &str) -> String {
    if text.is_ascii() {
        text.to_string()
    } else {
        format!("=?UTF-8?B?{}?=", BASE64.encode(text))
    }
}

/// `Name <address>` with the name encoded or quoted as needed.
fn address(name: &str, email: &str) -> String {
    if name.is_empty() {
        email.to_string()
    } else if !name.is_ascii() {
        format!("{} <{email}>", encode_word(name))
    } else if name.contains([',', '.', '"', '(', ')', ':', ';', '@']) {
        format!("\"{}\" <{email}>", name.replace('"', "\\\""))
    } else {
        format!("{name} <{email}>")
    }
}

fn crlf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', "\r\n")
}

fn quoted_printable(text: &str) -> String {
    let mut out = String::new();
    for line in text.replace("\r\n", "\n").split('\n') {
        let bytes = line.as_bytes();
        let mut width = 0;
        for (i, &b) in bytes.iter().enumerate() {
            let trailing_space = (b == b' ' || b == b'\t') && i + 1 == bytes.len();
            let encoded = if trailing_space || b == b'=' || (b < 32 && b != b'\t') || b > 126 {
                format!("={b:02X}")
            } else {
                char::from(b).to_string()
            };
            if width + encoded.len() > 75 {
                out.push_str("=\r\n");
                width = 0;
            }
            out.push_str(&encoded);
            width += encoded.len();
        }
        out.push_str("\r\n");
    }
    out
}

fn base64_lines(data: &[u8]) -> String {
    let encoded = BASE64.encode(data);
    let mut out = String::with_capacity(encoded.len() + encoded.len() / 76 * 2 + 2);
    for chunk in encoded.as_bytes().chunks(76) {
        out.push_str(&String::from_utf8_lossy(chunk));
        out.push_str("\r\n");
    }
    out
}

fn text_part(text: &str) -> String {
    format!(
        "Content-Type: text/plain; charset=UTF-8\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n{}",
        quoted_printable(text)
    )
}

fn html_part(html: &str) -> String {
    format!(
        "Content-Type: text/html; charset=UTF-8\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n{}",
        quoted_printable(html)
    )
}

fn multipart(subtype: &str, boundary: &str, parts: &[String]) -> String {
    let mut out = format!("Content-Type: multipart/{subtype}; boundary=\"{boundary}\"\r\n\r\n");
    for part in parts {
        out.push_str(&format!("--{boundary}\r\n{part}"));
        if !part.ends_with("\r\n") {
            out.push_str("\r\n");
        }
    }
    out.push_str(&format!("--{boundary}--\r\n"));
    out
}

struct Head<'a> {
    from: String,
    to: String,
    subject: &'a str,
    date: DateTime<FixedOffset>,
    message_id: String,
    extra: Vec<(&'a str, String)>,
}

impl Head<'_> {
    /// Full message: header block plus a MIME entity (its own headers, blank line, body).
    fn with_entity(&self, entity: &str) -> Vec<u8> {
        let mut out = format!(
            "From: {}\r\nTo: {}\r\nSubject: {}\r\nDate: {}\r\nMessage-ID: <{}>\r\n",
            self.from,
            self.to,
            encode_word(self.subject),
            self.date.to_rfc2822(),
            self.message_id
        );
        for (name, value) in &self.extra {
            out.push_str(&format!("{name}: {value}\r\n"));
        }
        out.push_str("MIME-Version: 1.0\r\n");
        out.push_str(entity);
        out.into_bytes()
    }

    fn plain(&self, text: &str) -> Vec<u8> {
        self.with_entity(&format!(
            "Content-Type: text/plain; charset=UTF-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n{}",
            crlf(text)
        ))
    }

    fn html(&self, text: &str, html: &str, boundary: &str) -> Vec<u8> {
        self.with_entity(&multipart(
            "alternative",
            boundary,
            &[text_part(text), html_part(html)],
        ))
    }
}

/// A minimal but valid one-page PDF showing `text` (ASCII, no parentheses).
fn tiny_pdf(text: &str) -> Vec<u8> {
    let stream = format!("BT /F1 16 Tf 30 90 Td ({text}) Tj ET");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 520 200] /Contents 4 0 R \
/Resources << /Font << /F1 5 0 R >> >> >>"
            .to_string(),
        format!(
            "<< /Length {} >>\nstream\n{stream}\nendstream",
            stream.len()
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = pdf.len();
    pdf.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    pdf
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + u32::from(byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

fn png_chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend_from_slice(&u32::try_from(data.len()).unwrap_or(0).to_be_bytes());
    let start = png.len();
    png.extend_from_slice(kind);
    png.extend_from_slice(data);
    let crc = crc32(&png[start..]);
    png.extend_from_slice(&crc.to_be_bytes());
}

/// An RGB PNG (uncompressed deflate blocks) drawn by `pixel(x, y)`.
fn png(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
    let mut scanlines = Vec::new();
    for y in 0..height {
        scanlines.push(0); // filter: none
        for x in 0..width {
            scanlines.extend_from_slice(&pixel(x, y));
        }
    }
    let mut zlib = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = scanlines.chunks(65_535).collect();
    for (i, block) in blocks.iter().enumerate() {
        zlib.push(u8::from(i + 1 == blocks.len()));
        let len = u16::try_from(block.len()).unwrap_or(u16::MAX);
        zlib.extend_from_slice(&len.to_le_bytes());
        zlib.extend_from_slice(&(!len).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    zlib.extend_from_slice(&adler32(&scanlines).to_be_bytes());
    let mut header = Vec::new();
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit RGB
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    png_chunk(&mut out, b"IHDR", &header);
    png_chunk(&mut out, b"IDAT", &zlib);
    png_chunk(&mut out, b"IEND", &[]);
    out
}

/// A small mountain landscape (sky, snowy peak, meadow).
fn mountain_png() -> Vec<u8> {
    png(96, 64, |x, y| {
        let distance = x.abs_diff(48);
        if y >= 50 {
            [76, 153, 0]
        } else if y >= 10 + distance {
            if y < 20 {
                [250, 250, 250]
            } else {
                [120, 110, 105]
            }
        } else {
            let shade = u8::try_from(y * 2).unwrap_or(0);
            [110 + shade / 2, 170 + shade / 3, 235]
        }
    })
}

fn demos(user: &str, now: DateTime<FixedOffset>) -> Vec<Demo> {
    let at = |days: i64, minutes: i64| now - TimeDelta::days(days) - TimeDelta::minutes(minutes);
    let me = address("", user);
    let head = |from: String, subject: &'static str, date: DateTime<FixedOffset>, id: &str| Head {
        from,
        to: me.clone(),
        subject,
        date,
        message_id: format!("{id}@nmmail.example"),
        extra: Vec::new(),
    };
    let mut demos = Vec::new();
    let mut push = |folder: Folder,
                    date: DateTime<FixedOffset>,
                    flags: &'static [&'static str],
                    raw: Vec<u8>| {
        demos.push(Demo {
            folder,
            date,
            flags,
            raw,
        });
    };

    // --- A conversation thread (Sent and INBOX) -------------------------------------
    let marco = address("Marco Rossi", "marco.rossi@example.com");
    let thread = [
        "seed-preventivo-1",
        "seed-preventivo-2",
        "seed-preventivo-3",
        "seed-preventivo-4",
    ]
    .map(|id| format!("<{id}@nmmail.example>"));
    let d = at(4, 300);
    let mut h = head(
        me.clone(),
        "Preventivo ristrutturazione cucina",
        d,
        "seed-preventivo-1",
    );
    h.to = marco.clone();
    push(
        Folder::Sent,
        d,
        &["\\Seen"],
        h.plain(
            "Buongiorno Marco,\n\ncome d'accordo al telefono le scrivo per il preventivo della cucina: \
             rifacimento dell'impianto elettrico, piastrelle nuove e spostamento del lavello sotto la finestra.\n\n\
             Riesce a mandarmi una stima entro fine settimana?\n\nGrazie mille,\nun saluto",
        ),
    );
    let d = at(3, 200);
    let mut h = head(
        marco.clone(),
        "Re: Preventivo ristrutturazione cucina",
        d,
        "seed-preventivo-2",
    );
    h.extra.push(("In-Reply-To", thread[0].clone()));
    h.extra.push(("References", thread[0].clone()));
    push(
        Folder::Inbox,
        d,
        &["\\Seen"],
        h.plain(
            "Buongiorno,\n\ngrazie per la richiesta. Per darle un prezzo preciso dovrei fare un sopralluogo: \
             le andrebbe bene mercoledì alle 17:30?\n\nCordiali saluti,\nMarco Rossi\nRossi Ristrutturazioni",
        ),
    );
    let d = at(2, 400);
    let mut h = head(
        me.clone(),
        "Re: Preventivo ristrutturazione cucina",
        d,
        "seed-preventivo-3",
    );
    h.to = marco.clone();
    h.extra.push(("In-Reply-To", thread[1].clone()));
    h.extra
        .push(("References", format!("{} {}", thread[0], thread[1])));
    push(
        Folder::Sent,
        d,
        &["\\Seen"],
        h.plain(
            "Perfetto, mercoledì alle 17:30 va benissimo. Il citofono è \"Interno 4\".\n\nA presto!\n\n\
             > le andrebbe bene mercoledì alle 17:30?",
        ),
    );
    let d = at(1, 90);
    let mut h = head(
        marco.clone(),
        "Re: Preventivo ristrutturazione cucina",
        d,
        "seed-preventivo-4",
    );
    h.extra.push(("In-Reply-To", thread[2].clone()));
    h.extra.push((
        "References",
        format!("{} {} {}", thread[0], thread[1], thread[2]),
    ));
    push(
        Folder::Inbox,
        d,
        &["\\Flagged"],
        h.plain(
            "Buonasera,\n\ncome promesso le invio il riepilogo dopo il sopralluogo:\n\n\
             - impianto elettrico: 2.400 €\n- piastrelle e posa: 3.150 €\n- spostamento lavello e idraulica: 1.200 €\n\n\
             Totale indicativo 6.750 € IVA esclusa. Possiamo iniziare già il 9 del mese prossimo.\n\n\
             Mi faccia sapere,\nMarco Rossi",
        ),
    );

    // --- Personal messages ----------------------------------------------------------
    let d = at(0, 95);
    push(
        Folder::Inbox,
        d,
        &[],
        head(address("Giulia Bianchi", "giulia.bianchi@example.com"), "Pranzo di giovedì?", d, "seed-pranzo").plain(
            "Ciao!\n\nCome va? Giovedì ho la pausa pranzo un po' più lunga: ti va di provare la nuova trattoria \
             in via Roma? Dicono che i tortellini siano spettacolari.\n\nFammi sapere entro domani così prenoto.\n\n\
             Un abbraccio,\nGiulia",
        ),
    );
    let d = at(3, 30);
    push(
        Folder::Inbox,
        d,
        &[],
        head(address("Roberto Gallo", "roberto.gallo@example.net"), "Riunione condominiale di lunedì", d, "seed-condominio")
            .plain(
                "Gentili condòmini,\n\nvi ricordo la riunione di lunedì alle 21:00 nella sala comune al piano terra.\n\n\
                 Ordine del giorno:\n1. approvazione del bilancio consuntivo\n2. preventivo per la tinteggiatura delle scale\n\
                 3. varie ed eventuali\n\nChi non può partecipare può delegare un vicino.\n\nRoberto Gallo, amministratore",
            ),
    );
    let d = at(13, 600);
    push(
        Folder::Inbox,
        d,
        &["\\Seen", "\\Answered"],
        head(address("Luca Ferrari", "luca.ferrari@example.org"), "Calcetto stasera?", d, "seed-calcetto").plain(
            "Ehi, stasera ci manca il decimo per il calcetto delle 20:30 al centro sportivo. Ci sei?\n\nLuca",
        ),
    );
    let d = at(13, 580);
    let mut h = head(me.clone(), "Re: Calcetto stasera?", d, "seed-calcetto-re");
    h.to = address("Luca Ferrari", "luca.ferrari@example.org");
    h.extra
        .push(("In-Reply-To", "<seed-calcetto@nmmail.example>".to_string()));
    h.extra
        .push(("References", "<seed-calcetto@nmmail.example>".to_string()));
    push(
        Folder::Sent,
        d,
        &["\\Seen"],
        h.plain("Ci sono! Porto anche il pallone nuovo.\n\n> Ci sei?"),
    );
    let d = at(18, 240);
    push(
        Folder::Inbox,
        d,
        &["\\Seen", "\\Flagged"],
        head(address("Francesca Romano", "francesca.romano@azienda.example"), "Verbale riunione di progetto", d, "seed-verbale")
            .plain(
                "Ciao a tutti,\n\necco il verbale della riunione di oggi.\n\nPresenti: Francesca, Davide, Chiara, Paolo.\n\n\
                 Decisioni:\n- il rilascio della versione 2.3 slitta al 15 del mese prossimo;\n\
                 - Davide prepara la demo per il cliente entro venerdì;\n\
                 - Chiara aggiorna la documentazione utente in italiano e in inglese.\n\n\
                 Prossima riunione: martedì alle 10:00.\n\nFrancesca",
            ),
    );
    let d = at(22, 700);
    push(
        Folder::Inbox,
        d,
        &["\\Seen"],
        head(address("Mamma", "mamma.rossi@example.org"), "Ricetta del tiramisù", d, "seed-tiramisu").plain(
            "Tesoro, eccoti la ricetta che mi avevi chiesto.\n\nIngredienti: 500 g di mascarpone, 4 uova freschissime, \
             100 g di zucchero, savoiardi, caffè della moka, cacao amaro.\n\n\
             Monta i tuorli con lo zucchero, aggiungi il mascarpone e poi gli albumi montati a neve. \
             Inzuppa velocemente i savoiardi nel caffè freddo e alterna strati di biscotti e crema. \
             Lascia riposare in frigo almeno sei ore e spolvera di cacao prima di servire.\n\nUn bacione, mamma",
        ),
    );
    let d = at(28, 120);
    push(
        Folder::Inbox,
        d,
        &["\\Seen"],
        head(address("Elena Conti", "elena.conti@example.com"), "Ti consiglio un libro", d, "seed-libro").plain(
            "Ciao! Ho appena finito \"Il nome della rosa\" e mi è piaciuto tantissimo: atmosfera cupa, \
             un'abbazia piena di misteri e un finale che non ti aspetti. Te lo presto quando ci vediamo?\n\nElena",
        ),
    );
    let d = at(33, 60);
    push(
        Folder::Inbox,
        d,
        &["\\Seen"],
        head(
            address("Paolo Esposito", "paolo.esposito@example.net"),
            "Auguri di buon compleanno!",
            d,
            "seed-auguri",
        )
        .plain(
            "Tanti auguri di buon compleanno! 🎉\n\nSpero che tu passi una splendida giornata. \
             Sabato festeggiamo con una pizza tutti insieme?\n\nPaolo",
        ),
    );
    let d = at(42, 800);
    push(
        Folder::Inbox,
        d,
        &["\\Seen"],
        head(address("Chiara Galli", "chiara.galli@example.com"), "Vacanze di Natale: proposta", d, "seed-natale").plain(
            "Ciao!\n\nStavo pensando alle vacanze di Natale: che ne dici di una settimana in Trentino? \
             Ho trovato una baita a Canazei con vista sulle Dolomiti, 4 posti letto, a 90 € a notte.\n\n\
             Se ti va blocchiamo le date dal 27 al 3 gennaio.\n\nChiara",
        ),
    );
    let d = at(55, 300);
    push(
        Folder::Inbox,
        d,
        &["\\Seen", "\\Answered"],
        head(address("Davide Marino", "davide.marino@example.org"), "Documenti per il notaio", d, "seed-notaio").plain(
            "Ciao, il notaio mi ha chiesto copia della carta d'identità, del codice fiscale e della \
             visura catastale aggiornata. Riesci a mandarmeli entro lunedì?\n\nGrazie, Davide",
        ),
    );
    let d = at(55, 100);
    let mut h = head(
        me.clone(),
        "Re: Documenti per il notaio",
        d,
        "seed-notaio-re",
    );
    h.to = address("Davide Marino", "davide.marino@example.org");
    h.extra
        .push(("In-Reply-To", "<seed-notaio@nmmail.example>".to_string()));
    push(
        Folder::Sent,
        d,
        &["\\Seen"],
        h.plain("Ciao Davide, te li porto di persona domani mattina in ufficio.\n\nA domani!"),
    );

    // --- Attachment, inline image, remote image ---------------------------------------
    let d = at(5, 400);
    let pdf = tiny_pdf("Biglietto Milano Centrale - Roma Termini, carrozza 7, posto 12A");
    let mut h = head(
        address("Treno Veloce", "biglietti@trenoveloce.example"),
        "Il tuo biglietto Milano Centrale → Roma Termini",
        d,
        "seed-biglietto",
    );
    h.extra.push(("X-Test-Category", "updates".to_string()));
    push(
        Folder::Inbox,
        d,
        &["\\Seen"],
        h.with_entity(&multipart(
            "mixed",
            "nmmail-mixed-biglietto",
            &[
                text_part(
                    "Gentile cliente,\n\ngrazie per aver viaggiato con Treno Veloce. In allegato trovi il biglietto \
                     in formato PDF per il treno delle 8:10 da Milano Centrale a Roma Termini.\n\n\
                     Carrozza 7, posto 12A. Ricorda di presentare il codice QR al personale di bordo.\n\nBuon viaggio!",
                ),
                format!(
                    "Content-Type: application/pdf; name=\"biglietto-milano-roma.pdf\"\r\n\
                     Content-Disposition: attachment; filename=\"biglietto-milano-roma.pdf\"\r\n\
                     Content-Transfer-Encoding: base64\r\n\r\n{}",
                    base64_lines(&pdf)
                ),
            ],
        )),
    );
    let d = at(8, 500);
    let html = "<html><body style=\"font-family:sans-serif\"><p>Ciao!</p>\
        <p>Ecco la foto della cima che abbiamo raggiunto domenica: che fatica, ma che panorama!</p>\
        <p><img src=\"cid:foto-cima@nmmail.example\" alt=\"La cima\" width=\"288\" height=\"192\"></p>\
        <p>La prossima volta proviamo il rifugio Vajolet.</p><p>Anna</p></body></html>";
    push(
        Folder::Inbox,
        d,
        &[],
        head(address("Anna Verdi", "anna.verdi@example.com"), "Foto della gita in montagna", d, "seed-foto").with_entity(
            &multipart(
                "alternative",
                "nmmail-alt-foto",
                &[
                    text_part(
                        "Ciao!\n\nEcco la foto della cima che abbiamo raggiunto domenica: che fatica, ma che panorama!\n\n\
                         La prossima volta proviamo il rifugio Vajolet.\n\nAnna",
                    ),
                    multipart(
                        "related",
                        "nmmail-rel-foto",
                        &[
                            html_part(html),
                            format!(
                                "Content-Type: image/png; name=\"cima.png\"\r\n\
                                 Content-Disposition: inline; filename=\"cima.png\"\r\n\
                                 Content-ID: <foto-cima@nmmail.example>\r\n\
                                 Content-Transfer-Encoding: base64\r\n\r\n{}",
                                base64_lines(&mountain_png())
                            ),
                        ],
                    ),
                ],
            ),
        ),
    );
    let d = at(1, 700);
    let mut h = head(
        address("Libreria Fenice", "newsletter@libreriafenice.example"),
        "Le novità di novembre: -20% su tutti i gialli",
        d,
        "seed-fenice",
    );
    h.extra.push(("X-Test-Category", "promotions".to_string()));
    h.extra.push((
        "List-Unsubscribe",
        "<https://www.libreriafenice.example/disiscriviti>".to_string(),
    ));
    push(
        Folder::Inbox,
        d,
        &[],
        h.html(
            "Il mese dei gialli alla Libreria Fenice\n\nPer tutto novembre sconto del 20% su tutti i romanzi gialli \
             e thriller, in negozio e online.\n\nScopri le offerte: https://www.libreriafenice.example/gialli",
            "<!DOCTYPE html><html><body style=\"margin:0;background:#f6f1e7;font-family:Georgia,serif\">\
             <table width=\"100%\" cellpadding=\"0\" cellspacing=\"0\"><tr><td align=\"center\">\
             <table width=\"600\" style=\"background:#ffffff\">\
             <tr><td><img src=\"https://images.libreriafenice.example/newsletter/novembre-banner.jpg\" \
             width=\"600\" height=\"200\" alt=\"Le novità di novembre\"></td></tr>\
             <tr><td style=\"padding:24px\"><h1 style=\"color:#8b2e16\">Il mese dei gialli</h1>\
             <p>Per tutto novembre <strong>sconto del 20%</strong> su tutti i romanzi gialli e thriller, \
             in negozio e online.</p><p>I più venduti della settimana: <em>La donna della domenica</em>, \
             <em>Il giorno della civetta</em>, <em>Io non ho paura</em>.</p>\
             <p><a href=\"https://www.libreriafenice.example/gialli?utm_source=newsletter\" \
             style=\"background:#8b2e16;color:#fff;padding:10px 18px;text-decoration:none\">Scopri le offerte</a></p>\
             </td></tr><tr><td style=\"padding:12px;font-size:11px;color:#888\">Ricevi questa email perché sei \
             iscritto alla newsletter della Libreria Fenice. \
             <a href=\"https://www.libreriafenice.example/disiscriviti\">Disiscriviti</a></td></tr>\
             </table></td></tr></table></body></html>",
            "nmmail-alt-fenice",
        ),
    );

    // --- Newsletters, services and notifications --------------------------------------
    let d = at(15, 200);
    let mut h = head(
        address("Supermercato Sole", "offerte@supermercatosole.example"),
        "Offerte della settimana: frutta e verdura a metà prezzo",
        d,
        "seed-sole",
    );
    h.extra.push(("X-Test-Category", "promotions".to_string()));
    push(
        Folder::Inbox,
        d,
        &[],
        h.html(
            "Questa settimana frutta e verdura a metà prezzo con la carta fedeltà Sole. Arance di Sicilia a 0,99 €/kg, \
             zucche mantovane a 1,20 €/kg.",
            "<html><body style=\"font-family:Arial,sans-serif\"><h2 style=\"color:#e08a00\">Offerte della settimana</h2>\
             <ul><li>Arance di Sicilia: <b>0,99 €/kg</b></li><li>Zucche mantovane: <b>1,20 €/kg</b></li>\
             <li>Radicchio di Treviso: <b>2,50 €/kg</b></li></ul>\
             <p>Valide fino a domenica con la carta fedeltà Sole.</p></body></html>",
            "nmmail-alt-sole",
        ),
    );
    let d = at(6, 300);
    push(
        Folder::Inbox,
        d,
        &["\\Seen"],
        head(
            address("Studio Dentistico Sorriso", "segreteria@studiosorriso.example"),
            "Promemoria appuntamento di martedì",
            d,
            "seed-dentista",
        )
        .plain(
            "Gentile paziente,\n\nle ricordiamo l'appuntamento di martedì alle 15:45 con la dott.ssa Moretti \
             per la pulizia dei denti.\n\nIn caso di impedimento la preghiamo di avvisarci con almeno 24 ore di anticipo.\n\n\
             Studio Dentistico Sorriso",
        ),
    );
    let d = at(10, 100);
    push(
        Folder::Inbox,
        d,
        &["\\Seen"],
        head(
            address("Comune di Esempio", "urp@comune.esempio.example"),
            "Avviso: lavori stradali in via Garibaldi",
            d,
            "seed-comune",
        )
        .plain(
            "Si comunica che da lunedì a venerdì, dalle 8:00 alle 18:00, via Garibaldi sarà chiusa al traffico \
             nel tratto tra piazza Duomo e via Mazzini per il rifacimento del manto stradale.\n\n\
             Ufficio Relazioni con il Pubblico",
        ),
    );
    let d = at(12, 450);
    let mut h = head(
        address("Banca Esempio", "notifiche@bancaesempio.example"),
        "Estratto conto di ottobre disponibile",
        d,
        "seed-banca",
    );
    h.extra.push(("X-Test-Category", "updates".to_string()));
    push(
        Folder::Inbox,
        d,
        &["\\Seen"],
        h.html(
            "Gentile cliente, l'estratto conto di ottobre è disponibile nella tua area riservata.",
            "<html><body style=\"font-family:Verdana,sans-serif\"><p>Gentile cliente,</p>\
             <p>l'estratto conto di <b>ottobre</b> è disponibile nella tua area riservata.</p>\
             <p style=\"font-size:11px;color:#777\">Questa è una comunicazione automatica, non rispondere.</p>\
             </body></html>",
            "nmmail-alt-banca",
        ),
    );
    let d = at(20, 50);
    push(
        Folder::Inbox,
        d,
        &["\\Seen"],
        head(
            address("Piattaforma Codice", "notifiche@codice.example"),
            "[nmmail] Nuova segnalazione: notifiche duplicate dopo il risveglio",
            d,
            "seed-issue",
        )
        .plain(
            "chiara-g ha aperto una nuova segnalazione nel progetto nmmail:\n\n\
             \"Dopo il risveglio dalla sospensione arrivano due notifiche per la stessa mail.\"\n\n\
             Rispondi a questa email o visualizzala sul sito.",
        ),
    );
    let d = at(25, 900);
    push(
        Folder::Inbox,
        d,
        &["\\Seen"],
        head(
            address("Assicurazioni Tranquille", "clienti@tranquille.example"),
            "Rinnovo polizza auto in scadenza",
            d,
            "seed-polizza",
        )
        .html(
            "La tua polizza RC auto scade il 30 del mese. Rinnova online in pochi minuti.",
            "<html><body><p>La tua polizza <b>RC auto</b> scade il 30 del mese.</p>\
             <p><a href=\"https://www.tranquille.example/rinnovo\">Rinnova online</a> in pochi minuti.</p></body></html>",
            "nmmail-alt-polizza",
        ),
    );
    let d = at(30, 200);
    let mut h = head(
        address("Palestra FitLife", "info@fitlife.example"),
        "Il tuo abbonamento scade tra 7 giorni",
        d,
        "seed-palestra",
    );
    h.extra.push(("X-Test-Category", "updates".to_string()));
    push(
        Folder::Inbox,
        d,
        &["\\Seen"],
        h.plain(
            "Ciao! Il tuo abbonamento annuale scade tra 7 giorni. Rinnovando entro la scadenza avrai un mese omaggio.",
        ),
    );
    let d = at(35, 30);
    let mut h = head(
        address("Amici in Rete", "notifiche@amicinrete.example"),
        "Hai 3 nuove notifiche",
        d,
        "seed-social",
    );
    h.extra.push(("X-Test-Category", "social".to_string()));
    push(
        Folder::Inbox,
        d,
        &["\\Seen"],
        h.plain(
            "Giulia, Luca e altre 2 persone hanno commentato la tua foto \"Tramonto sul lago\".",
        ),
    );
    let d = at(38, 400);
    push(
        Folder::Inbox,
        d,
        &["\\Seen", "\\Flagged"],
        head(address("Ufficio Personale", "hr@azienda.example"), "Cedolino di ottobre", d, "seed-cedolino").plain(
            "Il cedolino di ottobre è disponibile sul portale del dipendente. Per dubbi contatta l'ufficio personale.",
        ),
    );
    let d = at(45, 100);
    push(
        Folder::Inbox,
        d,
        &["\\Seen"],
        head(
            address("Corriere Rapido", "tracking@corriererapido.example"),
            "La tua spedizione è in consegna",
            d,
            "seed-corriere",
        )
        .plain("La spedizione 1Z-4815-1623 è in consegna oggi tra le 14:00 e le 18:00."),
    );
    let d = at(50, 600);
    let mut h = head(
        address(
            "Associazione Musica Insieme",
            "eventi@musicainsieme.example",
        ),
        "Invito: concerto di musica classica",
        d,
        "seed-concerto",
    );
    h.extra.push(("X-Test-Category", "promotions".to_string()));
    push(
        Folder::Inbox,
        d,
        &["\\Seen"],
        h.html(
            "Sabato alle 21 al Teatro Comunale: Vivaldi, Le quattro stagioni. Ingresso libero per i soci.",
            "<html><body><h2>Le quattro stagioni</h2><p>Sabato alle 21:00 al Teatro Comunale.</p>\
             <p>Ingresso libero per i soci, 10 € per gli altri.</p></body></html>",
            "nmmail-alt-concerto",
        ),
    );
    let d = at(59, 200);
    push(
        Folder::Inbox,
        d,
        &["\\Seen"],
        head(
            address("Energia Pulita", "bollette@energiapulita.example"),
            "La tua bolletta di settembre",
            d,
            "seed-bolletta",
        )
        .plain(
            "La bolletta di settembre di 84,30 € è disponibile nell'area clienti. \
             L'importo sarà addebitato sul conto corrente il giorno 15.",
        ),
    );
    demos
}

/// Stores the demo messages for `user`; returns how many were stored.
pub(crate) fn seed_user(store: &mut Store, user: &str) -> usize {
    let Some(key) = store.user_key(user) else {
        return 0;
    };
    let Some(sent) = store
        .user(&key)
        .and_then(|u| u.special(SpecialUse::Sent))
        .map(|m| m.name.clone())
    else {
        return 0;
    };
    let now = Local::now().fixed_offset();
    let now = now.with_nanosecond(0).unwrap_or(now);
    let mut demos = demos(&key, now);
    demos.sort_by_key(|d| d.date);
    let mut stored = 0;
    for demo in demos {
        let mailbox = match demo.folder {
            Folder::Inbox => "INBOX",
            Folder::Sent => sent.as_str(),
        };
        let flags = normalize_flags(demo.flags.iter().copied());
        if store
            .append(&key, mailbox, demo.raw, &flags, Some(demo.date), false)
            .is_ok()
        {
            stored += 1;
        }
    }
    stored
}

/// A new message for `to`, used by `--auto-deliver` (content rotates with `counter`).
pub fn demo_message(counter: u64, to: &str) -> Vec<u8> {
    const SENDERS: [(&str, &str, &str, &str); 5] = [
        (
            "Giulia Bianchi",
            "giulia.bianchi@example.com",
            "Hai visto il messaggio di prima?",
            "Ciao, ti ho appena scritto per sabato: confermi che ci sei?",
        ),
        (
            "Treno Veloce",
            "avvisi@trenoveloce.example",
            "Il tuo treno è in ritardo di 15 minuti",
            "Il Frecciaveloce 9512 delle 18:05 viaggia con un ritardo stimato di 15 minuti. Ci scusiamo per il disagio.",
        ),
        (
            "Corriere Rapido",
            "tracking@corriererapido.example",
            "Pacco consegnato",
            "Il tuo pacco è stato consegnato e lasciato in portineria.",
        ),
        (
            "Francesca Romano",
            "francesca.romano@azienda.example",
            "Domanda veloce sul rilascio",
            "Riusciamo a spostare la demo al pomeriggio? Il cliente ha un imprevisto la mattina.",
        ),
        (
            "Banca Esempio",
            "notifiche@bancaesempio.example",
            "Pagamento con carta di 23,40 €",
            "È stato registrato un pagamento con carta di 23,40 € presso Bar Centrale.",
        ),
    ];
    let (name, email, subject, body) = SENDERS[usize::try_from(counter % 5).unwrap_or(0)];
    let now = Local::now().fixed_offset();
    let head = Head {
        from: address(name, email),
        to: address("", to),
        subject,
        date: now.with_nanosecond(0).unwrap_or(now),
        message_id: format!("auto-{counter}-{}@nmmail.example", now.timestamp()),
        extra: Vec::new(),
    };
    head.plain(&format!(
        "{body}\n\n(Messaggio automatico n. {counter} del server di test NM Mail.)"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn demo_content_parses() {
        let mut store = Store::new(&[("demo@nmmail.local".into(), "x".into())], false);
        let count = seed_user(&mut store, "demo@nmmail.local");
        assert!(count >= 25, "only {count} demo messages");
        let inbox = store.mailbox("demo@nmmail.local", "INBOX").unwrap();
        assert!(inbox.messages.len() >= 22);
        assert!(
            inbox
                .messages
                .windows(2)
                .all(|w| w[0].data.internal_date <= w[1].data.internal_date)
        );
        for message in &inbox.messages {
            let parsed = mail_parser::MessageParser::new()
                .parse(&message.data.raw)
                .unwrap();
            assert!(parsed.subject().is_some_and(|s| !s.is_empty()));
        }
        assert!(
            store
                .mailbox("demo@nmmail.local", "Sent")
                .unwrap()
                .messages
                .len()
                >= 3
        );
        let pdf = tiny_pdf("x");
        assert!(pdf.starts_with(b"%PDF-1.4") && pdf.ends_with(b"%%EOF\n"));
        assert!(mountain_png().starts_with(b"\x89PNG"));
        assert!(
            String::from_utf8(demo_message(3, "a@b.c"))
                .unwrap()
                .contains("Subject: ")
        );
    }

    #[test]
    fn quoted_printable_wraps() {
        let encoded = quoted_printable(&"à".repeat(60));
        assert!(encoded.lines().all(|l| l.len() <= 76));
        assert!(encoded.starts_with("=C3=A0"));
    }
}
