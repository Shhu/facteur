//! Google Calendar API v3. `singleEvents` makes Google expand recurring events; an occurrence is
//! changed or deleted by its own id, leaving the rest of the series as it is.

use std::time::Duration;

use crate::calendar::{EventChanges, Instance, RemoteCalendar};
use crate::error::{Error, Result};

const API: &str = "https://www.googleapis.com/calendar/v3";
const MAX_PAGES: usize = 20;
const MAX_PAGE: usize = 16 * 1024 * 1024;

fn client() -> Result<reqwest::Client> {
    let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .user_agent("facteur")
        .build()
        .map_err(|e| Error::Other(e.to_string()))
}

/// Missing permission (a token from before facteur asked for the calendars, or the calendars
/// unticked at consent) maps to [`Error::Auth`]: the account must sign in again.
fn api_error(status: reqwest::StatusCode, body: &[u8]) -> Error {
    let json: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
    let error = &json["error"];
    let message = error["message"].as_str().unwrap_or_default().to_string();
    let reasons: Vec<&str> = error["errors"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| e["reason"].as_str())
        .chain(error["details"].as_array().into_iter().flatten().filter_map(|d| d["reason"].as_str()))
        .chain(error["status"].as_str())
        .collect();
    let has = |r: &str| reasons.iter().any(|x| x.eq_ignore_ascii_case(r));
    if status == reqwest::StatusCode::UNAUTHORIZED
        || has("insufficientPermissions")
        || has("ACCESS_TOKEN_SCOPE_INSUFFICIENT")
        || (status == reqwest::StatusCode::FORBIDDEN && message.to_ascii_lowercase().contains("insufficient authentication scopes"))
    {
        return Error::Auth(crate::tr!(
            "facteur n'a pas accès aux agendas de ce compte : reconnectez-le",
            "facteur has no access to this account's calendars: reconnect it"
        ));
    }
    if has("accessNotConfigured") || has("SERVICE_DISABLED") {
        return Error::Other(crate::tr!(
            "l'API Google Calendar n'est pas activée pour l'application",
            "the Google Calendar API is not enabled for the application"
        ));
    }
    if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::GONE {
        return Error::NotFound(crate::tr!("événement Google", "Google event"));
    }
    let detail = if message.is_empty() { status.to_string() } else { message };
    Error::Other(crate::tr!("Google Agenda : {detail}", "Google Calendar: {detail}"))
}

async fn get(http: &reqwest::Client, token: &str, url: reqwest::Url) -> Result<serde_json::Value> {
    send(http.get(url), token).await
}

async fn send(req: reqwest::RequestBuilder, token: &str) -> Result<serde_json::Value> {
    let (status, body) = send_raw(req, token).await?;
    json_of(status, &body)
}

async fn send_raw(req: reqwest::RequestBuilder, token: &str) -> Result<(reqwest::StatusCode, Vec<u8>)> {
    let resp = req
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| Error::Other(crate::tr!("Google Agenda injoignable : {e}", "Google Calendar unreachable: {e}")))?;
    let status = resp.status();
    Ok((status, crate::http::read_capped(resp, MAX_PAGE).await?.to_vec()))
}

fn json_of(status: reqwest::StatusCode, body: &[u8]) -> Result<serde_json::Value> {
    if !status.is_success() {
        return Err(api_error(status, body));
    }
    if body.is_empty() {
        return Ok(serde_json::Value::Null);
    }
    Ok(serde_json::from_slice(body)?)
}

async fn list(http: &reqwest::Client, token: &str, url: reqwest::Url) -> Result<Vec<serde_json::Value>> {
    Ok(pages(http, token, url).await?.iter().flat_map(|p| p["items"].as_array().cloned().unwrap_or_default()).collect())
}

/// Every page of a listing, as answered.
async fn pages(http: &reqwest::Client, token: &str, url: reqwest::Url) -> Result<Vec<serde_json::Value>> {
    let mut pages = Vec::new();
    let mut page: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut url = url.clone();
        if let Some(p) = &page {
            url.query_pairs_mut().append_pair("pageToken", p);
        }
        let json = get(http, token, url).await?;
        page = json["nextPageToken"].as_str().map(str::to_string);
        pages.push(json);
        if page.is_none() {
            break;
        }
    }
    Ok(pages)
}

pub async fn calendars(token: &str) -> Result<Vec<RemoteCalendar>> {
    let http = client()?;
    let mut url = reqwest::Url::parse(&format!("{API}/users/me/calendarList")).map_err(|e| Error::Other(e.to_string()))?;
    url.query_pairs_mut().append_pair("maxResults", "250");
    Ok(list(&http, token, url).await?.iter().filter_map(map_calendar).collect())
}

pub async fn events(token: &str, calendar_id: &str, from: i64, to: i64) -> Result<Vec<Instance>> {
    let http = client()?;
    let rfc3339 = |t: i64| chrono::DateTime::from_timestamp(t, 0).map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)).unwrap_or_default();
    let mut url = reqwest::Url::parse(API).map_err(|e| Error::Other(e.to_string()))?;
    url.path_segments_mut()
        .map_err(|_| Error::Other("URL".into()))?
        .extend(["calendars", calendar_id, "events"]);
    url.query_pairs_mut()
        .append_pair("singleEvents", "true")
        .append_pair("timeMin", &rfc3339(from))
        .append_pair("timeMax", &rfc3339(to))
        .append_pair("maxResults", "2500");
    let pages = pages(&http, token, url).await?;
    // The calendar's own reminders, for the events that keep them.
    let defaults = pages.first().map(|p| minutes(&p["defaultReminders"])).unwrap_or_default();
    Ok(pages
        .iter()
        .flat_map(|p| p["items"].as_array().cloned().unwrap_or_default())
        .filter_map(|item| {
            let mut event = map_event(&item)?;
            if item["reminders"]["useDefault"].as_bool() == Some(true) {
                event.reminders = defaults.clone();
            }
            Some(event)
        })
        .collect())
}

/// Whether an event of the calendar was added, changed or deleted since `since` (Unix seconds):
/// one small request in place of rereading the whole window.
pub async fn changed_since(token: &str, calendar_id: &str, since: i64) -> Result<bool> {
    let mut url = events_url(calendar_id, None)?;
    let since = chrono::DateTime::from_timestamp(since, 0).map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)).unwrap_or_default();
    url.query_pairs_mut()
        .append_pair("updatedMin", &since)
        .append_pair("showDeleted", "true")
        .append_pair("maxResults", "1")
        .append_pair("fields", "items(id)");
    let json = get(&client()?, token, url).await?;
    Ok(json["items"].as_array().is_some_and(|items| !items.is_empty()))
}

fn events_url(calendar_id: &str, event_id: Option<&str>) -> Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(API).map_err(|e| Error::Other(e.to_string()))?;
    {
        let mut path = url.path_segments_mut().map_err(|_| Error::Other("URL".into()))?;
        path.extend(["calendars", calendar_id, "events"]);
        if let Some(id) = event_id {
            path.push(id);
        }
    }
    Ok(url)
}

/// Where an event is written: its guests are told by Google (invitation, change, cancellation).
fn write_url(calendar_id: &str, event_id: Option<&str>) -> Result<reqwest::Url> {
    let mut url = events_url(calendar_id, event_id)?;
    url.query_pairs_mut().append_pair("sendUpdates", "all");
    Ok(url)
}

/// A bound of the event: `null` clears the other form, which a patch would otherwise keep.
fn bound(t: i64, day: Option<chrono::NaiveDate>) -> serde_json::Value {
    match day {
        Some(d) => serde_json::json!({ "date": d.format("%Y-%m-%d").to_string(), "dateTime": null, "timeZone": null }),
        None => {
            let at = chrono::DateTime::from_timestamp(t, 0).map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
            serde_json::json!({ "dateTime": at, "date": null })
        }
    }
}

fn body(changes: &EventChanges) -> Result<serde_json::Value> {
    let mut body = serde_json::Map::new();
    if let Some(s) = &changes.summary {
        body.insert("summary".into(), s.as_str().into());
    }
    if let Some(l) = &changes.location {
        body.insert("location".into(), l.as_deref().unwrap_or_default().into());
    }
    if let Some(d) = &changes.description {
        body.insert("description".into(), d.as_deref().unwrap_or_default().into());
    }
    if let Some((start, end, all_day)) = changes.times {
        let (first, after) = if all_day {
            let (first, after) = crate::calendar::days_of(start, end).ok_or_else(|| Error::Other("date".into()))?;
            (Some(first), Some(after))
        } else {
            (None, None)
        };
        match changes.zone().filter(|_| !all_day) {
            Some((name, tz)) => {
                let wall = |t: i64| chrono::DateTime::from_timestamp(t, 0).map(|d| d.with_timezone(&tz).naive_local().format("%Y-%m-%dT%H:%M:%S").to_string());
                body.insert("start".into(), serde_json::json!({ "dateTime": wall(start), "timeZone": name, "date": null }));
                body.insert("end".into(), serde_json::json!({ "dateTime": wall(end.max(start)), "timeZone": name, "date": null }));
            }
            None => {
                body.insert("start".into(), bound(start, first));
                body.insert("end".into(), bound(end.max(start), after));
            }
        }
    }
    if let Some(minutes) = &changes.reminders {
        let overrides: Vec<serde_json::Value> = minutes.iter().map(|m| serde_json::json!({ "method": "popup", "minutes": m })).collect();
        body.insert("reminders".into(), serde_json::json!({ "useDefault": false, "overrides": overrides }));
    }
    // The guests only: Google names the organizer itself. Their answers so far are kept.
    if changes.attendees.is_some() {
        let guests: Vec<serde_json::Value> = changes
            .guests()
            .into_iter()
            .map(|a| {
                let mut g = serde_json::json!({ "email": a.email, "responseStatus": status(a.response.as_deref()) });
                if let Some(n) = &a.name {
                    g["displayName"] = n.as_str().into();
                }
                g
            })
            .collect();
        body.insert("attendees".into(), guests.into());
    }
    Ok(serde_json::Value::Object(body))
}

fn written(json: &serde_json::Value) -> Result<Instance> {
    map_event(json).ok_or_else(|| Error::Other(crate::tr!("Google Agenda : réponse illisible", "Google Calendar: unreadable answer")))
}

/// `key`: the event's id, chosen by facteur (lowercase hex is valid base32hex), so that a creation
/// sent again after an uncertain answer finds the event instead of making a second one.
/// `repeat`: a series (its first occurrence the draft's times).
pub async fn create(token: &str, calendar_id: &str, key: &str, changes: &EventChanges, repeat: Option<&crate::repeat::Repeat>) -> Result<Instance> {
    let mut body = body(changes)?;
    if let (Some(repeat), Some((start, end, all_day))) = (repeat, changes.times) {
        let day = crate::calendar::first_day(start, end, all_day, changes.zone.as_deref());
        series_body(&mut body, (start, end, all_day, day.ok_or_else(|| Error::Other("date".into()))?), Some(repeat), changes.zone.as_deref())?;
    }
    body["id"] = key.into();
    let req = client()?.post(write_url(calendar_id, None)?).header("Content-Type", "application/json").body(serde_json::to_vec(&body)?);
    match send_raw(req, token).await? {
        (reqwest::StatusCode::CONFLICT, _) => written(&get(&client()?, token, events_url(calendar_id, Some(key))?).await?),
        (status, body) => written(&json_of(status, &body)?),
    }
}

pub async fn update(token: &str, calendar_id: &str, event_id: &str, changes: &EventChanges) -> Result<Instance> {
    let mut body = body(changes)?;
    merge_guests(token, calendar_id, event_id, changes, &mut body).await?;
    let req = client()?.patch(write_url(calendar_id, Some(event_id))?).header("Content-Type", "application/json").body(serde_json::to_vec(&body)?);
    written(&send(req, token).await?)
}

/// A PATCH replaces the whole guest list: the list asked for is merged into the one Google holds
/// now, so rooms, optional guests and answers given since the last sync are kept.
async fn merge_guests(token: &str, calendar_id: &str, event_id: &str, changes: &EventChanges, body: &mut serde_json::Value) -> Result<()> {
    if changes.attendees.is_none() {
        return Ok(());
    }
    let current = get(&client()?, token, events_url(calendar_id, Some(event_id))?).await?;
    let wanted: Vec<serde_json::Value> = body["attendees"].as_array().cloned().unwrap_or_default();
    body["attendees"] = merged_guests(current["attendees"].as_array().map(Vec::as_slice).unwrap_or_default(), &wanted).into();
    Ok(())
}

/// `current` entries that stay as they are (the organizer, rooms and resources, guests still
/// wanted), then the wanted guests not there yet.
fn merged_guests(current: &[serde_json::Value], wanted: &[serde_json::Value]) -> Vec<serde_json::Value> {
    let email = |g: &serde_json::Value| g["email"].as_str().unwrap_or_default().to_lowercase();
    let wanted_emails: std::collections::HashSet<String> = wanted.iter().map(email).collect();
    let mut out: Vec<serde_json::Value> = current
        .iter()
        .filter(|g| g["organizer"].as_bool() == Some(true) || g["resource"].as_bool() == Some(true) || wanted_emails.contains(&email(g)))
        .cloned()
        .collect();
    let present: std::collections::HashSet<String> = out.iter().map(email).collect();
    out.extend(wanted.iter().filter(|g| !present.contains(&email(g))).cloned());
    out
}

/// The whole series changed: its texts as `changes` says; with `times` (the new start of an
/// occurrence, and the new length), every date keeps its day and takes that time of day in the
/// series' own zone. Absolute, so sending it twice gives the same series.
pub async fn update_series(token: &str, calendar_id: &str, series_id: &str, changes: &EventChanges, times: Option<(i64, i64)>) -> Result<()> {
    let http = client()?;
    let url = events_url(calendar_id, Some(series_id))?;
    let mut body = body(&EventChanges { times: None, ..changes.clone() })?;
    merge_guests(token, calendar_id, series_id, changes, &mut body).await?;
    if let Some((at, length)) = times {
        let master = get(&http, token, url.clone()).await?;
        let (start, end) = moved(&master["start"], at, length).ok_or_else(|| Error::Other(crate::tr!("Google Agenda : série illisible", "Google Calendar: unreadable series")))?;
        body["start"] = start;
        body["end"] = end;
    }
    send(http.patch(write_url(calendar_id, Some(series_id))?).header("Content-Type", "application/json").body(serde_json::to_vec(&body)?), token).await?;
    Ok(())
}

/// A series' first start on its own day at the time of day of `at`, and its end after `length`.
/// Read in the event's zone (`timeZone`), not the calendar's one the API renders it in.
fn moved(start: &serde_json::Value, at: i64, length: i64) -> Option<(serde_json::Value, serde_json::Value)> {
    if let Some(date) = start["date"].as_str() {
        let first = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
        let days = ((length as f64 / 86400.0).round() as i64).max(1);
        let after = first.checked_add_signed(chrono::Duration::try_days(days)?)?;
        let date = |d: chrono::NaiveDate| serde_json::json!({ "date": d.format("%Y-%m-%d").to_string(), "dateTime": null });
        return Some((date(first), date(after)));
    }
    let master = chrono::DateTime::parse_from_rfc3339(start["dateTime"].as_str()?).ok()?;
    let length = chrono::Duration::try_seconds(length)?;
    if let Some(zone) = start["timeZone"].as_str()
        && let (Some(old), Some(new)) = (crate::calendar::wall_in(zone, master.timestamp()), crate::calendar::wall_in(zone, at))
    {
        let wall = old.date().and_time(new.time());
        let bound = |t: chrono::NaiveDateTime| serde_json::json!({ "dateTime": t.format("%Y-%m-%dT%H:%M:%S").to_string(), "timeZone": zone, "date": null });
        return Some((bound(wall), bound(wall.checked_add_signed(length)?)));
    }
    let offset = *master.offset();
    let time = chrono::DateTime::from_timestamp(at, 0)?.with_timezone(&offset).time();
    let wall = master.naive_local().date().and_time(time);
    let bound = |t: chrono::NaiveDateTime| serde_json::json!({ "dateTime": t.and_local_timezone(offset).single().map(|d| d.to_rfc3339()), "date": null });
    Some((bound(wall), bound(wall.checked_add_signed(length)?)))
}

/// A series' rule (RRULE value, `None` when it does not repeat) and its first day, here.
pub async fn series_rule(token: &str, calendar_id: &str, series_id: &str) -> Result<(Option<String>, chrono::NaiveDate)> {
    let master = get(&client()?, token, events_url(calendar_id, Some(series_id))?).await?;
    let rule = master["recurrence"].as_array().into_iter().flatten().filter_map(|r| r.as_str()).find_map(|r| r.strip_prefix("RRULE:")).map(str::to_string);
    let first = match time(&master["start"]) {
        Some((t, true)) => chrono::DateTime::from_timestamp(t, 0).map(|d| d.date_naive()),
        Some((t, false)) => crate::calendar::local_date(t),
        None => None,
    };
    Ok((rule, first.ok_or_else(|| Error::Other(crate::tr!("Google Agenda : série illisible", "Google Calendar: unreadable series")))?))
}

/// The series rebuilt from its new first occurrence (`first`: start, end, whole day, first day),
/// repeating as `repeat` (`None`: a single event). Timed, in the zone asked for or the computer's,
/// so that its dates keep their hour across changes of time. Google drops the dates changed alone.
pub async fn rebuild_series(
    token: &str,
    calendar_id: &str,
    series_id: &str,
    changes: &EventChanges,
    first: (i64, i64, bool, chrono::NaiveDate),
    repeat: Option<&crate::repeat::Repeat>,
) -> Result<()> {
    let mut body = body(&EventChanges { times: None, ..changes.clone() })?;
    series_body(&mut body, first, repeat, changes.zone.as_deref())?;
    let http = client()?;
    send(http.patch(write_url(calendar_id, Some(series_id))?).header("Content-Type", "application/json").body(serde_json::to_vec(&body)?), token).await?;
    Ok(())
}

/// A series' times and rule into `body` (see [`rebuild_series`]).
fn series_body(body: &mut serde_json::Value, first: (i64, i64, bool, chrono::NaiveDate), repeat: Option<&crate::repeat::Repeat>, zone: Option<&str>) -> Result<()> {
    let (start, end, all_day, day) = first;
    let zone = crate::calendar::series_zone(zone);
    if all_day {
        let (first, after) = crate::calendar::days_of(start, end).ok_or_else(|| Error::Other("date".into()))?;
        body["start"] = bound(start, Some(first));
        body["end"] = bound(end, Some(after));
    } else if let Some((name, tz)) = &zone {
        let wall = |t: i64| chrono::DateTime::from_timestamp(t, 0).map(|d| d.with_timezone(tz).naive_local().format("%Y-%m-%dT%H:%M:%S").to_string());
        body["start"] = serde_json::json!({ "dateTime": wall(start), "timeZone": name, "date": null });
        body["end"] = serde_json::json!({ "dateTime": wall(end), "timeZone": name, "date": null });
    } else {
        body["start"] = bound(start, None);
        body["end"] = bound(end, None);
    }
    let until = |d: chrono::NaiveDate| zone.as_ref().and_then(|(_, tz)| crate::repeat::end_of_day(*tz, d));
    let rule = repeat.map(|r| r.rrule(day, (!all_day).then_some(&until as &dyn Fn(chrono::NaiveDate) -> Option<i64>)));
    body["recurrence"] = serde_json::json!(rule.map(|r| vec![format!("RRULE:{r}")]).unwrap_or_default());
    Ok(())
}

/// The account's answer to an invitation (`accepted`, `tentative`, `declined`), sent to the
/// organizer by Google.
pub async fn respond(token: &str, calendar_id: &str, event_id: &str, answer: &str) -> Result<()> {
    let http = client()?;
    let url = events_url(calendar_id, Some(event_id))?;
    let mut read = url.clone();
    read.query_pairs_mut().append_pair("fields", "attendees");
    let attendees = get(&http, token, read).await?["attendees"].as_array().cloned().unwrap_or_default();
    let me = attendees
        .iter()
        .find(|a| a["self"].as_bool() == Some(true))
        .and_then(|a| a["email"].as_str())
        .ok_or_else(|| Error::Other(crate::tr!("vous n'êtes pas invité à cet événement", "you are not invited to this event")))?;
    let mut write = url;
    write.query_pairs_mut().append_pair("sendUpdates", "all");
    // Only the account's own row: the guest list may have been given in part (hidden by the
    // organizer, or too long), and sending it back would rewrite it.
    let body = serde_json::json!({ "attendees": [{ "email": me, "responseStatus": answer }], "attendeesOmitted": true });
    send(http.patch(write).header("Content-Type", "application/json").body(serde_json::to_vec(&body)?), token).await?;
    Ok(())
}

/// Already gone counts as deleted.
pub async fn delete(token: &str, calendar_id: &str, event_id: &str) -> Result<()> {
    match send(client()?.delete(write_url(calendar_id, Some(event_id))?), token).await {
        Ok(_) | Err(Error::NotFound(_)) => Ok(()),
        Err(e) => Err(e),
    }
}

/// facteur's answer as Google names it.
fn status(answer: Option<&str>) -> &'static str {
    match answer {
        Some("accepted") => "accepted",
        Some("tentative") => "tentative",
        Some("declined") => "declined",
        _ => "needsAction",
    }
}

fn response(status: Option<&str>) -> String {
    match status {
        Some("accepted") => "accepted",
        Some("tentative") => "tentative",
        Some("declined") => "declined",
        _ => "needs_action",
    }
    .to_string()
}

fn map_calendar(item: &serde_json::Value) -> Option<RemoteCalendar> {
    if item["deleted"].as_bool() == Some(true) {
        return None;
    }
    let id = item["id"].as_str()?;
    let name = item["summaryOverride"].as_str().or(item["summary"].as_str()).unwrap_or(id);
    let writable = matches!(item["accessRole"].as_str(), Some("owner" | "writer"));
    Some(RemoteCalendar {
        remote_id: id.to_string(),
        name: name.to_string(),
        color: item["backgroundColor"].as_str().and_then(crate::calendar::hex_color),
        read_only: !writable,
        enabled: item["selected"].as_bool().unwrap_or(false) && item["hidden"].as_bool() != Some(true),
        ctag: None,
    })
}

/// `{ "dateTime": "2026-10-20T14:00:00+02:00" }` or `{ "date": "2026-10-20" }` (whole day:
/// its UTC midnight, turned into the local one when read from the store).
fn time(v: &serde_json::Value) -> Option<(i64, bool)> {
    if let Some(t) = v["dateTime"].as_str() {
        return chrono::DateTime::parse_from_rfc3339(t).ok().map(|d| (d.timestamp(), false));
    }
    let d = chrono::NaiveDate::parse_from_str(v["date"].as_str()?, "%Y-%m-%d").ok()?;
    Some((d.and_hms_opt(0, 0, 0)?.and_utc().timestamp(), true))
}

/// Minutes of a list of reminders (popups and mails alike: both are meant to remind).
fn minutes(list: &serde_json::Value) -> Vec<i64> {
    let mut out: Vec<i64> = list.as_array().into_iter().flatten().filter_map(|r| r["minutes"].as_i64()).filter(|m| *m >= 0).collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// The organizer first, then the guests (rooms left out). None when nobody is invited.
fn attendees(item: &serde_json::Value) -> Vec<crate::model::EventAttendee> {
    let mut out: Vec<crate::model::EventAttendee> = item["attendees"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| a["resource"].as_bool() != Some(true))
        .filter_map(|a| {
            Some(crate::model::EventAttendee {
                email: a["email"].as_str()?.to_string(),
                name: a["displayName"].as_str().map(str::trim).filter(|n| !n.is_empty()).map(str::to_string),
                response: Some(response(a["responseStatus"].as_str())),
                organizer: a["organizer"].as_bool() == Some(true),
            })
        })
        .collect();
    out.sort_by_key(|a| !a.organizer);
    out
}

fn map_event(item: &serde_json::Value) -> Option<Instance> {
    // Working location ("Home", "Office") is not an event to show.
    if item["eventType"].as_str() == Some("workingLocation") {
        return None;
    }
    let (start, all_day) = time(&item["start"])?;
    let end = time(&item["end"]).map_or(start, |(t, _)| t).max(start);
    let text = |k: &str| item[k].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    let organizer = &item["organizer"];
    // Shown as free, declined by the account (its own attendee row), or cancelled: not busy.
    let declined = item["attendees"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|a| a["self"].as_bool() == Some(true) && a["responseStatus"].as_str() == Some("declined"));
    let busy = item["transparency"].as_str() != Some("transparent") && !declined && item["status"].as_str() != Some("cancelled");
    let own = organizer.is_null() || organizer["self"].as_bool() == Some(true);
    let me = item["attendees"].as_array().into_iter().flatten().find(|a| a["self"].as_bool() == Some(true));
    Some(Instance {
        uid: item["iCalUID"].as_str().or(item["id"].as_str())?.to_string(),
        instance: item["id"].as_str()?.to_string(),
        summary: text("summary").unwrap_or_default(),
        location: text("location"),
        description: text("description"),
        start,
        end,
        all_day,
        status: text("status"),
        organizer: organizer["displayName"].as_str().or(organizer["email"].as_str()).map(str::to_string),
        url: text("htmlLink").filter(|u| u.starts_with("https://")),
        busy,
        href: None,
        recurring: item["recurringEventId"].as_str().is_some(),
        own,
        series: item["recurringEventId"].as_str().map(str::to_string),
        response: if own { None } else { me.map(|a| response(a["responseStatus"].as_str())) },
        attendees: attendees(item),
        meeting: {
            let video = item["conferenceData"]["entryPoints"].as_array().into_iter().flatten().find(|p| p["entryPointType"].as_str() == Some("video"));
            let texts = [item["hangoutLink"].as_str(), video.and_then(|p| p["uri"].as_str()), item["location"].as_str(), item["description"].as_str()];
            crate::calendar::meeting_link(texts.into_iter().flatten())
        },
        reminders: minutes(&item["reminders"]["overrides"]),
        zone: if all_day { None } else { item["start"]["timeZone"].as_str().and_then(crate::calendar::event_zone) },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guests_merge_into_the_list_google_holds() {
        let current = vec![
            serde_json::json!({ "email": "me@example.org", "organizer": true, "responseStatus": "accepted" }),
            serde_json::json!({ "email": "Ann@example.org", "optional": true, "responseStatus": "accepted" }),
            serde_json::json!({ "email": "room@resource.calendar.google.com", "resource": true, "responseStatus": "accepted" }),
            serde_json::json!({ "email": "bob@example.org", "responseStatus": "declined" }),
        ];
        let wanted = vec![
            serde_json::json!({ "email": "ann@example.org", "responseStatus": "needsAction" }),
            serde_json::json!({ "email": "carl@example.org", "responseStatus": "needsAction" }),
        ];
        let merged = merged_guests(&current, &wanted);
        let emails: Vec<&str> = merged.iter().map(|g| g["email"].as_str().unwrap()).collect();
        assert_eq!(emails, ["me@example.org", "Ann@example.org", "room@resource.calendar.google.com", "carl@example.org"]);
        assert_eq!(merged[1]["responseStatus"], "accepted", "Ann's answer from Google, not the cache's");
        assert_eq!(merged[1]["optional"], true);
    }

    #[test]
    fn maps_the_calendar_list() {
        let json = serde_json::json!({ "items": [
            { "id": "y@example.org", "summary": "y@example.org", "summaryOverride": "Perso", "backgroundColor": "#9FE1E7",
              "accessRole": "owner", "selected": true, "primary": true },
            { "id": "fr.french#holiday@group.v.calendar.google.com", "summary": "Jours fériés", "backgroundColor": "#16a765",
              "accessRole": "reader" },
            { "id": "gone", "summary": "x", "deleted": true }
        ]});
        let list: Vec<_> = json["items"].as_array().unwrap().iter().filter_map(map_calendar).collect();
        assert_eq!(list.len(), 2);
        assert_eq!((list[0].name.as_str(), list[0].color.as_deref(), list[0].read_only, list[0].enabled), ("Perso", Some("#9fe1e7"), false, true));
        assert_eq!((list[1].remote_id.as_str(), list[1].read_only, list[1].enabled), ("fr.french#holiday@group.v.calendar.google.com", true, false));
    }

    #[test]
    fn maps_events() {
        let timed = serde_json::json!({
            "id": "abc_20261020T120000Z", "iCalUID": "abc@google.com", "status": "confirmed",
            "htmlLink": "https://www.google.com/calendar/event?eid=xyz", "summary": "Point projet", "location": " Salle 3 ",
            "organizer": { "email": "marie@example.org", "displayName": "Marie" },
            "start": { "dateTime": "2026-10-20T14:00:00+02:00", "timeZone": "Europe/Paris" },
            "end": { "dateTime": "2026-10-20T15:00:00+02:00" }
        });
        let e = map_event(&timed).unwrap();
        assert_eq!((e.uid.as_str(), e.instance.as_str()), ("abc@google.com", "abc_20261020T120000Z"));
        assert_eq!((e.start, e.end, e.all_day), (1_792_497_600, 1_792_501_200, false));
        assert_eq!((e.location.as_deref(), e.organizer.as_deref(), e.status.as_deref()), (Some("Salle 3"), Some("Marie"), Some("confirmed")));
        assert_eq!(e.url.as_deref(), Some("https://www.google.com/calendar/event?eid=xyz"));

        let day = serde_json::json!({ "id": "d", "start": { "date": "2026-12-24" }, "end": { "date": "2026-12-26" } });
        let d = map_event(&day).unwrap();
        assert!(d.all_day && d.summary.is_empty() && d.busy);
        assert_eq!(d.uid, "d");
        // Dates, whatever the computer's zone: their UTC midnights.
        assert_eq!((d.start, d.end), (1_798_070_400, 1_798_243_200));

        let free = serde_json::json!({ "id": "f", "transparency": "transparent", "start": { "dateTime": "2026-10-20T14:00:00Z" } });
        assert!(!map_event(&free).unwrap().busy);
        let declined = serde_json::json!({ "id": "n", "start": { "dateTime": "2026-10-20T14:00:00Z" }, "attendees": [
            { "email": "marie@example.org", "responseStatus": "accepted" },
            { "email": "y@example.org", "self": true, "responseStatus": "declined" } ] });
        assert!(!map_event(&declined).unwrap().busy);
        assert!(e.busy);

        assert!(map_event(&serde_json::json!({ "id": "w", "eventType": "workingLocation", "start": { "date": "2026-12-24" } })).is_none());
    }

    #[test]
    fn a_series_moves_in_its_own_zone() {
        // Rendered in the calendar's zone (New York), the event's own zone is Paris.
        let start = serde_json::json!({ "dateTime": "2026-03-02T08:00:00-05:00", "timeZone": "Europe/Paris" });
        // An occurrence after the change of time moved to 15:00 Paris (13:00 UTC).
        let at = chrono::DateTime::parse_from_rfc3339("2026-10-20T13:00:00Z").unwrap().timestamp();
        let (from, to) = moved(&start, at, 7200).unwrap();
        assert_eq!(from, serde_json::json!({ "dateTime": "2026-03-02T15:00:00", "timeZone": "Europe/Paris", "date": null }));
        assert_eq!(to["dateTime"], "2026-03-02T17:00:00");
        assert_eq!(moved(&start, at, 7200), Some((from, to)), "the same change twice gives the same series");
        let day = serde_json::json!({ "date": "2026-12-24" });
        let (from, to) = moved(&day, 0, 2 * 86400).unwrap();
        assert_eq!((from["date"].as_str(), to["date"].as_str()), (Some("2026-12-24"), Some("2026-12-26")));
    }

    #[test]
    fn reads_the_answer_to_an_invitation() {
        let item = serde_json::json!({ "id": "i_20261020", "recurringEventId": "i", "start": { "dateTime": "2026-10-20T14:00:00Z" },
            "organizer": { "email": "marie@example.org" },
            "attendees": [{ "email": "marie@example.org", "organizer": true }, { "email": "y@example.org", "self": true, "responseStatus": "tentative" }] });
        let e = map_event(&item).unwrap();
        assert_eq!((e.own, e.response.as_deref(), e.series.as_deref(), e.recurring), (false, Some("tentative"), Some("i"), true));
        let mine = serde_json::json!({ "id": "m", "start": { "dateTime": "2026-10-20T14:00:00Z" }, "organizer": { "email": "y@example.org", "self": true } });
        let e = map_event(&mine).unwrap();
        assert_eq!((e.own, e.response), (true, None));
    }

    #[test]
    fn reads_guests_meeting_and_reminders() {
        let item = serde_json::json!({ "id": "g", "start": { "dateTime": "2026-10-20T14:00:00Z" },
            "organizer": { "email": "y@example.org", "self": true },
            "attendees": [
                { "email": "paul@example.org", "displayName": "Paul", "responseStatus": "declined" },
                { "email": "y@example.org", "organizer": true, "self": true, "responseStatus": "accepted" },
                { "email": "salle@resource.calendar.google.com", "resource": true }],
            "conferenceData": { "entryPoints": [{ "entryPointType": "phone", "uri": "tel:+33" }, { "entryPointType": "video", "uri": "https://meet.google.com/abc-defg-hij" }] },
            "reminders": { "useDefault": false, "overrides": [{ "method": "popup", "minutes": 30 }, { "method": "email", "minutes": 10 }, { "method": "popup", "minutes": 10 }] } });
        let e = map_event(&item).unwrap();
        let who: Vec<(&str, bool, Option<&str>)> = e.attendees.iter().map(|a| (a.email.as_str(), a.organizer, a.response.as_deref())).collect();
        assert_eq!(who, [("y@example.org", true, Some("accepted")), ("paul@example.org", false, Some("declined"))]);
        assert_eq!(e.meeting.as_deref(), Some("https://meet.google.com/abc-defg-hij"));
        assert_eq!(e.reminders, [10, 30]);
        assert_eq!(minutes(&serde_json::json!([{ "method": "popup", "minutes": 15 }])), [15]);

        let changes = EventChanges { attendees: Some(e.attendees.clone()), ..Default::default() };
        let written = body(&changes).unwrap();
        assert_eq!(written["attendees"], serde_json::json!([{ "email": "paul@example.org", "displayName": "Paul", "responseStatus": "declined" }]), "the guests, their answers kept");
        assert_eq!(write_url("c", Some("e")).unwrap().query(), Some("sendUpdates=all"));

        let zoned = EventChanges { times: Some((1_792_497_600, 1_792_501_200, false)), zone: Some("Asia/Tokyo".into()), reminders: Some(vec![10]), ..Default::default() };
        let written = body(&zoned).unwrap();
        assert_eq!(written["start"], serde_json::json!({ "dateTime": "2026-10-20T21:00:00", "timeZone": "Asia/Tokyo", "date": null }));
        assert_eq!(written["reminders"], serde_json::json!({ "useDefault": false, "overrides": [{ "method": "popup", "minutes": 10 }] }));
        let item = serde_json::json!({ "id": "z", "start": { "dateTime": "2026-10-20T21:00:00+09:00", "timeZone": "Asia/Tokyo" } });
        assert_eq!(map_event(&item).unwrap().zone.as_deref(), Some("Asia/Tokyo"));
    }

    #[test]
    fn missing_scope_asks_to_reconnect() {
        let body = br#"{"error":{"code":403,"message":"Request had insufficient authentication scopes.","status":"PERMISSION_DENIED",
            "details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"ACCESS_TOKEN_SCOPE_INSUFFICIENT"}]}}"#;
        assert!(matches!(api_error(reqwest::StatusCode::FORBIDDEN, body), Error::Auth(_)));
        let body = br#"{"error":{"code":403,"message":"Google Calendar API has not been used","errors":[{"reason":"accessNotConfigured"}]}}"#;
        assert!(matches!(api_error(reqwest::StatusCode::FORBIDDEN, body), Error::Other(_)));
    }
}
