//! Settings: `notifications` = `people` (default, no bulk mail) | `all` | `none`.
//! `notify_new_senders` (on unless `off`) also announces screener-held messages, which never count
//! as new mail otherwise. `notify_duration` (Windows) = `long` (default, 25 s) | `system` (Windows'
//! own delay, a few seconds) | `sticky` (until dismissed).

use std::sync::Arc;

use facteur_core::{CalendarEvent, Engine, FollowUp, NewMail};
use serde::Serialize;
use tauri::AppHandle;
#[cfg(windows)]
use tauri::Emitter;
#[cfg(desktop)]
use tauri::Manager;

#[cfg_attr(not(windows), allow(dead_code))]
pub const APP_ID: &str = "dev.facteur.mail";

const MAX_SEPARATE: usize = 3;

#[derive(Clone, Serialize)]
pub struct OpenThread {
    pub account_id: i64,
    pub mailbox_id: i64,
    pub thread_id: i64,
}

pub fn new_mail(app: &AppHandle, engine: &Arc<Engine>, account_id: i64, messages: Vec<NewMail>) {
    let mode = engine.setting("notifications").ok().flatten().unwrap_or_else(|| "people".into());
    if mode == "none" {
        return;
    }
    // Low-trust messages never notify, whatever the setting.
    let (held, messages): (Vec<NewMail>, Vec<NewMail>) =
        messages.into_iter().filter(|m| m.trust != facteur_core::trust::Level::Low).partition(|m| m.held);
    if !held.is_empty() && engine.setting("notify_new_senders").ok().flatten().as_deref() != Some("off") {
        new_senders(app, engine, &held);
    }
    let messages: Vec<NewMail> = match mode.as_str() {
        "all" => messages,
        _ => messages.into_iter().filter(|m| !m.bulk).collect(),
    };
    if messages.is_empty() {
        return;
    }
    if messages.len() > MAX_SEPARATE {
        let mut senders: Vec<String> = messages.iter().filter_map(|m| m.from.as_ref()).map(sender).collect();
        senders.dedup();
        let title = facteur_core::tr!("{} nouveaux messages", "{} new messages", messages.len());
        let text = senders.into_iter().take(4).collect::<Vec<_>>().join(", ");
        show_summary(app, engine, &title, &text, "open-unified");
        return;
    }
    for m in messages {
        show_message(app, engine, account_id, m);
    }
}

#[derive(Clone, Serialize)]
pub struct OpenEvent {
    pub id: i64,
    pub start: i64,
}

/// An event's reminder: its title, when it starts and where. It stays until dismissed, as a
/// calendar's reminders do; "Join" opens its video meeting.
pub fn event_reminder(app: &AppHandle, engine: &Arc<Engine>, event: &CalendarEvent, minutes: i64) {
    if engine.setting("notifications").ok().flatten().as_deref() == Some("none") {
        return;
    }
    let title = if event.summary.is_empty() { facteur_core::tr!("(sans titre)", "(untitled)") } else { event.summary.clone() };
    let when = match minutes {
        0 => facteur_core::tr!("Maintenant", "Now"),
        m if m < 60 => facteur_core::tr!("Dans {m} min", "In {m} min"),
        m if m < 24 * 60 && m % 60 == 0 => facteur_core::tr!("Dans {} h", "In {} h", m / 60),
        m if m < 24 * 60 => facteur_core::tr!("Dans {} h {:02}", "In {} h {:02}", m / 60, m % 60),
        m => facteur_core::tr!("Dans {} j", "In {} d", m / (24 * 60)),
    };
    let at = (!event.all_day)
        .then(|| chrono::DateTime::from_timestamp(event.start, 0))
        .flatten()
        .map(|d| d.with_timezone(&chrono::Local).format("%H:%M").to_string());
    let text = [Some(when), at].into_iter().flatten().collect::<Vec<_>>().join(" · ");
    let place = event.location.clone().unwrap_or_default();
    show_event(app, engine, &title, &text, &place, OpenEvent { id: event.id, start: event.start }, event.meeting.clone());
}

#[cfg(windows)]
fn show_event(app: &AppHandle, _engine: &Arc<Engine>, title: &str, text: &str, place: &str, target: OpenEvent, meeting: Option<String>) {
    use tauri_winrt_notification::{Scenario, Toast};
    let (join_label, open_label) = (facteur_core::tr!("Rejoindre", "Join"), facteur_core::tr!("Ouvrir", "Open"));
    let build = |app_id: &str| {
        let (app, target, meeting) = (app.clone(), target.clone(), meeting.clone());
        let mut toast = Toast::new(app_id).title(title).text1(text).scenario(Scenario::Reminder);
        if !place.is_empty() {
            toast = toast.text2(place);
        }
        if meeting.is_some() {
            toast = toast.add_button(&join_label, "join");
        }
        toast.add_button(&open_label, "open").on_activated(move |action| {
            let (app, target, meeting) = (app.clone(), target.clone(), meeting.clone());
            crate::engine_rt().spawn(async move {
                match (action.as_deref(), meeting) {
                    (Some("join"), Some(url)) => {
                        use tauri_plugin_opener::OpenerExt;
                        if let Err(e) = app.opener().open_url(url, None::<&str>) {
                            tracing::warn!("meeting link from a reminder: {e}");
                        }
                    }
                    _ => {
                        focus(&app);
                        let _ = app.emit("open-event", target);
                    }
                }
            });
            Ok(())
        })
    };
    if build(APP_ID).show().is_err() {
        let _ = build(Toast::POWERSHELL_APP_ID).show();
    }
}

#[cfg(not(windows))]
fn show_event(app: &AppHandle, _engine: &Arc<Engine>, title: &str, text: &str, place: &str, _target: OpenEvent, _meeting: Option<String>) {
    use tauri_plugin_notification::NotificationExt;
    let body = if place.is_empty() { text.to_string() } else { format!("{text}\n{place}") };
    let _ = app.notification().builder().title(plain(title)).body(plain(&body)).show();
}

fn new_senders(app: &AppHandle, engine: &Engine, held: &[NewMail]) {
    let mut seen = std::collections::HashSet::new();
    let firsts: Vec<&NewMail> = held.iter().filter(|m| seen.insert(m.from.as_ref().map(|a| a.email.to_lowercase()))).collect();
    if firsts.len() > MAX_SEPARATE {
        let title = facteur_core::tr!("{} nouveaux expéditeurs", "{} new senders", firsts.len());
        let text = firsts.iter().filter_map(|m| m.from.as_ref()).map(sender).take(4).collect::<Vec<_>>().join(", ");
        show_summary(app, engine, &title, &text, "open-screener");
        return;
    }
    for m in firsts {
        let who = m.from.as_ref().map(sender).unwrap_or_else(|| facteur_core::tr!("(inconnu)", "(unknown)"));
        let title = facteur_core::tr!("Nouvel expéditeur : {}", "New sender: {}", who);
        let subject = if m.subject.is_empty() { no_subject() } else { m.subject.clone() };
        show_summary(app, engine, &title, &subject, "open-screener");
    }
}

pub fn follow_ups(app: &AppHandle, engine: &Arc<Engine>, due: &[FollowUp]) {
    if due.is_empty() || engine.setting("notifications").ok().flatten().as_deref() == Some("none") {
        return;
    }
    let (title, text) = match due {
        [one] => (
            facteur_core::tr!("Toujours pas de réponse", "Still no reply"),
            format!("{} · {}", if one.subject.is_empty() { no_subject() } else { one.subject.clone() }, one.to.join(", ")),
        ),
        many => (
            facteur_core::tr!("{} messages sans réponse", "{} messages without a reply", many.len()),
            many.iter().map(|f| f.subject.as_str()).take(3).collect::<Vec<_>>().join(" · "),
        ),
    };
    let target = due
        .first()
        .and_then(|f| Some(OpenThread { account_id: f.account_id, mailbox_id: f.mailbox_id?, thread_id: f.thread_id? }));
    show_simple(app, engine, &title, &text, target);
}

#[cfg(windows)]
fn show_simple(app: &AppHandle, engine: &Engine, title: &str, text: &str, target: Option<OpenThread>) {
    use tauri_winrt_notification::Toast;
    let toast = |id: &str| {
        let (app, target) = (app.clone(), target.clone());
        styled(Toast::new(id).title(title).text1(text), engine, true).on_activated(move |_| {
            let (app, target) = (app.clone(), target.clone());
            tauri::async_runtime::spawn(async move {
                focus(&app);
                let _ = match target {
                    Some(t) => app.emit("open-thread", t),
                    None => app.emit("open-unified", ()),
                };
            });
            Ok(())
        })
    };
    if toast(APP_ID).show().is_err() {
        let _ = toast(Toast::POWERSHELL_APP_ID).show();
    }
}

#[cfg(not(windows))]
fn show_simple(app: &AppHandle, _engine: &Engine, title: &str, text: &str, _target: Option<OpenThread>) {
    use tauri_plugin_notification::NotificationExt;
    let _ = app.notification().builder().title(plain(title)).body(plain(text)).show();
}

fn no_subject() -> String {
    facteur_core::tr!("(sans objet)", "(no subject)")
}

fn new_message() -> String {
    facteur_core::tr!("Nouveau message", "New message")
}

fn sender(a: &facteur_core::Address) -> String {
    a.name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| a.email.clone())
}

#[cfg_attr(mobile, allow(dead_code))]
pub fn focus(app: &AppHandle) {
    #[cfg(desktop)]
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
    #[cfg(mobile)]
    let _ = app;
}

#[cfg(windows)]
fn show_message(app: &AppHandle, engine: &Arc<Engine>, account_id: i64, m: NewMail) {
    use tauri_winrt_notification::Toast;

    let title = m.from.as_ref().map(sender).unwrap_or_else(new_message);
    let subject = if m.subject.is_empty() { no_subject() } else { m.subject.clone() };
    let (done_label, reply_label) = (facteur_core::tr!("Traité", "Done"), facteur_core::tr!("Répondre", "Reply"));
    let target = OpenThread { account_id, mailbox_id: m.mailbox_id, thread_id: m.thread_id };
    let id = m.id;
    let (app, engine) = (app.clone(), engine.clone());
    let build = |app_id: &str| {
        let (app, engine, target) = (app.clone(), engine.clone(), target.clone());
        styled(Toast::new(app_id), &engine, false)
            .title(&title)
            .text1(&subject)
            .text2(&m.preview)
            .add_button(&done_label, "done")
            .add_button(&reply_label, "reply")
            .on_activated(move |action| {
                // Called on a Windows thread: hand the work back to the engine runtime.
                let (app, engine, target) = (app.clone(), engine.clone(), target.clone());
                crate::engine_rt().spawn(async move {
                    match action.as_deref() {
                        Some("done") => {
                            if let Err(e) = engine.archive(&[id]) {
                                tracing::warn!("archive from notification: {e}");
                            }
                        }
                        Some("reply") => {
                            focus(&app);
                            let _ = app.emit("open-reply", id);
                        }
                        _ => {
                            focus(&app);
                            let _ = app.emit("open-thread", target);
                        }
                    }
                });
                Ok(())
            })
    };
    // Our own AppUserModelID shows as facteur; PowerShell's is always registered, as a fallback.
    if build(APP_ID).show().is_err() {
        let _ = build(Toast::POWERSHELL_APP_ID).show();
    }
}

/// Some Linux notification servers interpret markup, and subject and name come from the sender.
#[cfg(not(windows))]
fn plain(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

#[cfg(not(windows))]
fn show_message(app: &AppHandle, _engine: &Arc<Engine>, _account_id: i64, m: NewMail) {
    use tauri_plugin_notification::NotificationExt;
    let title = m.from.as_ref().map(sender).unwrap_or_else(new_message);
    let _ = app.notification().builder().title(plain(&title)).body(plain(&m.subject)).show();
}

/// Windows keeps a reminder on screen until dismissed only if it carries a button: `open_button`
/// adds one to notifications that have none.
#[cfg(windows)]
fn styled(toast: tauri_winrt_notification::Toast, engine: &Engine, open_button: bool) -> tauri_winrt_notification::Toast {
    use tauri_winrt_notification::{Duration, Scenario};
    match engine.setting("notify_duration").ok().flatten().as_deref() {
        Some("system") => toast,
        Some("sticky") if open_button => toast.scenario(Scenario::Reminder).add_button(&facteur_core::tr!("Ouvrir", "Open"), "open"),
        Some("sticky") => toast.scenario(Scenario::Reminder),
        _ => toast.duration(Duration::Long),
    }
}

#[cfg(windows)]
fn show_summary(app: &AppHandle, engine: &Engine, title: &str, text: &str, event: &'static str) {
    use tauri_winrt_notification::Toast;
    let app2 = app.clone();
    let toast = |id: &str| {
        let app = app2.clone();
        styled(Toast::new(id).title(title).text1(text), engine, true).on_activated(move |_| {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                focus(&app);
                let _ = app.emit(event, ());
            });
            Ok(())
        })
    };
    if toast(APP_ID).show().is_err() {
        let _ = toast(Toast::POWERSHELL_APP_ID).show();
    }
}

#[cfg(not(windows))]
fn show_summary(app: &AppHandle, _engine: &Engine, title: &str, text: &str, _event: &'static str) {
    use tauri_plugin_notification::NotificationExt;
    let _ = app.notification().builder().title(plain(title)).body(plain(text)).show();
}

/// Registers the AppUserModelID (notification name and icon) without an installer.
pub fn register_app_id(icon_dir: &std::path::Path) {
    #[cfg(windows)]
    {
        use winreg::RegKey;
        use winreg::enums::HKEY_CURRENT_USER;
        let icon = icon_dir.join("facteur.png");
        if !icon.exists() {
            let _ = std::fs::write(&icon, include_bytes!("../icons/128x128.png"));
        }
        let res = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey(format!(r"Software\Classes\AppUserModelId\{APP_ID}"))
            .and_then(|(key, _)| {
                key.set_value("DisplayName", &"facteur")?;
                key.set_value("IconUri", &icon.to_string_lossy().into_owned())
            });
        if let Err(e) = res {
            tracing::warn!("notification app id not registered: {e}");
        }
    }
    #[cfg(not(windows))]
    let _ = icon_dir;
}
