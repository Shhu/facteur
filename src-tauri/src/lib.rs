mod mailclient;
mod notify;
mod selfupdate;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use facteur_core::{
    Account, AccountStats, SimilarCriteria, SimilarFacets, SimilarResult, Address, BackupMessage, BackupStats, BackupStatus, Catalog, Dashboard, Discovered, Draft, Engine, Error, Event, FlagKind, FollowUp, Mailbox, MessageDetail, MessageSummary, NewAccount,
    Manifest, OAuthIdentity, OAuthProvider, OpsStatus, Outgoing, Unsubscribed, RestoreFilter, RestoreRequest, RestoreStats, ThreadFilter, ThreadSummary,
};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_opener::OpenerExt;

type Res<T> = Result<T, Error>;
type Eng<'a> = State<'a, Arc<Engine>>;

// Every command is `async` so it runs on Tauri's tokio runtime, which the engine needs to spawn
// background tasks; a sync command would run on the main thread and freeze the window.

/// Syncs, the queue, sending and store work run here, so the threads answering the UI are never
/// held up behind them (a long sync once made a search wait).
static ENGINE_RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();

pub(crate) fn engine_rt() -> &'static tokio::runtime::Handle {
    ENGINE_RT
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(8)
                .thread_name("facteur-engine")
                .enable_all()
                .build()
                .expect("engine runtime")
        })
        .handle()
}

macro_rules! on_engine {
    ($engine:expr, |$e:ident| $body:expr) => {{
        let $e = Arc::clone(&*$engine);
        on_engine(async move { $body }).await
    }};
}

async fn on_engine<T: Send + 'static>(work: impl std::future::Future<Output = Res<T>> + Send + 'static) -> Res<T> {
    engine_rt().spawn(work).await.map_err(|e| Error::Other(e.to_string()))?
}

/// On the engine runtime's blocking pool, so background work it starts (a queue replay) also
/// lands on the engine's threads.
async fn blocking<T: Send + 'static>(engine: &Arc<Engine>, job: impl FnOnce(&Arc<Engine>) -> Res<T> + Send + 'static) -> Res<T> {
    let engine = engine.clone();
    engine_rt().spawn_blocking(move || job(&engine)).await.map_err(|e| Error::Other(e.to_string()))?
}

#[tauri::command]
async fn accounts(engine: Eng<'_>) -> Res<Vec<Account>> {
    blocking(&engine, |e| e.accounts()).await
}

#[tauri::command]
async fn discover(email: String) -> Res<Discovered> {
    facteur_core::discover(&email).await
}

#[tauri::command]
async fn oauth_login(
    app: AppHandle,
    engine: Eng<'_>,
    provider: OAuthProvider,
    email: Option<String>,
    extended: Option<bool>,
) -> Res<OAuthIdentity> {
    engine
        .oauth_login(provider, email.as_deref(), extended.unwrap_or(false), |url| {
            app.opener().open_url(url, None::<&str>).map_err(|e| Error::Other(facteur_core::tr!("navigateur introuvable : {e}", "browser not found: {e}")))
        })
        .await
}

#[tauri::command]
async fn add_account(engine: Eng<'_>, account: NewAccount) -> Res<Account> {
    let e = engine.inner().clone();
    let account = on_engine(async move { e.add_account(account).await }).await?;
    let engine = engine.inner().clone();
    let id = account.id;
    engine_rt().spawn(async move {
        let _ = engine.sync(id).await;
        engine.ensure_listener(id);
    });
    Ok(account)
}

#[tauri::command]
async fn remove_account(engine: Eng<'_>, id: i64) -> Res<()> {
    blocking(&engine, move |e| e.remove_account(id)).await
}

#[tauri::command]
async fn mailboxes(engine: Eng<'_>, account_id: i64) -> Res<Vec<Mailbox>> {
    blocking(&engine, move |e| e.mailboxes(account_id)).await
}

#[tauri::command]
async fn threads(
    engine: Eng<'_>,
    mailbox_id: i64,
    filter: ThreadFilter,
    category: Option<facteur_core::Category>,
    limit: usize,
    offset: usize,
) -> Res<Vec<ThreadSummary>> {
    blocking(&engine, move |e| e.threads(mailbox_id, filter, category, limit, offset)).await
}

#[tauri::command]
async fn unified_threads(
    engine: Eng<'_>,
    filter: ThreadFilter,
    category: Option<facteur_core::Category>,
    limit: usize,
    offset: usize,
) -> Res<Vec<ThreadSummary>> {
    blocking(&engine, move |e| e.unified_threads(filter, category, limit, offset)).await
}

/// Without `mailbox_id`: the unified inbox.
#[tauri::command]
async fn category_unread(engine: Eng<'_>, mailbox_id: Option<i64>) -> Res<std::collections::HashMap<facteur_core::Category, i64>> {
    blocking(&engine, move |e| e.category_unread(mailbox_id)).await
}

#[tauri::command]
async fn set_sender_category(engine: Eng<'_>, email: String, category: facteur_core::Category) -> Res<()> {
    blocking(&engine, move |e| e.set_sender_category(&email, category)).await
}

#[tauri::command]
async fn sender_category(engine: Eng<'_>, email: String) -> Res<Option<facteur_core::Category>> {
    blocking(&engine, move |e| e.sender_category(&email)).await
}

#[tauri::command]
async fn done_threads(engine: Eng<'_>, filter: ThreadFilter, limit: usize, offset: usize) -> Res<Vec<ThreadSummary>> {
    blocking(&engine, move |e| e.done_threads(filter, limit, offset)).await
}

/// Moved messages not yet in their new folder get there first, instead of the action being refused
/// until the next sync. The wait is bounded: past it, settling carries on in the background.
async fn settle(engine: &Eng<'_>, ids: &[i64]) {
    let (e, ids) = (engine.inner().clone(), ids.to_vec());
    if let Err(e) = on_engine(async move { e.settle_bounded(&ids).await }).await {
        tracing::info!("moved messages not settled: {e}");
    }
}

#[tauri::command]
async fn to_inbox(engine: Eng<'_>, ids: Vec<i64>) -> Res<Option<u64>> {
    settle(&engine, &ids).await;
    blocking(&engine, move |e| e.to_inbox(&ids)).await
}

#[tauri::command]
async fn dashboard(engine: Eng<'_>, since: i64) -> Res<Dashboard> {
    blocking(&engine, move |e| e.dashboard(since, 6)).await
}

#[tauri::command]
async fn backup(engine: Eng<'_>, account_id: i64, dest: String) -> Res<BackupStats> {
    on_engine!(engine, |e| e.backup(account_id, Path::new(&dest)).await)
}

#[tauri::command]
async fn backup_busy(engine: Eng<'_>) -> Res<Vec<i64>> {
    Ok(engine.backup_busy())
}

#[tauri::command]
async fn backup_status(engine: Eng<'_>, dest: String) -> Res<Vec<BackupStatus>> {
    blocking(&engine, move |e| e.backup_status(Path::new(&dest))).await
}

#[tauri::command]
async fn list_backups(engine: Eng<'_>, dest: String) -> Res<Vec<Manifest>> {
    blocking(&engine, move |e| e.list_backups(Path::new(&dest))).await
}

#[tauri::command]
async fn backup_catalog(
    engine: Eng<'_>,
    dest: String,
    email: String,
    filter: RestoreFilter,
    limit: usize,
    offset: usize,
) -> Res<Catalog> {
    on_engine!(engine, |e| e.backup_catalog(Path::new(&dest), &email, filter, limit, offset).await)
}

#[tauri::command]
async fn backup_message(engine: Eng<'_>, dest: String, email: String, id: i64) -> Res<BackupMessage> {
    on_engine!(engine, |e| e.backup_message(Path::new(&dest), &email, id).await)
}

#[tauri::command]
async fn restore(
    engine: Eng<'_>,
    dest: String,
    source: String,
    account_id: i64,
    request: RestoreRequest,
) -> Res<RestoreStats> {
    on_engine!(engine, |e| e.restore(Path::new(&dest), &source, account_id, request).await)
}

#[tauri::command]
async fn unsubscribe(engine: Eng<'_>, id: i64, confirmed: Option<bool>) -> Res<Unsubscribed> {
    on_engine!(engine, |e| e.unsubscribe(id, confirmed.unwrap_or(false)).await)
}

#[tauri::command]
async fn report_spam(engine: Eng<'_>, ids: Vec<i64>) -> Res<Option<u64>> {
    settle(&engine, &ids).await;
    blocking(&engine, move |e| e.report_spam(&ids)).await
}

#[tauri::command]
async fn not_spam(engine: Eng<'_>, ids: Vec<i64>) -> Res<Option<u64>> {
    settle(&engine, &ids).await;
    blocking(&engine, move |e| e.not_spam(&ids)).await
}

#[tauri::command]
async fn block_sender(engine: Eng<'_>, email: String) -> Res<()> {
    blocking(&engine, move |e| e.block_sender(&email)).await
}

#[tauri::command]
async fn unblock_sender(engine: Eng<'_>, email: String) -> Res<()> {
    blocking(&engine, move |e| e.unblock_sender(&email)).await
}

#[tauri::command]
async fn blocked_senders(engine: Eng<'_>) -> Res<Vec<String>> {
    blocking(&engine, |e| e.blocked_senders()).await
}

#[tauri::command]
async fn sender_logos(engine: Eng<'_>, emails: Vec<String>) -> Res<std::collections::HashMap<String, Option<String>>> {
    blocking(&engine, move |e| e.sender_logos(&emails)).await
}

#[tauri::command]
async fn set_account_label(engine: Eng<'_>, account_id: i64, label: String) -> Res<()> {
    blocking(&engine, move |e| e.set_account_label(account_id, &label)).await
}

#[tauri::command]
async fn set_account_color(engine: Eng<'_>, account_id: i64, color: String) -> Res<()> {
    blocking(&engine, move |e| e.set_account_color(account_id, &color)).await
}

#[tauri::command]
async fn update_check(app: AppHandle) -> Res<Option<facteur_core::update::Available>> {
    selfupdate::check(&app).await
}

#[tauri::command]
async fn update_download(app: AppHandle) -> Res<()> {
    selfupdate::download(&app).await
}

#[tauri::command]
async fn update_apply(app: AppHandle) -> Res<()> {
    selfupdate::apply(&app)
}

#[tauri::command]
async fn update_discard() -> Res<()> {
    selfupdate::discard();
    Ok(())
}

#[tauri::command]
async fn reorder_accounts(engine: Eng<'_>, ids: Vec<i64>) -> Res<()> {
    blocking(&engine, move |e| e.reorder_accounts(&ids)).await
}

#[tauri::command]
async fn set_screener(engine: Eng<'_>, account_id: i64, on: bool) -> Res<()> {
    blocking(&engine, move |e| e.set_screener(account_id, on)).await
}

#[tauri::command]
async fn held_senders(engine: Eng<'_>, account_id: Option<i64>) -> Res<Vec<facteur_core::HeldSender>> {
    blocking(&engine, move |e| e.held_senders(account_id)).await
}

#[tauri::command]
async fn screen_sender(engine: Eng<'_>, account_id: i64, email: String, verdict: facteur_core::Screen, domain: bool) -> Res<Option<u64>> {
    on_engine!(engine, |e| e.screen_sender(account_id, &email, verdict, domain).await)
}

#[tauri::command]
async fn screening(engine: Eng<'_>, account_id: i64) -> Res<Vec<facteur_core::Screening>> {
    blocking(&engine, move |e| e.screening(account_id)).await
}

#[tauri::command]
async fn forget_screening(engine: Eng<'_>, account_id: i64, pattern: String) -> Res<()> {
    on_engine!(engine, |e| e.forget_screening(account_id, &pattern).await)
}

#[tauri::command]
async fn refresh_account_avatar(engine: Eng<'_>, account_id: i64) -> Res<bool> {
    on_engine!(engine, |e| e.refresh_account_avatar(account_id).await)
}

#[tauri::command]
async fn set_account_avatar(engine: Eng<'_>, account_id: i64, image: Option<String>) -> Res<()> {
    blocking(&engine, move |e| e.set_account_avatar(account_id, image.as_deref())).await
}

#[tauri::command]
async fn mark_account_read(engine: Eng<'_>, account_id: i64, inbox_only: Option<bool>) -> Res<usize> {
    blocking(&engine, move |e| e.mark_account_read(account_id, inbox_only.unwrap_or(false))).await
}

#[tauri::command]
async fn account_stats(engine: Eng<'_>, account_id: i64) -> Res<AccountStats> {
    blocking(&engine, move |e| e.account_stats(account_id)).await
}

#[tauri::command]
async fn similar_facets(engine: Eng<'_>, id: i64) -> Res<SimilarFacets> {
    on_engine!(engine, |e| e.similar_facets(id).await)
}

#[tauri::command]
async fn find_similar(engine: Eng<'_>, criteria: SimilarCriteria) -> Res<SimilarResult> {
    blocking(&engine, move |e| e.find_similar(&criteria)).await
}

#[tauri::command]
async fn similar_from_server(engine: Eng<'_>, criteria: SimilarCriteria) -> Res<usize> {
    on_engine!(engine, |e| e.similar_from_server(&criteria).await)
}

#[tauri::command]
async fn trusted_images(engine: Eng<'_>) -> Res<Vec<String>> {
    blocking(&engine, |e| e.trusted_images()).await
}

#[tauri::command]
async fn add_trusted_images(engine: Eng<'_>, entry: String) -> Res<String> {
    blocking(&engine, move |e| e.add_trusted_images(&entry)).await
}

#[tauri::command]
async fn remove_trusted_images(engine: Eng<'_>, key: String) -> Res<()> {
    blocking(&engine, move |e| e.remove_trusted_images(&key)).await
}

#[tauri::command]
async fn coverage(engine: Eng<'_>) -> Res<Vec<facteur_core::store::Coverage>> {
    blocking(&engine, |e| e.coverage()).await
}

#[tauri::command]
async fn history_busy(engine: Eng<'_>) -> Res<Vec<i64>> {
    Ok(engine.history_busy())
}

#[tauri::command]
async fn fetch_all_history(engine: Eng<'_>, account_id: i64) -> Res<usize> {
    on_engine!(engine, |e| e.fetch_all_history(account_id).await)
}

#[tauri::command]
async fn get_setting(engine: Eng<'_>, key: String) -> Res<Option<String>> {
    blocking(&engine, move |e| e.setting(&key)).await
}

#[tauri::command]
async fn ai_status(engine: Eng<'_>) -> Res<facteur_core::AiStatus> {
    blocking(&engine, |e| Ok(e.ai_status())).await
}

/// Downloads an offered model (`id`) and turns the local AI on with it; `ai_changed` follows.
#[tauri::command]
async fn ai_install(engine: Eng<'_>, id: String) -> Res<()> {
    on_engine!(engine, |e| e.ai_install(&id))
}

#[tauri::command]
async fn ai_cancel(engine: Eng<'_>) -> Res<()> {
    blocking(&engine, |e| {
        e.ai_cancel();
        Ok(())
    })
    .await
}

/// A GGUF model file of the user's own.
#[tauri::command]
async fn ai_use_file(engine: Eng<'_>, path: String) -> Res<()> {
    blocking(&engine, move |e| e.ai_use_file(&path)).await
}

#[tauri::command]
async fn ai_turn_off(engine: Eng<'_>) -> Res<()> {
    blocking(&engine, |e| e.ai_turn_off()).await
}

#[tauri::command]
async fn ai_summarize(engine: Eng<'_>, account_id: i64, thread_id: i64) -> Res<String> {
    on_engine!(engine, |e| e.ai_summarize(account_id, thread_id).await)
}

#[tauri::command]
async fn ai_graphics(engine: Eng<'_>, on: bool) -> Res<()> {
    blocking(&engine, move |e| e.set_ai_graphics(on)).await
}

#[tauri::command]
async fn dictation_off(engine: Eng<'_>) -> Res<()> {
    blocking(&engine, |e| e.dictation_off()).await
}

/// The recording as the raw body (16 kHz mono, 16-bit little-endian), its language in the
/// `language` header ("fr", "en"; none: found by the model).
#[tauri::command]
async fn ai_transcribe(engine: Eng<'_>, request: tauri::ipc::Request<'_>) -> Res<String> {
    let tauri::ipc::InvokeBody::Raw(pcm) = request.body() else {
        return Err(Error::Other("recording expected as raw bytes".into()));
    };
    let pcm = pcm.clone();
    let language = request.headers().get("language").and_then(|v| v.to_str().ok()).map(str::to_string);
    on_engine!(engine, |e| e.ai_transcribe(pcm, language).await)
}

#[tauri::command]
async fn ai_find_event(engine: Eng<'_>, id: i64) -> Res<Option<facteur_core::ai::FoundEvent>> {
    on_engine!(engine, |e| e.ai_find_event(id).await)
}

/// Accounts offered the single-connection mode (their server keeps dropping connections).
#[tauri::command]
async fn connection_advice(engine: Eng<'_>) -> Res<Vec<i64>> {
    blocking(&engine, move |e| Ok(e.connection_advice())).await
}

/// `accept`: `true` turns the mode on, `false` never offers it again, `null`: later.
#[tauri::command]
async fn answer_connection_advice(engine: Eng<'_>, account_id: i64, accept: Option<bool>) -> Res<()> {
    blocking(&engine, move |e| e.answer_connection_advice(account_id, accept)).await
}

#[tauri::command]
async fn set_setting(engine: Eng<'_>, key: String, value: String) -> Res<()> {
    blocking(&engine, move |e| e.set_setting(&key, &value)).await
}

#[tauri::command]
async fn mail_client_status() -> mailclient::MailClientStatus {
    mailclient::status()
}

#[tauri::command]
async fn register_mail_client(app: AppHandle) -> Result<(), String> {
    mailclient::register()?;
    app.opener()
        .open_url("ms-settings:defaultapps?registeredAppUser=facteur", None::<&str>)
        .map_err(|e| e.to_string())
}

/// A `mailto:` received at launch, kept until the UI is ready to open it.
#[tauri::command]
async fn take_launch_mailto(state: State<'_, LaunchMailto>) -> Res<Option<String>> {
    Ok(state.0.lock().unwrap_or_else(|e| e.into_inner()).take())
}

struct LaunchMailto(Mutex<Option<String>>);

#[tauri::command]
async fn reconnect(engine: Eng<'_>, account_id: i64, ticket: String) -> Res<Account> {
    on_engine!(engine, |e| e.reconnect(account_id, &ticket).await)
}

#[tauri::command]
async fn update_password(engine: Eng<'_>, account_id: i64, password: String) -> Res<()> {
    on_engine!(engine, |e| e.update_password(account_id, &password).await)
}

#[tauri::command]
async fn auth_problems(engine: Eng<'_>) -> Res<std::collections::HashMap<i64, String>> {
    Ok(engine.auth_problems())
}

/// Called by the UI when the machine wakes up or comes back online.
#[tauri::command]
async fn wake(engine: Eng<'_>) -> Res<()> {
    engine.resume();
    Ok(())
}

#[tauri::command]
async fn calendars(engine: Eng<'_>) -> Res<Vec<facteur_core::CalendarInfo>> {
    blocking(&engine, |e| e.calendars()).await
}

/// Occurrences overlapping [from, to) (Unix seconds) in the shown calendars, by start.
#[tauri::command]
async fn calendar_events(engine: Eng<'_>, from: i64, to: i64) -> Res<Vec<facteur_core::CalendarEvent>> {
    blocking(&engine, move |e| e.calendar_events(from, to)).await
}

#[tauri::command]
async fn set_calendar_enabled(engine: Eng<'_>, id: i64, enabled: bool) -> Res<()> {
    blocking(&engine, move |e| e.set_calendar_enabled(id, enabled)).await
}

/// Whether the user uses an account's calendar (ignored: left out, never synced).
#[tauri::command]
async fn set_calendar_ignored(engine: Eng<'_>, account_id: i64, ignored: bool) -> Res<()> {
    blocking(&engine, move |e| e.set_calendar_ignored(account_id, ignored)).await
}

/// The engine then emits `calendar_changed`.
#[tauri::command]
async fn sync_calendars(engine: Eng<'_>, account_id: Option<i64>) -> Res<()> {
    on_engine!(engine, |e| e.sync_calendars(account_id).await)
}

/// Returns the new event's id; the engine then emits `calendar_changed`.
#[tauri::command]
async fn create_event(engine: Eng<'_>, calendar_id: i64, draft: facteur_core::EventDraft, key: String) -> Res<Option<i64>> {
    on_engine!(engine, |e| e.create_event(calendar_id, draft, &key).await)
}

/// `series`: the whole series of an occurrence, else that occurrence alone.
#[tauri::command]
async fn update_event(engine: Eng<'_>, event: facteur_core::EventRef, draft: facteur_core::EventDraft, series: bool) -> Res<()> {
    on_engine!(engine, |e| e.update_event(&event, draft, series).await)
}

#[tauri::command]
async fn event_repeat(engine: Eng<'_>, event: facteur_core::EventRef) -> Res<facteur_core::SeriesRepeat> {
    on_engine!(engine, |e| e.event_repeat(&event).await)
}

#[tauri::command]
async fn delete_event(engine: Eng<'_>, event: facteur_core::EventRef, series: bool) -> Res<()> {
    on_engine!(engine, |e| e.delete_event(&event, series).await)
}

/// `answer`: `accepted`, `tentative` or `declined`.
#[tauri::command]
async fn respond_event(engine: Eng<'_>, event: facteur_core::EventRef, answer: String, series: bool) -> Res<()> {
    on_engine!(engine, |e| e.respond_event(&event, &answer, series).await)
}

#[tauri::command]
async fn address_book_status(engine: Eng<'_>) -> Res<Vec<facteur_core::AddressBookStatus>> {
    blocking(&engine, |e| e.address_book_status()).await
}

/// Read again now, whatever the last sync.
#[tauri::command]
async fn sync_address_books(engine: Eng<'_>, account_id: Option<i64>) -> Res<()> {
    on_engine!(engine, |e| e.sync_address_books(account_id, true).await)
}

#[tauri::command]
async fn calendar_status(engine: Eng<'_>) -> Res<Vec<facteur_core::CalendarStatus>> {
    blocking(&engine, |e| e.calendar_status()).await
}

/// `None` goes back to discovery.
#[tauri::command]
async fn set_caldav_url(engine: Eng<'_>, account_id: i64, url: Option<String>) -> Res<()> {
    on_engine!(engine, |e| e.set_caldav_url(account_id, url).await)
}

#[tauri::command]
async fn ops_status(engine: Eng<'_>) -> Res<OpsStatus> {
    blocking(&engine, |e| e.ops_status()).await
}

#[tauri::command]
async fn ops_details(engine: Eng<'_>) -> Res<facteur_core::OpsDetails> {
    blocking(&engine, |e| e.ops_details()).await
}

#[tauri::command]
async fn retry_op(engine: Eng<'_>, id: i64) -> Res<()> {
    on_engine!(engine, |e| e.retry_op(id).await)
}

#[tauri::command]
async fn discard_op(engine: Eng<'_>, id: i64, created_at: i64) -> Res<()> {
    blocking(&engine, move |e| e.discard_op(id, created_at)).await
}

#[tauri::command]
async fn undo(engine: Eng<'_>, token: u64) -> Res<()> {
    on_engine!(engine, |e| e.undo(token).await)
}

#[tauri::command]
async fn create_folder(engine: Eng<'_>, account_id: i64, parent: Option<i64>, name: String) -> Res<i64> {
    blocking(&engine, move |e| e.create_folder(account_id, parent, &name)).await
}

#[tauri::command]
async fn rename_folder(engine: Eng<'_>, mailbox_id: i64, name: String) -> Res<()> {
    blocking(&engine, move |e| e.rename_folder(mailbox_id, &name)).await
}

#[tauri::command]
async fn rules(engine: Eng<'_>) -> Res<Vec<facteur_core::rules::Rule>> {
    blocking(&engine, |e| e.rules()).await
}

#[tauri::command]
async fn save_rule(engine: Eng<'_>, rule: facteur_core::rules::Rule) -> Res<i64> {
    blocking(&engine, move |e| e.save_rule(rule)).await
}

#[tauri::command]
async fn delete_rule(engine: Eng<'_>, id: i64) -> Res<()> {
    blocking(&engine, move |e| e.delete_rule(id)).await
}

#[tauri::command]
async fn reorder_rules(engine: Eng<'_>, ids: Vec<i64>) -> Res<()> {
    blocking(&engine, move |e| e.reorder_rules(&ids)).await
}

#[tauri::command]
async fn preview_rule(engine: Eng<'_>, rule: facteur_core::rules::Rule) -> Res<facteur_core::rules::Preview> {
    blocking(&engine, move |e| e.preview_rule(rule)).await
}

#[tauri::command]
async fn run_rule(engine: Eng<'_>, id: i64) -> Res<usize> {
    blocking(&engine, move |e| e.run_rule(id)).await
}

#[tauri::command]
async fn set_identities(engine: Eng<'_>, account_id: i64, identities: Vec<facteur_core::Identity>) -> Res<()> {
    blocking(&engine, move |e| e.set_identities(account_id, identities)).await
}

#[tauri::command]
async fn respond_invite(engine: Eng<'_>, id: i64, partstat: String) -> Res<()> {
    on_engine!(engine, |e| e.respond_invite(id, &partstat).await)
}

#[tauri::command]
async fn save_invite(app: AppHandle, engine: Eng<'_>, id: i64, name: String) -> Res<Option<String>> {
    let raw = on_engine!(engine, |e| e.message_source(id).await)?;
    let data = blocking(&engine, move |_| {
        facteur_core::mime::parse_body(&raw)?.calendar.ok_or_else(|| Error::Other(facteur_core::tr!("ce message ne contient pas d'invitation", "this message contains no invitation")))
    })
    .await?;
    save_as(&app, &name, (&facteur_core::tr!("Événement", "Event"), &["ics"]), data.into_bytes()).await
}

/// The save dialog is opened here, not by the UI: the UI never supplies a path, so a hijacked
/// script cannot write anywhere on disk.
async fn save_as(app: &AppHandle, name: &str, filter: (&str, &[&str]), data: Vec<u8>) -> Res<Option<String>> {
    use tauri_plugin_dialog::DialogExt;
    // Suggested name only: no folder, no invisible or bidi character, nothing Windows refuses.
    let name = facteur_core::attachments::safe_filename(name);
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut dialog = app.dialog().file().set_file_name(if name.is_empty() { facteur_core::tr!("sans-nom", "untitled") } else { name });
    if !filter.1.is_empty() {
        dialog = dialog.add_filter(filter.0, filter.1);
    }
    dialog.save_file(move |path| {
        let _ = tx.send(path);
    });
    let Some(path) = rx.await.ok().flatten() else { return Ok(None) };
    let shown = path.to_string();
    // Through the fs plugin: on Android the dialog returns a `content://` URI (Storage Access
    // Framework), which is no file path; the plugin asks Android for a writable descriptor.
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || -> std::io::Result<()> {
        use std::io::Write;
        use tauri_plugin_fs::{FsExt, OpenOptions};
        #[cfg(windows)]
        let local = path.as_path().map(Path::to_path_buf);
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        let mut file = app.fs().open(path, options)?;
        file.write_all(&data)?;
        file.flush()?;
        drop(file);
        #[cfg(windows)]
        if let Some(local) = local {
            mark_of_the_web(&local);
        }
        Ok(())
    })
    .await
    .map_err(|e| Error::Other(e.to_string()))??;
    Ok(Some(shown))
}

/// Windows: the Mark of the Web on a saved file, the `Zone.Identifier` stream with zone 3
/// (Internet), so SmartScreen and Office's Protected View treat it as downloaded, as a browser
/// would. No `HostUrl`/`ReferrerUrl`: nothing to say where it came from. Ignored where alternate
/// streams do not exist (FAT, some network shares).
#[cfg(windows)]
fn mark_of_the_web(path: &Path) {
    let mut stream = path.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    if let Err(e) = std::fs::write(&stream, "[ZoneTransfer]\r\nZoneId=3\r\n") {
        tracing::debug!("no Mark of the Web: {e}");
    }
}

#[tauri::command]
async fn message_source(engine: Eng<'_>, id: i64) -> Res<String> {
    let raw = on_engine!(engine, |e| e.message_source(id).await)?;
    let mut text = String::from_utf8_lossy(&raw[..raw.len().min(5 << 20)]).into_owned();
    if raw.len() > 5 << 20 {
        text.push_str(&facteur_core::tr!("\n\n[… tronqué : message de plus de 5 Mo, enregistrez-le pour le lire en entier]", "\n\n[… truncated: message larger than 5 MB, save it to read it in full]"));
    }
    Ok(text)
}

#[tauri::command]
async fn save_message_source(app: AppHandle, engine: Eng<'_>, id: i64, name: String) -> Res<Option<String>> {
    let data = on_engine!(engine, |e| e.message_source(id).await)?;
    save_as(&app, &name, ("Message", &["eml"]), data).await
}

#[tauri::command]
async fn set_signature(engine: Eng<'_>, account_id: i64, signature: String) -> Res<()> {
    blocking(&engine, move |e| e.set_signature(account_id, &signature)).await
}

#[tauri::command]
async fn contacts(engine: Eng<'_>, query: String) -> Res<Vec<Address>> {
    blocking(&engine, move |e| e.contacts(&query)).await
}

#[tauri::command]
async fn save_draft(engine: Eng<'_>, draft: Draft) -> Res<String> {
    on_engine!(engine, |e| e.save_draft(draft).await)
}

#[tauri::command]
async fn discard_draft(engine: Eng<'_>, account_id: i64, draft_id: String) -> Res<()> {
    blocking(&engine, move |e| e.discard_draft(account_id, &draft_id)).await
}

#[tauri::command]
async fn open_draft(engine: Eng<'_>, id: i64) -> Res<Draft> {
    on_engine!(engine, |e| e.open_draft(id).await)
}

#[tauri::command]
async fn load_older(engine: Eng<'_>, mailbox_id: i64) -> Res<usize> {
    on_engine!(engine, |e| e.load_older(mailbox_id).await)
}

#[tauri::command]
async fn search_server(engine: Eng<'_>, account_id: i64, query: String, bins: Option<bool>) -> Res<Vec<MessageSummary>> {
    on_engine!(engine, |e| e.search_server(account_id, &query, bins.unwrap_or(false)).await)
}

#[tauri::command]
async fn thread(engine: Eng<'_>, account_id: i64, thread_id: i64, prefer: Option<i64>) -> Res<Vec<MessageSummary>> {
    blocking(&engine, move |e| e.thread(account_id, thread_id, prefer)).await
}

#[tauri::command]
async fn message(engine: Eng<'_>, id: i64) -> Res<MessageDetail> {
    // Most moved messages open from the cache: the move is settled only when it is needed.
    match on_engine!(engine, |e| e.message(id).await) {
        Err(_) if engine.is_moving(id) => {
            settle(&engine, &[id]).await;
            on_engine!(engine, |e| e.message(id).await)
        }
        res => res,
    }
}

#[tauri::command]
async fn search(engine: Eng<'_>, account_id: i64, query: String, bins: Option<bool>) -> Res<Vec<MessageSummary>> {
    blocking(&engine, move |e| e.search(account_id, &query, bins.unwrap_or(false))).await
}

#[tauri::command]
async fn set_flag(engine: Eng<'_>, ids: Vec<i64>, flag: FlagKind, value: bool) -> Res<()> {
    blocking(&engine, move |e| e.set_flag(&ids, flag, value)).await
}

/// `from`: the folder the messages are moved from (Gmail: the label they leave).
#[tauri::command]
async fn move_to(engine: Eng<'_>, ids: Vec<i64>, mailbox_id: i64, from: Option<i64>) -> Res<Option<u64>> {
    settle(&engine, &ids).await;
    blocking(&engine, move |e| e.move_to(&ids, mailbox_id, from)).await
}

#[tauri::command]
async fn set_labels(engine: Eng<'_>, ids: Vec<i64>, add: Vec<i64>, remove: Vec<i64>) -> Res<Option<u64>> {
    settle(&engine, &ids).await;
    blocking(&engine, move |e| e.set_labels(&ids, &add, &remove)).await
}

#[tauri::command]
async fn message_labels(engine: Eng<'_>, ids: Vec<i64>) -> Res<std::collections::HashMap<i64, Vec<i64>>> {
    blocking(&engine, move |e| e.message_labels(&ids)).await
}

#[tauri::command]
async fn archive(engine: Eng<'_>, ids: Vec<i64>) -> Res<Option<u64>> {
    settle(&engine, &ids).await;
    blocking(&engine, move |e| e.archive(&ids)).await
}

/// Muting also archives `ids`.
#[tauri::command]
async fn mute_thread(engine: Eng<'_>, account_id: i64, thread_id: i64, muted: bool, ids: Vec<i64>) -> Res<Option<u64>> {
    settle(&engine, &ids).await;
    blocking(&engine, move |e| e.mute_thread(account_id, thread_id, muted, &ids)).await
}

#[tauri::command]
async fn thread_muted(engine: Eng<'_>, account_id: i64, thread_id: i64) -> Res<bool> {
    blocking(&engine, move |e| e.thread_muted(account_id, thread_id)).await
}

#[tauri::command]
async fn trash(engine: Eng<'_>, ids: Vec<i64>, purge: Option<bool>) -> Res<Option<u64>> {
    settle(&engine, &ids).await;
    blocking(&engine, move |e| e.trash(&ids, purge.unwrap_or(false))).await
}

#[tauri::command]
async fn trash_folder(engine: Eng<'_>, mailbox_id: i64) -> Res<Option<u64>> {
    blocking(&engine, move |e| e.trash_folder(mailbox_id)).await
}

#[tauri::command]
async fn send(engine: Eng<'_>, draft: Draft, send_at: Option<i64>, remind_after: Option<i64>) -> Res<Option<i64>> {
    on_engine!(engine, |e| e.send(draft, send_at, remind_after).await)
}

#[tauri::command]
async fn folder_summary(engine: Eng<'_>, mailbox_id: i64, recent: Option<usize>) -> Res<facteur_core::FolderSummary> {
    blocking(&engine, move |e| e.folder_summary(mailbox_id, recent)).await
}

#[tauri::command]
async fn folder_messages_from(engine: Eng<'_>, mailbox_id: i64, from: String, limit: Option<usize>) -> Res<Vec<MessageSummary>> {
    blocking(&engine, move |e| e.folder_messages_from(mailbox_id, &from, limit)).await
}

#[tauri::command]
async fn empty_folder(engine: Eng<'_>, mailbox_id: i64) -> Res<usize> {
    blocking(&engine, move |e| e.empty_folder(mailbox_id)).await
}

#[tauri::command]
async fn delete_folder(engine: Eng<'_>, mailbox_id: i64, dest: Option<i64>) -> Res<()> {
    blocking(&engine, move |e| e.delete_folder(mailbox_id, dest)).await
}

#[tauri::command]
async fn inspect_link(engine: Eng<'_>, message_id: i64, href: String, text: String) -> Res<facteur_core::links::LinkReport> {
    on_engine!(engine, |e| e.inspect_link(message_id, &href, &text).await)
}

/// Checked before a link is opened: the hover card's analysis, without any lookup. `None` for a
/// non-web link.
#[tauri::command]
fn check_link(href: String, text: String) -> Option<facteur_core::links::LinkFacts> {
    facteur_core::links::analyze(&href, &text)
}

#[tauri::command]
async fn outbox(engine: Eng<'_>) -> Res<Vec<Outgoing>> {
    blocking(&engine, |e| e.outbox()).await
}

#[tauri::command]
async fn cancel_send(engine: Eng<'_>, id: i64) -> Res<Draft> {
    blocking(&engine, move |e| e.cancel_send(id)).await
}

#[tauri::command]
async fn send_now(engine: Eng<'_>, id: i64) -> Res<()> {
    blocking(&engine, move |e| e.send_now(id)).await
}

#[tauri::command]
async fn snooze(engine: Eng<'_>, ids: Vec<i64>, until: i64) -> Res<Option<u64>> {
    settle(&engine, &ids).await;
    blocking(&engine, move |e| e.snooze(&ids, until)).await
}

#[tauri::command]
async fn unsnooze(engine: Eng<'_>, account_id: i64, thread_id: i64) -> Res<()> {
    blocking(&engine, move |e| e.unsnooze(account_id, thread_id)).await
}

#[tauri::command]
async fn follow_ups(engine: Eng<'_>) -> Res<Vec<FollowUp>> {
    blocking(&engine, |e| e.follow_ups()).await
}

#[tauri::command]
async fn dismiss_follow_up(engine: Eng<'_>, id: i64) -> Res<()> {
    blocking(&engine, move |e| e.dismiss_follow_up(id)).await
}

#[tauri::command]
async fn sync(engine: Eng<'_>, account_id: i64) -> Res<()> {
    on_engine!(engine, |e| e.sync(account_id).await).map(|_| ())
}

/// Parsed on a blocking thread: a message can weigh tens of MB.
async fn inspected_attachment(engine: &Arc<Engine>, id: i64, index: usize) -> Res<(facteur_core::attachments::AttachmentReport, Vec<u8>)> {
    let raw = on_engine!(engine, |e| e.message_source(id).await)?;
    blocking(engine, move |e| {
        let (filename, declared, data) = facteur_core::mime::attachment(&raw, index)?;
        let trust = e.trust(id, None).ok().map(|t| t.level);
        Ok((facteur_core::attachments::inspect(&filename, &declared, &data, trust), data))
    })
    .await
}

#[tauri::command]
async fn inspect_attachment(engine: Eng<'_>, id: i64, index: usize) -> Res<facteur_core::attachments::AttachmentReport> {
    Ok(inspected_attachment(&engine, id, index).await?.0)
}

/// Raw bytes (no JSON), framed with the type sniffed from the content, never the one the sender
/// declared: `[n: u8][n bytes: MIME type, ASCII][content]`. The UI builds its `blob:` URL with
/// that type. Refused for anything but images, PDF and plain text.
#[tauri::command]
async fn attachment_view(engine: Eng<'_>, id: i64, index: usize) -> Res<tauri::ipc::Response> {
    let (report, data) = inspected_attachment(&engine, id, index).await?;
    let mime = report.view_mime.ok_or_else(|| {
        Error::Other(facteur_core::tr!("cette pièce jointe ne peut pas être affichée dans facteur", "this attachment cannot be displayed in facteur"))
    })?;
    // The viewer reads text as UTF-8, whatever the file's own encoding (its declared charset first).
    let data = if mime == "text/plain" {
        facteur_core::mime::text_as_utf8(&data, facteur_core::mime::mime_charset(&report.declared_mime))
    } else {
        std::borrow::Cow::Borrowed(&data[..])
    };
    Ok(tauri::ipc::Response::new(frame_view(mime, &data)))
}

fn frame_view(mime: &str, data: &[u8]) -> Vec<u8> {
    let mime = &mime.as_bytes()[..mime.len().min(255)];
    let mut out = Vec::with_capacity(1 + mime.len() + data.len());
    out.push(mime.len() as u8);
    out.extend_from_slice(mime);
    out.extend_from_slice(data);
    out
}

/// Saves an attachment. A suspicious one (low trust) gets a native warning first, whatever the
/// UI did, unless the caller says it has already shown that warning (`confirmed`).
#[tauri::command]
async fn save_attachment(app: AppHandle, engine: Eng<'_>, id: i64, index: usize, confirmed: Option<bool>) -> Res<Option<Saved>> {
    let (report, data) = inspected_attachment(&engine, id, index).await?;
    if report.trust.is_low() && !confirmed.unwrap_or(false) && !confirm_suspicious(&app, &report).await {
        return Ok(None);
    }
    let Some(path) = save_as(&app, &report.filename, ("", &[]), data).await? else { return Ok(None) };
    let openable = facteur_core::attachments::openable(&report, &path);
    app.state::<SavedFiles>().0.lock().unwrap_or_else(|e| e.into_inner()).insert(path.clone(), openable);
    Ok(Some(Saved { path, openable }))
}

#[derive(serde::Serialize)]
struct Saved {
    path: String,
    openable: bool,
}

/// Attachments saved in this session, by path, and whether each may be opened from facteur. The
/// interface can only open these: a path it makes up, or one of a program, is refused.
struct SavedFiles(Mutex<std::collections::HashMap<String, bool>>);

#[tauri::command]
async fn open_saved(app: AppHandle, path: String) -> Res<()> {
    let allowed = app.state::<SavedFiles>().0.lock().unwrap_or_else(|e| e.into_inner()).get(&path).copied().unwrap_or(false);
    if !allowed {
        return Err(Error::Other(facteur_core::tr!("ce fichier ne s'ouvre pas depuis facteur", "this file cannot be opened from facteur")));
    }
    app.opener()
        .open_path(path, None::<&str>)
        .map_err(|e| Error::Other(facteur_core::tr!("impossible d'ouvrir le fichier : {e}", "could not open the file: {e}")))
}

async fn confirm_suspicious(app: &AppHandle, report: &facteur_core::attachments::AttachmentReport) -> bool {
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
    let why: Vec<String> = report.trust.reasons.iter().filter(|r| !r.positive).map(|r| format!("• {}", r.text)).collect();
    let text = format!("{}\n\n{}", why.join("\n"), facteur_core::tr!("Enregistrer quand même ?", "Save anyway?"));
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .message(text)
        .title(facteur_core::tr!("Pièce jointe suspecte", "Suspicious attachment"))
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(facteur_core::tr!("Enregistrer", "Save"), facteur_core::tr!("Annuler", "Cancel")))
        .show(move |ok| {
            let _ = tx.send(ok);
        });
    rx.await.unwrap_or(false)
}

/// Where the main webview may go: the app's own pages (`tauri://localhost`, or
/// `http(s)://tauri.localhost` on Windows and Android), the asset protocol, `about:blank`/`srcdoc`
/// frames, and the dev server in development. Anything else (a link, a redirect, a script) would
/// leave the app with the IPC still reachable. No `blob:` either: the app only uses them as image
/// sources, and an attachment shown that way (an SVG) must never become a page of the app's origin.
fn is_app_url(url: &tauri::Url, dev: Option<&tauri::Url>) -> bool {
    let same_origin = |a: &tauri::Url, b: &tauri::Url| {
        a.scheme() == b.scheme() && a.host_str() == b.host_str() && a.port_or_known_default() == b.port_or_known_default()
    };
    match url.scheme() {
        "tauri" | "asset" => url.host_str() == Some("localhost") && url.port().is_none(),
        "http" | "https" => {
            (matches!(url.host_str(), Some("tauri.localhost" | "asset.localhost")) && url.port().is_none())
                || dev.is_some_and(|d| same_origin(url, d))
        }
        "about" => matches!(url.path(), "blank" | "srcdoc"),
        _ => false,
    }
}

fn navigation_guard<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::new("navigation-guard")
        .on_navigation(|webview, url| {
            let dev = if tauri::is_dev() { webview.config().build.dev_url.clone() } else { None };
            if is_app_url(url, dev.as_ref()) {
                return true;
            }
            tracing::warn!(scheme = url.scheme(), host = url.host_str().unwrap_or(""), "navigation blocked");
            if matches!(url.scheme(), "http" | "https" | "mailto") {
                let _ = webview.app_handle().opener().open_url(url.as_str(), None::<&str>);
            }
            false
        })
        .build()
}

/// The database could not be opened at startup: said in a native dialog (in both languages: the
/// language setting lives in that database), with the logs folder. A damaged file can be moved
/// aside, facteur then starts again on an empty one (the mail comes back from the servers).
fn startup_failed(app: AppHandle, data_dir: &Path, error: Error) {
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
    tracing::error!("cannot open the database: {error:?}");
    let damaged = error.is_damaged_db();
    let detail = error.to_string();
    let logs = data_dir.join("logs");
    let data_dir = data_dir.to_path_buf();
    // `blocking_show` must not run on the main thread: the event loop draws the dialog.
    std::thread::spawn(move || {
        let title = "facteur";
        let logs = logs.display();
        if damaged {
            let text = format!(
                "La base de données de facteur est endommagée.\n\
                 facteur peut la mettre de côté (le fichier est gardé, renommé) et repartir d'une base vide. \
                 Les comptes seront à ajouter de nouveau, leurs messages reviendront des serveurs ; \
                 les réglages, règles et la boîte d'envoi locaux repartent à zéro.\n\
                 Journaux : {logs}\n\n\
                 facteur's database is damaged.\n\
                 facteur can set it aside (the file is kept, renamed) and start again with an empty one. \
                 The accounts will have to be added again, their mail comes back from the servers; \
                 local settings, rules and the outbox start over.\n\
                 Logs: {logs}"
            );
            let set_aside = app
                .dialog()
                .message(text)
                .title(title)
                .kind(MessageDialogKind::Error)
                .buttons(MessageDialogButtons::OkCancelCustom("Repartir de zéro / Start fresh".into(), "Quitter / Quit".into()))
                .blocking_show();
            if set_aside {
                match Engine::set_aside_database(&data_dir) {
                    Ok(aside) => {
                        tracing::warn!("damaged database moved to {}", aside.display());
                        app.restart();
                    }
                    Err(e) => {
                        tracing::error!("cannot move the damaged database aside: {e}");
                        app.dialog()
                            .message(format!(
                                "Impossible de mettre la base de côté : {e}\nJournaux : {logs}\n\n\
                                 The database could not be set aside: {e}\nLogs: {logs}"
                            ))
                            .title(title)
                            .kind(MessageDialogKind::Error)
                            .blocking_show();
                    }
                }
            }
        } else {
            let text = format!(
                "facteur ne peut pas démarrer : sa base de données n'a pas pu être ouverte.\n{detail}\nJournaux : {logs}\n\n\
                 facteur cannot start: its database could not be opened.\n{detail}\nLogs: {logs}"
            );
            app.dialog().message(text).title(title).kind(MessageDialogKind::Error).blocking_show();
        }
        app.exit(1);
    });
}

/// Logs to a file too (a week kept): the released exe has no console, and testers' problems must
/// leave a trace.
fn init_logging(data_dir: &Path) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::prelude::*;
    let filter = || {
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "facteur_core=info,facteur_app_lib=info,ui=info".into())
    };
    let file = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("facteur")
        .filename_suffix("log")
        .max_log_files(7)
        .build(data_dir.join("logs"));
    let terminal = tracing_subscriber::fmt::layer().with_filter(filter());
    let guard = match file {
        Ok(appender) => {
            let (writer, guard) = tracing_appender::non_blocking(appender);
            let file = tracing_subscriber::fmt::layer().with_ansi(false).with_writer(writer).with_filter(filter());
            let _ = tracing_subscriber::registry().with(terminal).with(file).try_init();
            Some(guard)
        }
        Err(e) => {
            let _ = tracing_subscriber::registry().with(terminal).try_init();
            tracing::warn!("no log file: {e}");
            None
        }
    };
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("panic: {info}");
        default_hook(info);
    }));
    guard
}

/// Dropped on `RunEvent::Exit` so the last lines reach the file: a managed state is never dropped
/// otherwise.
struct LogGuard(Mutex<Option<tracing_appender::non_blocking::WorkerGuard>>);

#[tauri::command]
fn log_ui(level: String, message: String) {
    // One line per entry: a newline in the text must not forge a log line of its own.
    let message: String = message
        .chars()
        .take(4000)
        .flat_map(|c| match c {
            '\n' => vec!['\\', 'n'],
            '\r' => vec!['\\', 'r'],
            c if (c.is_control() && c != '\t') || matches!(c, '\u{2028}' | '\u{2029}') => vec!['\u{FFFD}'],
            c => vec![c],
        })
        .collect();
    match level.as_str() {
        "error" => tracing::error!(target: "ui", "{message}"),
        "warn" => tracing::warn!(target: "ui", "{message}"),
        _ => tracing::info!(target: "ui", "{message}"),
    }
}

#[tauri::command]
fn perf_enabled() -> bool {
    facteur_core::perf::ENABLED
}

/// `since`: Unix ms.
#[tauri::command]
fn perf_events(since: i64) -> Vec<facteur_core::perf::PerfEvent> {
    facteur_core::perf::since(since)
}

/// Must stay read-only and cheap: polled while the diagnostics panel is open.
#[tauri::command]
async fn diagnostics(app: AppHandle, engine: Eng<'_>) -> Res<facteur_core::Diagnostics> {
    let version = app.package_info().version.to_string();
    let mut d = blocking(&engine, |e| e.diagnostics()).await?;
    d.app_version = version;
    Ok(d)
}

/// `level`: comma-separated, none means all. Oldest first; only the end of the newest file is read.
#[tauri::command]
async fn read_log(app: AppHandle, lines: usize, level: Option<String>, query: Option<String>) -> Res<Vec<facteur_core::logs::LogLine>> {
    let dir = app.path().app_data_dir().map_err(|e| Error::Other(e.to_string()))?.join("logs");
    engine_rt()
        .spawn_blocking(move || facteur_core::logs::read_tail(&dir, lines, level.as_deref(), query.as_deref()))
        .await
        .map_err(|e| Error::Other(e.to_string()))?
}

#[tauri::command]
fn open_logs(app: AppHandle) -> Res<()> {
    let dir = app.path().app_data_dir().map_err(|e| Error::Other(e.to_string()))?.join("logs");
    std::fs::create_dir_all(&dir)?;
    app.opener().open_path(dir.to_string_lossy(), None::<&str>).map_err(|e| Error::Other(e.to_string()))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {

    let builder = tauri::Builder::default();
    // Registered first: a second launch (a mailto: link) is handed to the open window.
    // In the Flatpak, the sandbox only lets the app own bus names under its own id: under the
    // configured identifier, the name is refused and every launch would start a second engine on
    // the same database.
    #[cfg(desktop)]
    let builder = builder.plugin({
        let single = tauri_plugin_single_instance::Builder::new().callback(|app, argv, _cwd| {
            notify::focus(app);
            if let Some(link) = mailclient::mailto_arg(&argv) {
                let _ = app.emit("mailto", link);
            }
        });
        match std::env::var("FLATPAK_ID").ok().filter(|id| !id.is_empty()) {
            Some(id) => single.dbus_id(id).build(),
            None => single.build(),
        }
    });
    // Window geometry persists, not its visibility: it opens hidden until the UI has drawn.
    #[cfg(desktop)]
    let builder = builder.plugin({
        use tauri_plugin_window_state::StateFlags;
        tauri_plugin_window_state::Builder::default().with_state_flags(StateFlags::all() & !StateFlags::VISIBLE).build()
    });
    builder
        .plugin(navigation_guard())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            app.manage(LogGuard(Mutex::new(init_logging(&data_dir))));
            tracing::info!("facteur {} starting", app.package_info().version);
            let engine = match Engine::open(&data_dir) {
                Ok(engine) => engine,
                // Told in a dialog, not a silent exit (nor a panic on the setup error).
                Err(e) => {
                    startup_failed(app.handle().clone(), &data_dir, e);
                    return Ok(());
                }
            };
            // The engine's background tasks (queue replays…) on its own runtime, even when a
            // command running on Tauri's runtime starts them.
            engine.set_runtime(engine_rt().clone());
            app.manage(engine.clone());
            let args: Vec<String> = std::env::args().collect();
            app.manage(LaunchMailto(Mutex::new(mailclient::mailto_arg(&args))));
            app.manage(SavedFiles(Mutex::default()));
            notify::register_app_id(&data_dir);
            selfupdate::cleanup();
            mailclient::refresh_if_registered();
            // The window opens hidden, shown by the interface once drawn (no white flash before
            // the first paint); shown anyway if the interface never says so.
            #[cfg(desktop)]
            {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_secs(5));
                    if let Some(window) = handle.get_webview_window("main") {
                        let _ = window.show();
                    }
                });
            }

            let handle = app.handle().clone();
            engine_rt().spawn(async move {
                let mut events = engine.subscribe();
                engine.start_background(Duration::from_secs(5 * 60));
                loop {
                    match events.recv().await {
                        Ok(event) => {
                            match &event {
                                Event::NewMail { account_id, messages } => {
                                    notify::new_mail(&handle, &engine, *account_id, messages.clone());
                                }
                                Event::FollowUpsDue { follow_ups } => notify::follow_ups(&handle, &engine, follow_ups),
                                Event::EventReminder { event, minutes } => notify::event_reminder(&handle, &engine, event, *minutes),
                                _ => {}
                            }
                            let _ = handle.emit("engine", event);
                        }
                        // Events were dropped (UI busy, burst of changes): the UI cannot know which,
                        // so it is told to reload everything and re-query busy states.
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                            tracing::warn!(missed, "engine events lagged: asking the UI to resync");
                            let _ = handle.emit("engine", serde_json::json!({ "type": "resync", "missed": missed }));
                        }
                        Err(_) => break,
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            accounts,
            discover,
            oauth_login,
            add_account,
            remove_account,
            mailboxes,
            threads,
            unified_threads,
            category_unread,
            set_sender_category,
            sender_category,
            done_threads,
            to_inbox,
            dashboard,
            backup,
            backup_status,
            backup_busy,
            restore,
            list_backups,
            ops_status,
            ops_details,
            unsubscribe,
            report_spam,
            not_spam,
            block_sender,
            unblock_sender,
            blocked_senders,
            get_setting,
            coverage,
            history_busy,
            fetch_all_history,
            trusted_images,
            similar_facets,
            find_similar,
            similar_from_server,
            add_trusted_images,
            remove_trusted_images,
            set_account_label,
            set_account_color,
            refresh_account_avatar,
            reorder_accounts,
            update_check,
            log_ui,
            perf_enabled,
            perf_events,
            diagnostics,
            read_log,
            open_logs,
            update_download,
            update_apply,
            update_discard,
            set_screener,
            held_senders,
            screen_sender,
            screening,
            forget_screening,
            set_account_avatar,
            mark_account_read,
            account_stats,
            sender_logos,
            set_setting,
            connection_advice,
            answer_connection_advice,
            ai_status,
            ai_install,
            ai_cancel,
            ai_use_file,
            ai_turn_off,
            ai_summarize,
            ai_find_event,
            dictation_off,
            ai_graphics,
            ai_transcribe,
            mail_client_status,
            register_mail_client,
            take_launch_mailto,
            reconnect,
            update_password,
            auth_problems,
            wake,
            calendars,
            calendar_events,
            set_calendar_enabled,
            set_calendar_ignored,
            sync_calendars,
            create_event,
            update_event,
            event_repeat,
            delete_event,
            respond_event,
            address_book_status,
            sync_address_books,
            calendar_status,
            set_caldav_url,
            retry_op,
            discard_op,
            set_signature,
            undo,
            create_folder,
            rename_folder,
            rules,
            save_rule,
            delete_rule,
            reorder_rules,
            run_rule,
            preview_rule,
            set_identities,
            respond_invite,
            save_invite,
            message_source,
            save_message_source,
            contacts,
            save_draft,
            discard_draft,
            open_draft,
            load_older,
            search_server,
            backup_catalog,
            backup_message,
            thread,
            message,
            search,
            set_flag,
            move_to,
            set_labels,
            message_labels,
            archive,
            mute_thread,
            thread_muted,
            trash,
            trash_folder,
            send,
            outbox,
            inspect_link,
            check_link,
            folder_summary,
            folder_messages_from,
            empty_folder,
            delete_folder,
            cancel_send,
            send_now,
            snooze,
            unsnooze,
            follow_ups,
            dismiss_follow_up,
            sync,
            save_attachment,
            open_saved,
            inspect_attachment,
            attachment_view,
        ])
        .build(tauri::generate_context!())
        .expect("failed to start facteur")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                tracing::info!("facteur exiting");
                // First: the AI process would finish a request, holding gigabytes meanwhile, and one
                // waiting would start another nothing stops.
                facteur_core::ai::shut_down();
                // The servers are told: Outlook would otherwise turn down the next launch's
                // sign-ins for a minute.
                if let Some(engine) = app.try_state::<Arc<Engine>>() {
                    let engine = engine.inner().clone();
                    tauri::async_runtime::block_on(async move { engine.sign_out().await });
                }
                if let Some(guard) = app.try_state::<LogGuard>() {
                    drop(guard.0.lock().unwrap_or_else(|e| e.into_inner()).take());
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_app_origin_is_navigable() {
        let ok = |u: &str, dev: Option<&str>| {
            let dev = dev.map(|d| tauri::Url::parse(d).unwrap());
            is_app_url(&tauri::Url::parse(u).unwrap(), dev.as_ref())
        };
        for u in ["tauri://localhost/mail/inbox", "http://tauri.localhost/", "https://tauri.localhost/settings", "asset://localhost/x", "about:blank", "about:srcdoc"] {
            assert!(ok(u, None), "{u}");
        }
        for u in ["https://evil.example/", "http://tauri.localhost.evil.example/", "http://tauri.localhost:8080/", "tauri://evil/", "data:text/html,<script>x</script>", "file:///etc/passwd", "javascript:alert(1)", "blob:https://evil.example/1", "blob:http://tauri.localhost/1234-5678", "about:config", "http://localhost:3000/"] {
            assert!(!ok(u, None), "{u}");
        }
        assert!(ok("http://localhost:3000/mail", Some("http://localhost:3000")));
        assert!(!ok("http://localhost:3001/", Some("http://localhost:3000")));
    }

    #[test]
    fn viewed_attachments_carry_their_sniffed_type() {
        let framed = frame_view("image/png", b"\x89PNG");
        assert_eq!(framed[0] as usize, "image/png".len());
        assert_eq!(&framed[1..10], b"image/png");
        assert_eq!(&framed[10..], b"\x89PNG");
    }
}
