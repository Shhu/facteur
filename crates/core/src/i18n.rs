//! Language of the engine's own text; follows the interface's `language` setting, English
//! until the interface has chosen.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(not(test))]
static ENGLISH: AtomicBool = AtomicBool::new(true);
// Tests check the French text, so they run in French with no switching (global state shared
// by parallel tests).
#[cfg(test)]
static ENGLISH: AtomicBool = AtomicBool::new(false);

pub fn set(language: &str) {
    let lower = language.to_ascii_lowercase();
    ENGLISH.store(!(lower == "fr" || lower.starts_with("fr-")), Ordering::Relaxed);
}

thread_local! {
    /// Set by [`both`] while it writes one of the two languages.
    static FORCED: Cell<Option<bool>> = const { Cell::new(None) };
}

pub fn english() -> bool {
    FORCED.with(Cell::get).unwrap_or_else(|| ENGLISH.load(Ordering::Relaxed))
}

const BETWEEN: char = '\u{1f}';

/// Text kept to be shown later (a queued operation's label), written in both languages: [`pick`]
/// gives the one of the moment it is shown.
pub fn both(text: impl Fn() -> String) -> String {
    let written = |english: bool| {
        let before = FORCED.with(|f| f.replace(Some(english)));
        let out = text();
        FORCED.with(|f| f.set(before));
        out
    };
    let (fr, en) = (written(false), written(true));
    if fr == en { fr } else { format!("{fr}{BETWEEN}{en}") }
}

/// The language of the moment's half of a text written by [`both`]; any other text as it is.
pub fn pick(text: &str) -> &str {
    match text.split_once(BETWEEN) {
        Some((fr, en)) => if english() { en } else { fr },
        None => text,
    }
}

#[macro_export]
macro_rules! tr {
    ($fr:literal, $en:literal $(, $arg:expr)* $(,)?) => {
        if $crate::i18n::english() { format!($en $(, $arg)*) } else { format!($fr $(, $arg)*) }
    };
}

#[cfg(test)]
mod tests {
    #[test]
    fn tr_formats_in_french_under_test() {
        // No switch to English here: the state is global and tests run in parallel.
        let n = 3;
        assert_eq!(tr!("{n} messages", "{n} emails"), "3 messages");
        assert_eq!(tr!("{} dossiers", "{} folders", n + 1), "4 dossiers");
    }

    #[test]
    fn text_kept_in_both_languages() {
        let n = 2;
        let kept = super::both(|| tr!("Déplacer {n} messages", "Move {n} messages"));
        assert_eq!(super::pick(&kept), "Déplacer 2 messages");
        assert_eq!(crate::Error::Dropped(kept.clone()).to_string(), "connexion coupée par le serveur : Déplacer 2 messages");
        let error = super::both(|| crate::Error::Dropped(kept.clone()).to_string());
        assert_eq!(error.split_once('\u{1f}').unwrap().1, "connection dropped by the server: Move 2 messages");
        assert_eq!(super::both(|| "INBOX".to_string()), "INBOX", "the same in both: written once");
        assert_eq!(super::pick("plain"), "plain");
        assert!(!super::english(), "the language of the moment is back");
    }
}
