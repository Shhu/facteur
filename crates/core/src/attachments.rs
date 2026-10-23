use serde::Serialize;

use crate::tr;
use crate::trust::{Level, Reason, Trust};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Image,
    Pdf,
    Text,
    Archive,
    Document,
    Executable,
    Other,
}

/// Detected from the first bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Detected {
    Png,
    Jpeg,
    Gif,
    Webp,
    Bmp,
    Svg,
    Pdf,
    Zip,
    Ole,
    Rar,
    SevenZip,
    Gzip,
    WindowsProgram,
    LinuxProgram,
    Script,
    Text,
    Unknown,
}

impl Detected {
    fn of(data: &[u8]) -> Detected {
        let starts = |sig: &[u8]| data.starts_with(sig);
        if starts(b"\x89PNG\r\n\x1a\n") {
            Detected::Png
        } else if starts(&[0xFF, 0xD8, 0xFF]) {
            Detected::Jpeg
        } else if starts(b"GIF87a") || starts(b"GIF89a") {
            Detected::Gif
        } else if starts(b"RIFF") && data.get(8..12) == Some(b"WEBP") {
            Detected::Webp
        } else if starts(b"BM") && data.len() > 26 {
            Detected::Bmp
        } else if starts(b"%PDF-") {
            Detected::Pdf
        } else if starts(b"PK\x03\x04") || starts(b"PK\x05\x06") {
            Detected::Zip
        } else if starts(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]) {
            Detected::Ole
        } else if starts(b"Rar!\x1a\x07") {
            Detected::Rar
        } else if starts(b"7z\xBC\xAF\x27\x1C") {
            Detected::SevenZip
        } else if starts(&[0x1F, 0x8B]) {
            Detected::Gzip
        } else if starts(b"MZ") {
            Detected::WindowsProgram
        } else if starts(b"\x7FELF") {
            Detected::LinuxProgram
        } else if starts(b"#!") {
            Detected::Script
        } else if let Some((encoding, bom)) = encoding_rs::Encoding::for_bom(data).filter(|(e, _)| *e != encoding_rs::UTF_8) {
            // UTF-16 with a BOM ("Unicode" text from Notepad): text when the decoded head is.
            // Never SVG: shown as plain text, mime::text_as_utf8 converts it for the viewer.
            let head = encoding.decode_without_bom_handling(&data[bom..data.len().min(4096)]).0;
            if looks_like_text(&head) { Detected::Text } else { Detected::Unknown }
        } else if std::str::from_utf8(&data[..data.len().min(4096)]).is_err_and(|e| e.valid_up_to() == 0) {
            // First byte is not UTF-8 ("École…" in Windows-1252): text when the head reads as such.
            let head = encoding_rs::WINDOWS_1252.decode_without_bom_handling(&data[..data.len().min(4096)]).0;
            if looks_like_text(&head) { Detected::Text } else { Detected::Unknown }
        } else if let Ok(text) = std::str::from_utf8(&data[..data.len().min(4096)]).or_else(|e| {
            // Cut in the middle of a character: the start is still text.
            std::str::from_utf8(&data[..e.valid_up_to()])
        }) {
            let head = text.trim_start_matches('\u{feff}').trim_start().to_ascii_lowercase();
            if head.starts_with("<svg") || (head.starts_with("<?xml") && head.contains("<svg")) {
                Detected::Svg
            } else if !text.is_empty() && !text.contains('\0') {
                Detected::Text
            } else {
                Detected::Unknown
            }
        } else {
            Detected::Unknown
        }
    }

    fn label(self) -> String {
        match self {
            Detected::Png => tr!("Image PNG", "PNG image"),
            Detected::Jpeg => tr!("Image JPEG", "JPEG image"),
            Detected::Gif => tr!("Image GIF", "GIF image"),
            Detected::Webp => tr!("Image WebP", "WebP image"),
            Detected::Bmp => tr!("Image BMP", "BMP image"),
            Detected::Svg => tr!("Image SVG", "SVG image"),
            Detected::Pdf => tr!("Document PDF", "PDF document"),
            Detected::Zip => tr!("Archive ZIP", "ZIP archive"),
            Detected::Ole => tr!("Document Office (ancien format)", "Office document (legacy format)"),
            Detected::Rar => tr!("Archive RAR", "RAR archive"),
            Detected::SevenZip => tr!("Archive 7-Zip", "7-Zip archive"),
            Detected::Gzip => tr!("Archive gzip", "gzip archive"),
            Detected::WindowsProgram => tr!("Programme Windows", "Windows program"),
            Detected::LinuxProgram => tr!("Programme Linux", "Linux program"),
            Detected::Script => tr!("Script", "Script"),
            Detected::Text => tr!("Texte", "Text"),
            Detected::Unknown => tr!("Données binaires", "Binary data"),
        }
    }

    /// Safe MIME type for the preview, never the one the sender declared.
    fn view_mime(self) -> Option<&'static str> {
        Some(match self {
            Detected::Png => "image/png",
            Detected::Jpeg => "image/jpeg",
            Detected::Gif => "image/gif",
            Detected::Webp => "image/webp",
            Detected::Bmp => "image/bmp",
            Detected::Svg => "image/svg+xml",
            Detected::Pdf => "application/pdf",
            Detected::Text => "text/plain",
            _ => return None,
        })
    }
}

/// Decoded head of a file in a legacy or UTF-16 encoding that reads as text: not empty, no NUL,
/// and control characters other than tab, line breaks and form feed, or undecodable units
/// (lone UTF-16 surrogates), are rare: binary data decoded this way is full of them.
fn looks_like_text(head: &str) -> bool {
    let mut chars = 0usize;
    let mut controls = 0usize;
    for c in head.chars() {
        if c == '\0' {
            return false;
        }
        chars += 1;
        controls += (c == '\u{fffd}' || (c.is_control() && !matches!(c, '\t' | '\r' | '\n' | '\x0c'))) as usize;
    }
    chars > 0 && controls * 100 <= chars
}

/// Extensions that run code on a double click.
const PROGRAMS: &[&str] = &[
    "exe", "scr", "com", "pif", "cpl", "msi", "msix", "msp", "appx", "appxbundle", "dll", "bat", "cmd", "js", "jse",
    "vbs", "vbe", "wsf", "wsh", "hta", "ps1", "psm1", "jar", "lnk", "reg", "inf", "scf", "url", "application",
    "gadget", "msc", "sh", "app", "command", "apk", "deb", "rpm", "dmg", "pkg", "xll", "iqy", "slk", "py", "pyw",
    "chm", "ws", "wsc", "sct", "one", "settingcontent-ms", "library-ms", "search-ms", "appref-ms", "website",
    // Opened by a system tool that connects or installs: Remote Desktop, App Installer, themes
    // (a wallpaper on a share leaks the Windows password hash), troubleshooters.
    "rdp", "appinstaller", "theme", "themepack", "desktopthemepack", "diagcab", "xbap", "msh", "msh1", "msh2",
    "ps1xml", "psc1", "mshxml", "hlp",
];
/// Opened by a browser, where they may run scripts: shown in facteur only.
const ACTIVE_IMAGES: &[&str] = &["svg", "svgz", "xht", "xhtm"];
/// Disk images: mounted on a double click, they bypass Windows' protections.
const DISK_IMAGES: &[&str] = &["iso", "img", "vhd", "vhdx"];
const MACRO_OFFICE: &[&str] = &["docm", "dotm", "xlsm", "xltm", "xlsb", "xlam", "pptm", "potm", "ppam", "ppsm", "sldm"];
/// "Reassuring" document extensions that a double extension imitates.
const DECOYS: &[&str] = &["pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "jpg", "jpeg", "png", "gif", "zip", "rtf", "odt", "csv"];
const ARCHIVES: &[&str] = &["zip", "rar", "7z", "gz", "tgz", "tar", "bz2", "xz", "cab", "arj", "lzh", "ace"];

/// facteur may open this saved attachment with the system's program for it: not a program, a
/// disk image or a macro document, by what it is and by the name it was saved under, and not
/// judged suspicious (saving one anyway, once warned, is no consent to running it).
pub fn openable(report: &AttachmentReport, saved_as: &str) -> bool {
    let risky = |name: &str| {
        ext(name).is_some_and(|e| [PROGRAMS, DISK_IMAGES, MACRO_OFFICE, ACTIVE_IMAGES].iter().any(|list| list.contains(&e.as_str())))
    };
    report.kind != Kind::Executable && !report.trust.is_low() && !risky(&report.filename) && !risky(saved_as)
}

fn ext(name: &str) -> Option<String> {
    let (stem, e) = name.rsplit_once('.')?;
    (!stem.is_empty() && !e.is_empty() && e.len() <= 12).then(|| e.trim().to_ascii_lowercase())
}

/// File names in a ZIP's central directory, and whether one is encrypted.
fn zip_entries(data: &[u8]) -> (Vec<String>, bool) {
    let mut names = Vec::new();
    let mut encrypted = false;
    let mut i = 0;
    while let Some(pos) = data.get(i..).and_then(|rest| rest.windows(4).position(|w| w == b"PK\x01\x02")) {
        let at = i + pos;
        let Some(header) = data.get(at..at + 46) else { break };
        let flags = u16::from_le_bytes([header[8], header[9]]);
        let len = u16::from_le_bytes([header[28], header[29]]) as usize;
        encrypted |= flags & 1 == 1;
        if let Some(name) = data.get(at + 46..at + 46 + len) {
            names.push(String::from_utf8_lossy(name).into_owned());
        }
        i = at + 46 + len;
        if names.len() >= 2000 {
            break;
        }
    }
    (names, encrypted)
}

fn contains(data: &[u8], needle: &[u8]) -> bool {
    data.windows(needle.len()).any(|w| w == needle)
}

#[derive(Debug, Clone, Serialize)]
pub struct AttachmentReport {
    pub filename: String,
    /// As declared by the sender.
    pub declared_mime: String,
    /// What the content really is.
    pub detected: String,
    pub kind: Kind,
    pub size: usize,
    pub viewable: bool,
    /// Deduced from the content.
    pub view_mime: Option<&'static str>,
    /// For an archive: a few of the files it holds.
    pub archive_entries: Vec<String>,
    pub archive_count: usize,
    pub trust: Trust,
}

pub fn inspect(filename: &str, declared_mime: &str, data: &[u8], message_trust: Option<Level>) -> AttachmentReport {
    let detected = Detected::of(data);
    // Judged as it will be saved (`safe_filename`): invisible characters and trailing dots, which
    // the saved name loses, must not hide the extension from this check either.
    let judged = safe_filename(filename);
    let extension = ext(&judged);
    let e = extension.as_deref().unwrap_or("");
    let mut score: i32 = 50;
    let mut reasons = Vec::new();
    fn note(score: &mut i32, reasons: &mut Vec<Reason>, delta: i32, text: String) {
        *score += delta;
        reasons.push(Reason { positive: delta > 0, text });
    }
    let trapped = filename.chars().any(|c| matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}'));

    // Bidi control characters (U+202E…): « facture<U+202E>fdp.exe » shows as
    // « factureexe.pdf ».
    if trapped {
        note(&mut score, &mut reasons, -60, tr!("Nom de fichier piégé : des caractères invisibles inversent l'affichage de l'extension", "Booby-trapped file name: invisible characters reverse how the extension is displayed"));
    }
    let program_ext = PROGRAMS.contains(&e);
    if program_ext {
        note(&mut score, &mut reasons, -50, tr!("Fichier exécutable (.{e}) : l'ouvrir lance un programme sur l'ordinateur", "Executable file (.{e}): opening it runs a program on the computer"));
    } else if DISK_IMAGES.contains(&e) {
        note(&mut score, &mut reasons, -35, tr!("Image disque (.{e}) : souvent utilisée pour faire passer un programme", "Disk image (.{e}): often used to sneak in a program"));
    } else if matches!(e, "html" | "htm" | "xhtml" | "xht" | "xhtm" | "shtml" | "mht" | "mhtml") {
        note(&mut score, &mut reasons, -25, tr!("Page web jointe : technique courante pour imiter une page de connexion", "Attached web page: a common trick to imitate a sign-in page"));
    }
    let disguised = !trapped && (filename.chars().any(invisible) || filename.trim_end() != filename.trim_end().trim_end_matches('.'));
    if disguised {
        note(&mut score, &mut reasons, -40, tr!(
            "Nom de fichier truqué : caractères invisibles ou points en fin de nom, pour cacher sa vraie extension",
            "Disguised file name: invisible characters or trailing dots, to hide its real extension"
        ));
    }
    let parts: Vec<&str> = judged.split('.').collect();
    if parts.len() >= 3 && DECOYS.contains(&parts[parts.len() - 2].trim().to_ascii_lowercase().as_str()) && (program_ext || DISK_IMAGES.contains(&e)) {
        note(&mut score, &mut reasons, -30, tr!("Double extension (.{}.{e}) : se fait passer pour un document", "Double extension (.{}.{e}): pretends to be a document", parts[parts.len() - 2].trim().to_ascii_lowercase()));
    }
    if filename.contains("     ") {
        note(&mut score, &mut reasons, -20, tr!("Nom de fichier rempli d'espaces : cache sa vraie extension", "File name padded with spaces: hides its real extension"));
    }

    let is_program = matches!(detected, Detected::WindowsProgram | Detected::LinuxProgram);
    if is_program && !program_ext {
        note(&mut score, &mut reasons, -50, tr!("Programme déguisé : le contenu est un exécutable, malgré son nom", "Disguised program: the content is an executable, despite its name"));
    }
    let claims_image = matches!(e, "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp");
    let is_image = matches!(detected, Detected::Png | Detected::Jpeg | Detected::Gif | Detected::Webp | Detected::Bmp);
    if (claims_image && !is_image) || (e == "pdf" && detected != Detected::Pdf) {
        note(&mut score, &mut reasons, -20, tr!("Le contenu n'est pas celui annoncé par l'extension .{e} ({})", "The content does not match the .{e} extension ({})", detected.label().to_lowercase()));
    }

    let mut archive_entries = Vec::new();
    let mut archive_count = 0;
    match detected {
        Detected::Zip => {
            let (names, encrypted) = zip_entries(data);
            let office = names.iter().any(|n| n.starts_with("word/") || n.starts_with("xl/") || n.starts_with("ppt/"));
            if office || MACRO_OFFICE.contains(&e) {
                if names.iter().any(|n| n.to_ascii_lowercase().ends_with("vbaproject.bin")) || MACRO_OFFICE.contains(&e) {
                    note(&mut score, &mut reasons, -35, tr!("Document Office à macros : du code peut s'exécuter à l'ouverture", "Office document with macros: code may run when it is opened"));
                }
            } else {
                if encrypted {
                    note(&mut score, &mut reasons, -25, tr!("Archive protégée par mot de passe : son contenu échappe aux antivirus", "Password-protected archive: its content evades antivirus scanners"));
                }
                if let Some(bad) = names.iter().find(|n| ext(n).is_some_and(|x| PROGRAMS.contains(&x.as_str()) || DISK_IMAGES.contains(&x.as_str()))) {
                    note(&mut score, &mut reasons, -40, tr!("L'archive contient un exécutable : {}", "The archive contains an executable: {}", bad.rsplit('/').next().unwrap_or(bad)));
                }
                archive_count = names.iter().filter(|n| !n.ends_with('/')).count();
                archive_entries = names.into_iter().filter(|n| !n.ends_with('/')).take(8).collect();
            }
        }
        Detected::Rar | Detected::SevenZip => note(&mut score, &mut reasons, -10, tr!("Archive dont facteur ne peut pas lire le contenu", "Archive whose content facteur cannot read")),
        Detected::Ole => note(&mut score, &mut reasons, -15, tr!("Ancien format Office : peut contenir des macros", "Legacy Office format: may contain macros")),
        Detected::Pdf => {
            if contains(data, b"/JavaScript") || contains(data, b"/Launch") || contains(data, b"/EmbeddedFile") {
                note(&mut score, &mut reasons, -20, tr!("Le PDF contient du code ou un fichier caché (JavaScript, lancement, pièce intégrée)", "The PDF contains code or a hidden file (JavaScript, launch action, embedded file)"));
            }
        }
        Detected::Svg => {
            let lower = data.to_ascii_lowercase();
            if [&b"<script"[..], b"javascript:", b"<foreignobject", b"onload=", b"onclick=", b"onerror=", b"onmouseover=", b"onbegin="]
                .iter()
                .any(|code| contains(&lower, code))
            {
                note(&mut score, &mut reasons, -20, tr!("Image SVG contenant du code : à n'ouvrir que dans facteur", "SVG image containing code: only open it in facteur"));
            }
        }
        _ => {}
    }

    let kind = match detected {
        Detected::Png | Detected::Jpeg | Detected::Gif | Detected::Webp | Detected::Bmp | Detected::Svg => Kind::Image,
        Detected::Pdf => Kind::Pdf,
        Detected::WindowsProgram | Detected::LinuxProgram | Detected::Script => Kind::Executable,
        _ if program_ext => Kind::Executable,
        Detected::Zip if !MACRO_OFFICE.contains(&e) && !matches!(e, "docx" | "xlsx" | "pptx" | "odt" | "ods" | "odp") => Kind::Archive,
        Detected::Zip | Detected::Ole => Kind::Document,
        Detected::Rar | Detected::SevenZip | Detected::Gzip => Kind::Archive,
        _ if ARCHIVES.contains(&e) => Kind::Archive,
        Detected::Text => Kind::Text,
        _ => Kind::Other,
    };
    // HTML or a script is text, but is not shown as such: it is made to be run.
    let viewable = detected.view_mime().is_some() && !program_ext && !matches!(e, "html" | "htm" | "xhtml" | "svgz") && !trapped;
    if viewable && reasons.is_empty() {
        note(&mut score, &mut reasons, 20, tr!("Contenu conforme : {}", "Content matches: {}", detected.label().to_lowercase()));
    }
    match message_trust {
        Some(Level::Low) => note(&mut score, &mut reasons, -30, tr!("Le message lui-même est peu fiable", "The message itself is untrustworthy")),
        Some(Level::High) => note(&mut score, &mut reasons, 10, tr!("Message d'un expéditeur vérifié", "Message from a verified sender")),
        _ => {}
    }

    let score = score.clamp(0, 100) as u8;
    let level = match score {
        70.. => Level::High,
        40..=69 => Level::Medium,
        _ => Level::Low,
    };
    reasons.sort_by_key(|r| r.positive);
    AttachmentReport {
        filename: filename.to_string(),
        declared_mime: declared_mime.to_string(),
        detected: detected.label(),
        kind,
        size: data.len(),
        viewable,
        view_mime: if viewable { detected.view_mime() } else { None },
        archive_entries,
        archive_count,
        trust: Trust { score, level, reasons },
    }
}

/// Invisible characters that change how a name reads without being seen: bidi controls
/// (`facture\u{202E}fdp.exe` shows as `factureexe.pdf`), zero-width and other format characters.
pub(crate) fn invisible(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{034F}' | '\u{061C}' | '\u{115F}' | '\u{1160}' | '\u{17B4}' | '\u{17B5}' | '\u{180B}'..='\u{180F}'
        | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{206F}' | '\u{3164}' | '\u{FE00}'..='\u{FE0F}'
        | '\u{FEFF}' | '\u{FFA0}' | '\u{FFF0}'..='\u{FFFB}' | '\u{E0000}'..='\u{E0FFF}')
}

/// Name suggested in the "Save as" dialog for a file named by the sender: no folder, no
/// invisible or control character, nothing Windows refuses or reinterprets (`<>:"|?*`,
/// trailing dots and spaces, `CON`, `NUL`…), no leading dot (hidden file), at most 180 characters
/// with the extension kept. Empty when nothing is left.
pub fn safe_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .filter(|c| !invisible(*c))
        .map(|c| if c.is_control() || "<>:\"|?*".contains(c) { '_' } else if c.is_whitespace() { ' ' } else { c })
        .collect();
    // Runs of spaces hide an extension far to the right ("facture.pdf          .exe").
    let mut collapsed = String::with_capacity(cleaned.len());
    for c in cleaned.chars() {
        if !(c == ' ' && collapsed.ends_with(' ')) {
            collapsed.push(c);
        }
    }
    let mut name = collapsed.trim_matches(|c: char| c == ' ' || c == '.').to_string();
    if name.chars().count() > 180 {
        let (stem, ext) = match name.rsplit_once('.') {
            Some((stem, ext)) if !stem.is_empty() && ext.chars().count() <= 12 => (stem.to_string(), format!(".{ext}")),
            _ => (name.clone(), String::new()),
        };
        name = stem.chars().take(180 - ext.chars().count()).collect::<String>().trim_end_matches([' ', '.']).to_string() + &ext;
    }
    let stem = name.split('.').next().unwrap_or("").trim().to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        name.insert(0, '_');
    }
    name
}

pub fn view_mime(filename: &str, data: &[u8]) -> Option<&'static str> {
    inspect(filename, "", data, None).view_mime
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_disguised_name_does_not_hide_a_program() {
        for name in ["facture.pdf.js.", "facture.pdf.j\u{200B}s", "facture.pdf.js .."] {
            let report = inspect(name, "application/octet-stream", b"WScript.Shell", None);
            assert!(report.trust.is_low(), "{name}: {:?}", report.trust);
            assert!(report.trust.reasons.iter().any(|r| r.text.contains(".js")), "{name}: {:?}", report.trust.reasons);
        }
        assert!(!inspect("rapport.pdf", "application/pdf", b"%PDF-1.7", None).trust.is_low());
    }

    fn zip_with(names: &[&str], encrypted: bool) -> Vec<u8> {
        let mut out = b"PK\x03\x04".to_vec();
        out.extend([0u8; 26]);
        for name in names {
            let mut h = b"PK\x01\x02".to_vec();
            h.extend([0u8; 4]);
            h.extend((encrypted as u16).to_le_bytes());
            h.extend([0u8; 18]);
            h.extend((name.len() as u16).to_le_bytes());
            h.extend([0u8; 16]);
            h.extend(name.as_bytes());
            out.extend(h);
        }
        out
    }

    #[test]
    fn only_plain_documents_saved_under_a_plain_name_open_from_facteur() {
        let pdf = inspect("facture.pdf", "application/pdf", b"%PDF-1.7 ...", None);
        assert!(openable(&pdf, "C:\\Users\\y\\Downloads\\facture.pdf"));
        assert!(!openable(&pdf, "C:\\Users\\y\\Downloads\\facture.exe"), "renamed to a program when saved");
        let program = inspect("facture.pdf", "application/pdf", b"MZ\x90\x00\x03", None);
        assert!(!openable(&program, "facture.pdf"), "a program under a document's name");
        let macro_doc = inspect("devis.docm", "application/octet-stream", b"PK\x03\x04", None);
        assert!(!openable(&macro_doc, "devis.docm"));
        let svg = inspect("facture.svg", "image/svg+xml", br#"<svg xmlns="http://www.w3.org/2000/svg" onload="location='https://x/'">"#, None);
        assert!(!openable(&svg, "facture.svg"), "a browser would run its script");
        assert!(!svg.trust.reasons.is_empty(), "event handlers count as code");
        let python = inspect("outil.py", "text/plain", b"import os\nprint(1)\n", None);
        assert!(!openable(&python, "outil.py"));
        let rdp = inspect("acces-vpn.rdp", "application/x-rdp", b"full address:s:evil.example\nredirectdrives:i:1\n", None);
        assert!(!openable(&rdp, "acces-vpn.rdp"), "Remote Desktop would connect to the sender's machine");
        let theme = inspect("noel.theme", "text/plain", b"[Control Panel\\Desktop]\nWallpaper=\\\\evil\\share\\a.jpg\n", None);
        assert!(!openable(&theme, "noel.theme"));
    }

    #[test]
    fn suggested_file_names_are_cleaned() {
        assert_eq!(safe_filename("facture\u{202E}fdp.exe"), "facturefdp.exe");
        assert_eq!(safe_filename("\u{2067}rapport\u{2069}\u{200B}.pdf\u{200E}"), "rapport.pdf");
        assert_eq!(safe_filename("../../etc/passwd"), "passwd");
        assert_eq!(safe_filename("C:\\Windows\\a<b>:c?.txt"), "a_b__c_.txt");
        assert_eq!(safe_filename("facture.pdf            .exe"), "facture.pdf .exe");
        assert_eq!(safe_filename(".bashrc"), "bashrc");
        assert_eq!(safe_filename("note.txt. . "), "note.txt");
        assert_eq!(safe_filename("CON.txt"), "_CON.txt");
        assert_eq!(safe_filename("com1"), "_com1");
        assert_eq!(safe_filename("tab\there\nnew"), "tab_here_new");
        assert_eq!(safe_filename("\u{202E}\u{200B}"), "");
        let long = format!("{}.docx", "a".repeat(400));
        let cut = safe_filename(&long);
        assert_eq!(cut.chars().count(), 180);
        assert!(cut.ends_with("a.docx"));
        // The inspection still sees the trap in the original name.
        assert_eq!(inspect("facture\u{202E}fdp.exe", "", b"MZ", None).trust.level, Level::Low);
    }

    #[test]
    fn truncated_zip_does_not_panic() {
        let mut zip = zip_with(&["a.txt"], false);
        // Announced name longer than what is left.
        let at = zip.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
        zip[at + 28] = 200;
        zip.truncate(at + 50);
        let _ = inspect("x.zip", "application/zip", &zip, None);
    }

    #[test]
    fn real_images_and_pdfs_are_viewable() {
        let png = inspect("photo.png", "image/png", b"\x89PNG\r\n\x1a\n....", None);
        assert!(png.viewable && png.kind == Kind::Image && png.trust.level == Level::High);
        let pdf = inspect("facture.pdf", "application/pdf", b"%PDF-1.7 ...", Some(Level::High));
        assert_eq!((pdf.kind, pdf.viewable, pdf.trust.level), (Kind::Pdf, true, Level::High));
        assert_eq!(view_mime("notes.txt", b"bonjour"), Some("text/plain"));
    }

    #[test]
    fn disguised_and_dangerous_files() {
        let fake = inspect("facture.pdf", "application/pdf", b"MZ\x90\x00 programme", None);
        assert!(!fake.viewable);
        assert_eq!(fake.trust.level, Level::Low);
        assert!(fake.trust.reasons.iter().any(|r| r.text.starts_with("Programme déguisé")));

        let double = inspect("Facture 2026.pdf.exe", "application/octet-stream", b"MZ", None);
        assert!(double.trust.reasons.iter().any(|r| r.text.starts_with("Double extension (.pdf.exe)")));
        assert_eq!(double.kind, Kind::Executable);

        let rtl = inspect("facture\u{202E}fdp.exe", "application/pdf", b"MZ", None);
        assert!(rtl.trust.reasons[0].text.starts_with("Nom de fichier piégé") || rtl.trust.reasons.iter().any(|r| r.text.starts_with("Nom de fichier piégé")));

        let archive = inspect("documents.zip", "application/zip", &zip_with(&["scan/", "scan/lettre.pdf.js"], true), None);
        assert_eq!(archive.kind, Kind::Archive);
        assert_eq!((archive.archive_count, archive.archive_entries.len()), (1, 1));
        assert_eq!(archive.trust.level, Level::Low);

        let macros = inspect("budget.xlsm", "application/vnd.ms-excel", &zip_with(&["xl/workbook.xml", "xl/vbaProject.bin"], false), None);
        assert!(macros.trust.reasons.iter().any(|r| r.text.contains("macros")));
        assert_eq!(macros.kind, Kind::Document);

        assert!(!inspect("page.html", "text/html", b"<html><form>", None).viewable);
        assert!(view_mime("fake.png", b"MZ").is_none());
    }

    #[test]
    fn utf16_and_legacy_text_is_viewable() {
        let utf16 = inspect("a.txt", "text/plain", b"\xff\xfeh\x00\xe9\x00", None);
        assert_eq!((utf16.viewable, utf16.view_mime, utf16.kind), (true, Some("text/plain"), Kind::Text));
        assert_eq!(view_mime("b.txt", b"\xfe\xff\x00h\x00\xe9\x00\n"), Some("text/plain"));
        // Windows-1252 starting with an accented letter.
        assert_eq!(view_mime("c.txt", b"\xc9cole fran\xe7aise\r\n"), Some("text/plain"));
        // Not text: NUL, control characters, lone surrogates.
        assert!(view_mime("d.bin", b"\xff\xfe\x00\x00ab").is_none());
        assert!(view_mime("e.bin", b"\xc9\x01\x02\x03\x04\x05").is_none());
        assert!(view_mime("f.bin", b"\xff\xfe\x00\xd8\x41\x00").is_none());
        // A UTF-16 SVG stays plain text.
        let svg: Vec<u8> = [0xFF, 0xFE].into_iter().chain("<svg>".encode_utf16().flat_map(u16::to_le_bytes)).collect();
        assert_eq!(view_mime("g.svg", &svg), Some("text/plain"));
        assert!(view_mime("h.bin", b"").is_none());
    }
}
