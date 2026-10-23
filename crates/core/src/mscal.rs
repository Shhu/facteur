//! Outlook calendars through Microsoft Graph (Outlook.com has no CalDAV). `calendarView` expands
//! recurring events server side; an occurrence is changed or deleted by its own id.

use std::time::Duration;

use crate::calendar::{EventChanges, Instance, RemoteCalendar};
use crate::error::{Error, Result};

const API: &str = "https://graph.microsoft.com/v1.0";
const MAX_PAGES: usize = 40;
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

pub fn needs_consent() -> Error {
    Error::Auth(crate::tr!(
        "facteur n'a pas accès aux agendas de ce compte : reconnectez-le",
        "facteur has no access to this account's calendars: reconnect it"
    ))
}

/// Signed in before facteur asked to write to the calendars.
pub fn needs_write_consent() -> Error {
    Error::Other(crate::tr!(
        "facteur ne peut pas modifier les agendas de ce compte : reconnectez-le pour l'autoriser",
        "facteur may not change this account's calendars: reconnect it to allow it"
    ))
}

/// A 403 is a missing permission only when Graph says the token falls short; otherwise the account
/// may not change that event (a shared calendar, an organizer-only change) and reconnecting would
/// not help.
fn api_error(status: reqwest::StatusCode, body: &[u8]) -> Error {
    let json: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
    let code = json["error"]["code"].as_str().unwrap_or_default();
    let text = json["error"]["message"].as_str().unwrap_or_default().to_ascii_lowercase();
    let scope = code == "InvalidAuthenticationToken" || text.contains("check credentials") || text.contains("scope");
    if status == reqwest::StatusCode::UNAUTHORIZED || (status == reqwest::StatusCode::FORBIDDEN && scope) {
        return needs_consent();
    }
    if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::GONE {
        return Error::NotFound(crate::tr!("événement Outlook", "Outlook event"));
    }
    let message = json["error"]["message"].as_str().map(str::to_string).unwrap_or_else(|| status.to_string());
    Error::Other(crate::tr!("Agenda Outlook : {message}", "Outlook calendar: {message}"))
}

async fn list(http: &reqwest::Client, token: &str, url: reqwest::Url) -> Result<Vec<serde_json::Value>> {
    let mut items = Vec::new();
    let mut next = Some(url);
    for _ in 0..MAX_PAGES {
        let Some(url) = next.take() else { break };
        // Only Graph's own pages are followed: the token goes nowhere else.
        if url.scheme() != "https" || url.host_str() != Some("graph.microsoft.com") {
            break;
        }
        let resp = http
            .get(url)
            .bearer_auth(token)
            .header("Prefer", "outlook.timezone=\"UTC\"")
            .send()
            .await
            .map_err(|e| Error::Other(crate::tr!("Microsoft Graph injoignable : {e}", "Microsoft Graph unreachable: {e}")))?;
        let status = resp.status();
        let body = crate::http::read_capped(resp, MAX_PAGE).await?;
        if !status.is_success() {
            return Err(api_error(status, &body));
        }
        let json: serde_json::Value = serde_json::from_slice(&body)?;
        items.extend(json["value"].as_array().cloned().unwrap_or_default());
        next = json["@odata.nextLink"].as_str().and_then(|u| reqwest::Url::parse(u).ok());
    }
    Ok(items)
}

const WRITE_PAGE: usize = 1024 * 1024;

async fn write(req: reqwest::RequestBuilder, token: &str) -> Result<serde_json::Value> {
    let resp = req
        .bearer_auth(token)
        .header("Prefer", "outlook.timezone=\"UTC\"")
        .send()
        .await
        .map_err(|e| Error::Other(crate::tr!("Microsoft Graph injoignable : {e}", "Microsoft Graph unreachable: {e}")))?;
    let status = resp.status();
    let body = crate::http::read_capped(resp, WRITE_PAGE).await?;
    if !status.is_success() {
        return Err(api_error(status, &body));
    }
    if body.is_empty() {
        return Ok(serde_json::Value::Null);
    }
    Ok(serde_json::from_slice(&body)?)
}

fn url(segments: &[&str]) -> Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(API).map_err(|e| Error::Other(e.to_string()))?;
    url.path_segments_mut().map_err(|_| Error::Other("URL".into()))?.extend(segments);
    Ok(url)
}

/// Times in the zone asked for, else in UTC; a whole day as its dates at midnight.
fn body(changes: &EventChanges) -> Result<serde_json::Value> {
    let mut body = serde_json::Map::new();
    if let Some(s) = &changes.summary {
        body.insert("subject".into(), s.as_str().into());
    }
    if let Some(l) = &changes.location {
        body.insert("location".into(), serde_json::json!({ "displayName": l.as_deref().unwrap_or_default() }));
    }
    if let Some(d) = &changes.description {
        body.insert("body".into(), serde_json::json!({ "contentType": "text", "content": d.as_deref().unwrap_or_default() }));
    }
    if let Some((start, end, all_day)) = changes.times {
        let (from, to) = if all_day {
            let (first, after) = crate::calendar::days_of(start, end).ok_or_else(|| Error::Other("date".into()))?;
            (first.format("%Y-%m-%dT00:00:00").to_string(), after.format("%Y-%m-%dT00:00:00").to_string())
        } else {
            let tz = changes.zone().map(|(_, tz)| tz);
            let at = |t: i64| {
                chrono::DateTime::from_timestamp(t, 0)
                    .map(|d| match tz {
                        Some(tz) => d.with_timezone(&tz).naive_local(),
                        None => d.naive_utc(),
                    })
                    .map(|d| d.format("%Y-%m-%dT%H:%M:%S").to_string())
                    .unwrap_or_default()
            };
            (at(start), at(end.max(start)))
        };
        let zone = changes.zone().filter(|_| !all_day).map_or_else(|| "UTC".to_string(), |(name, _)| name);
        body.insert("isAllDay".into(), all_day.into());
        body.insert("start".into(), serde_json::json!({ "dateTime": from, "timeZone": zone }));
        body.insert("end".into(), serde_json::json!({ "dateTime": to, "timeZone": zone }));
    }
    // Outlook has one reminder per event: the earliest asked for.
    if let Some(minutes) = &changes.reminders {
        body.insert("isReminderOn".into(), (!minutes.is_empty()).into());
        if let Some(m) = minutes.iter().max() {
            body.insert("reminderMinutesBeforeStart".into(), (*m).into());
        }
    }
    // The guests: Outlook names the organizer and sends the invitations itself.
    if changes.attendees.is_some() {
        let guests: Vec<serde_json::Value> = changes
            .guests()
            .into_iter()
            .map(|a| serde_json::json!({ "emailAddress": { "address": a.email, "name": a.name.as_deref().unwrap_or(&a.email) }, "type": "required" }))
            .collect();
        body.insert("attendees".into(), guests.into());
    }
    Ok(serde_json::Value::Object(body))
}

fn written(json: &serde_json::Value) -> Result<Instance> {
    map_event(json).ok_or_else(|| Error::Other(crate::tr!("Agenda Outlook : réponse illisible", "Outlook calendar: unreadable answer")))
}

/// `key`: Graph's `transactionId`, so that a creation sent again after an uncertain answer is not
/// made twice.
/// `repeat`: a series (its first occurrence the draft's times).
pub async fn create(token: &str, calendar_id: &str, key: &str, changes: &EventChanges, repeat: Option<&crate::repeat::Repeat>) -> Result<Instance> {
    let mut body = body(changes)?;
    if let (Some(repeat), Some((start, end, all_day))) = (repeat, changes.times) {
        let day = crate::calendar::first_day(start, end, all_day, changes.zone.as_deref());
        series_body(&mut body, (start, end, all_day, day.ok_or_else(|| Error::Other("date".into()))?), Some(repeat), changes.zone.as_deref())?;
    }
    body["transactionId"] = key.into();
    let req = client()?.post(url(&["me", "calendars", calendar_id, "events"])?).header("Content-Type", "application/json").body(serde_json::to_vec(&body)?);
    written(&write(req, token).await?)
}

pub async fn update(token: &str, event_id: &str, changes: &EventChanges) -> Result<Instance> {
    let mut body = body(changes)?;
    merge_guests(token, event_id, changes, &mut body).await?;
    let req = client()?.patch(url(&["me", "events", event_id])?).header("Content-Type", "application/json").body(serde_json::to_vec(&body)?);
    written(&write(req, token).await?)
}

/// A PATCH replaces the whole guest list: the list asked for is merged into the one Outlook holds
/// now, so rooms (resources) and optional guests stay as they are.
async fn merge_guests(token: &str, event_id: &str, changes: &EventChanges, body: &mut serde_json::Value) -> Result<()> {
    if changes.attendees.is_none() {
        return Ok(());
    }
    let mut read = url(&["me", "events", event_id])?;
    read.query_pairs_mut().append_pair("$select", "attendees");
    let current = write(client()?.get(read), token).await?;
    let wanted: Vec<serde_json::Value> = body["attendees"].as_array().cloned().unwrap_or_default();
    body["attendees"] = merged_guests(current["attendees"].as_array().map(Vec::as_slice).unwrap_or_default(), &wanted).into();
    Ok(())
}

/// `current` entries that stay (rooms and resources, guests still wanted), their answer left out
/// (read-only in Graph), then the wanted guests not there yet.
fn merged_guests(current: &[serde_json::Value], wanted: &[serde_json::Value]) -> Vec<serde_json::Value> {
    let email = |g: &serde_json::Value| g["emailAddress"]["address"].as_str().unwrap_or_default().to_lowercase();
    let wanted_emails: std::collections::HashSet<String> = wanted.iter().map(email).collect();
    let mut out: Vec<serde_json::Value> = current
        .iter()
        .filter(|g| g["type"].as_str() == Some("resource") || wanted_emails.contains(&email(g)))
        .map(|g| serde_json::json!({ "emailAddress": g["emailAddress"], "type": g["type"] }))
        .collect();
    let present: std::collections::HashSet<String> = out.iter().map(email).collect();
    out.extend(wanted.iter().filter(|g| !present.contains(&email(g))).cloned());
    out
}

/// The whole series changed, as for Google (`gcal::update_series`): every date keeps its day and
/// takes the time of day of `times`' start in the series' own zone. Absolute, so sending it twice
/// gives the same series.
pub async fn update_series(token: &str, series_id: &str, changes: &EventChanges, times: Option<(i64, i64)>) -> Result<()> {
    let http = client()?;
    let mut body = body(&EventChanges { times: None, ..changes.clone() })?;
    merge_guests(token, series_id, changes, &mut body).await?;
    if let Some((at, length)) = times {
        let mut read = url(&["me", "events", series_id])?;
        read.query_pairs_mut().append_pair("$select", "start,isAllDay,originalStartTimeZone");
        let master = write(http.get(read), token).await?;
        let unreadable = || Error::Other(crate::tr!("Agenda Outlook : série illisible", "Outlook calendar: unreadable series"));
        let all_day = master["isAllDay"].as_bool() == Some(true);
        let first = time(&master["start"], all_day).ok_or_else(unreadable)?;
        let zone = master["originalStartTimeZone"]
            .as_str()
            .filter(|z| !z.is_empty() && *z != "tzone://Microsoft/Custom" && crate::calendar::wall_in(z, first).is_some());
        let utc = |t: i64| chrono::DateTime::from_timestamp(t, 0).map(|d| d.naive_utc());
        let (start, end, zone) = if all_day {
            let day = utc(first).ok_or_else(unreadable)?.date();
            let days = ((length as f64 / 86400.0).round() as i64).max(1);
            let end = day.checked_add_signed(chrono::Duration::try_days(days).ok_or_else(unreadable)?).ok_or_else(unreadable)?;
            (day.and_time(chrono::NaiveTime::MIN), end.and_time(chrono::NaiveTime::MIN), "UTC")
        } else {
            let wall = |t: i64| zone.and_then(|z| crate::calendar::wall_in(z, t)).or_else(|| utc(t));
            let start = wall(first).ok_or_else(unreadable)?.date().and_time(wall(at).ok_or_else(unreadable)?.time());
            let end = start.checked_add_signed(chrono::Duration::try_seconds(length).ok_or_else(unreadable)?).ok_or_else(unreadable)?;
            (start, end, zone.unwrap_or("UTC"))
        };
        let bound = |t: chrono::NaiveDateTime| serde_json::json!({ "dateTime": t.format("%Y-%m-%dT%H:%M:%S").to_string(), "timeZone": zone });
        body["start"] = bound(start);
        body["end"] = bound(end);
    }
    write(http.patch(url(&["me", "events", series_id])?).header("Content-Type", "application/json").body(serde_json::to_vec(&body)?), token).await?;
    Ok(())
}

/// A series' recurrence (`Null` when it does not repeat) and its first day, here.
pub async fn series_rule(token: &str, series_id: &str) -> Result<(serde_json::Value, chrono::NaiveDate)> {
    let mut read = url(&["me", "events", series_id])?;
    read.query_pairs_mut().append_pair("$select", "start,isAllDay,recurrence,originalStartTimeZone");
    let master = write(client()?.get(read), token).await?;
    let all_day = master["isAllDay"].as_bool() == Some(true);
    let zone = master["originalStartTimeZone"].as_str().and_then(crate::calendar::event_zone);
    let first = time(&master["start"], all_day).and_then(|t| {
        if all_day { chrono::DateTime::from_timestamp(t, 0).map(|d| d.date_naive()) } else { crate::calendar::day_in(t, zone.as_deref()) }
    });
    let first = first.ok_or_else(|| Error::Other(crate::tr!("Agenda Outlook : série illisible", "Outlook calendar: unreadable series")))?;
    Ok((master["recurrence"].clone(), first))
}

/// As `gcal::rebuild_series`, through Graph.
pub async fn rebuild_series(
    token: &str,
    series_id: &str,
    changes: &EventChanges,
    first: (i64, i64, bool, chrono::NaiveDate),
    repeat: Option<&crate::repeat::Repeat>,
) -> Result<()> {
    let mut body = body(&EventChanges { times: None, ..changes.clone() })?;
    // As in `update_series`: rooms kept, optional guests left optional.
    merge_guests(token, series_id, changes, &mut body).await?;
    series_body(&mut body, first, repeat, changes.zone.as_deref())?;
    write(client()?.patch(url(&["me", "events", series_id])?).header("Content-Type", "application/json").body(serde_json::to_vec(&body)?), token).await?;
    Ok(())
}

/// A series' times and rule into `body` (see [`rebuild_series`]).
fn series_body(body: &mut serde_json::Value, first: (i64, i64, bool, chrono::NaiveDate), repeat: Option<&crate::repeat::Repeat>, zone: Option<&str>) -> Result<()> {
    let (start, end, all_day, day) = first;
    let (zone, tz) = match crate::calendar::series_zone(zone) {
        Some((name, tz)) => (name, Some(tz)),
        None => ("UTC".to_string(), None),
    };
    let wall = |t: i64| {
        chrono::DateTime::from_timestamp(t, 0).map(|d| match tz {
            Some(tz) => d.with_timezone(&tz).naive_local(),
            None => d.naive_utc(),
        })
    };
    let (from, to) = if all_day {
        let (first, after) = crate::calendar::days_of(start, end).ok_or_else(|| Error::Other("date".into()))?;
        (first.and_time(chrono::NaiveTime::MIN), after.and_time(chrono::NaiveTime::MIN))
    } else {
        (wall(start).ok_or_else(|| Error::Other("date".into()))?, wall(end).ok_or_else(|| Error::Other("date".into()))?)
    };
    let bound = |t: chrono::NaiveDateTime| serde_json::json!({ "dateTime": t.format("%Y-%m-%dT%H:%M:%S").to_string(), "timeZone": zone });
    body["isAllDay"] = all_day.into();
    body["start"] = bound(from);
    body["end"] = bound(to);
    body["recurrence"] = repeat.map_or(serde_json::Value::Null, |r| r.graph(day, &zone));
    Ok(())
}

/// The account's answer to an invitation (`accepted`, `tentative`, `declined`), sent to the
/// organizer.
pub async fn respond(token: &str, event_id: &str, answer: &str) -> Result<()> {
    let action = match answer {
        "accepted" => "accept",
        "tentative" => "tentativelyAccept",
        _ => "decline",
    };
    let body = serde_json::json!({ "sendResponse": true });
    write(client()?.post(url(&["me", "events", event_id, action])?).header("Content-Type", "application/json").body(serde_json::to_vec(&body)?), token).await?;
    Ok(())
}

/// Already gone counts as deleted.
/// `guests`: the event has some, told of the cancellation (Graph's `cancel` deletes it too).
pub async fn delete(token: &str, event_id: &str, guests: bool) -> Result<()> {
    let req = if guests {
        client()?.post(url(&["me", "events", event_id, "cancel"])?).header("Content-Type", "application/json").body(r#"{"comment":""}"#)
    } else {
        client()?.delete(url(&["me", "events", event_id])?)
    };
    match write(req, token).await {
        Ok(_) | Err(Error::NotFound(_)) => Ok(()),
        Err(e) => Err(e),
    }
}

pub async fn calendars(token: &str) -> Result<Vec<RemoteCalendar>> {
    let http = client()?;
    let mut url = reqwest::Url::parse(&format!("{API}/me/calendars")).map_err(|e| Error::Other(e.to_string()))?;
    url.query_pairs_mut().append_pair("$top", "100").append_pair("$select", "id,name,hexColor,canEdit,isDefaultCalendar");
    Ok(list(&http, token, url).await?.iter().filter_map(map_calendar).collect())
}

pub async fn events(token: &str, calendar_id: &str, from: i64, to: i64) -> Result<Vec<Instance>> {
    let http = client()?;
    let iso = |t: i64| chrono::DateTime::from_timestamp(t, 0).map(|d| d.format("%Y-%m-%dT%H:%M:%SZ").to_string()).unwrap_or_default();
    let mut url = reqwest::Url::parse(API).map_err(|e| Error::Other(e.to_string()))?;
    url.path_segments_mut()
        .map_err(|_| Error::Other("URL".into()))?
        .extend(["me", "calendars", calendar_id, "calendarView"]);
    url.query_pairs_mut()
        .append_pair("startDateTime", &iso(from))
        .append_pair("endDateTime", &iso(to))
        .append_pair("$top", "500")
        .append_pair(
            "$select",
            "id,iCalUId,subject,location,bodyPreview,start,end,isAllDay,showAs,isCancelled,organizer,webLink,responseStatus,type,isOrganizer,seriesMasterId,attendees,onlineMeeting,isReminderOn,reminderMinutesBeforeStart,originalStartTimeZone",
        );
    Ok(list(&http, token, url).await?.iter().filter_map(map_event).collect())
}

/// What the calendar holds in the window, in a few bytes: each occurrence's id and last change.
/// The same fingerprint as at the last fetch means nothing was added, changed or deleted, for a
/// fraction of a full read (no subjects nor previews).
pub async fn fingerprint(token: &str, calendar_id: &str, from: i64, to: i64) -> Result<String> {
    use sha2::{Digest, Sha256};
    let http = client()?;
    let iso = |t: i64| chrono::DateTime::from_timestamp(t, 0).map(|d| d.format("%Y-%m-%dT%H:%M:%SZ").to_string()).unwrap_or_default();
    let mut url = reqwest::Url::parse(API).map_err(|e| Error::Other(e.to_string()))?;
    url.path_segments_mut().map_err(|_| Error::Other("URL".into()))?.extend(["me", "calendars", calendar_id, "calendarView"]);
    url.query_pairs_mut()
        .append_pair("startDateTime", &iso(from))
        .append_pair("endDateTime", &iso(to))
        .append_pair("$top", "1000")
        .append_pair("$select", "id,lastModifiedDateTime");
    let mut lines: Vec<String> = list(&http, token, url)
        .await?
        .iter()
        .map(|e| format!("{} {}", e["id"].as_str().unwrap_or_default(), e["lastModifiedDateTime"].as_str().unwrap_or_default()))
        .collect();
    lines.sort_unstable();
    let digest = Sha256::digest(lines.join("\n").as_bytes());
    Ok(digest.iter().take(16).map(|b| format!("{b:02x}")).collect())
}

fn map_calendar(item: &serde_json::Value) -> Option<RemoteCalendar> {
    let id = item["id"].as_str()?;
    Some(RemoteCalendar {
        remote_id: id.to_string(),
        name: item["name"].as_str().unwrap_or(id).to_string(),
        color: item["hexColor"].as_str().filter(|c| !c.is_empty()).and_then(crate::calendar::hex_color),
        read_only: item["canEdit"].as_bool() != Some(true),
        // Shown at first: the account's own calendars (not the holidays or birthdays added).
        enabled: item["isDefaultCalendar"].as_bool() == Some(true) || item["canEdit"].as_bool() == Some(true),
        ctag: None,
    })
}

/// Times are asked for in UTC. A whole day's dates are taken as written, at UTC midnight.
fn time(v: &serde_json::Value, all_day: bool) -> Option<i64> {
    let text = v["dateTime"].as_str()?;
    let at = chrono::NaiveDateTime::parse_from_str(text.get(..19)?, "%Y-%m-%dT%H:%M:%S").ok()?;
    if all_day {
        return Some(at.date().and_hms_opt(0, 0, 0)?.and_utc().timestamp());
    }
    Some(at.and_utc().timestamp())
}

fn map_event(item: &serde_json::Value) -> Option<Instance> {
    let all_day = item["isAllDay"].as_bool() == Some(true);
    let start = time(&item["start"], all_day)?;
    let end = time(&item["end"], all_day).unwrap_or(start).max(start);
    let text = |v: &serde_json::Value| v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    let cancelled = item["isCancelled"].as_bool() == Some(true);
    let declined = item["responseStatus"]["response"].as_str() == Some("declined");
    let show_as = item["showAs"].as_str().unwrap_or("busy");
    let organizer = &item["organizer"]["emailAddress"];
    let own = item["isOrganizer"].as_bool() != Some(false);
    Some(Instance {
        uid: item["iCalUId"].as_str().or(item["id"].as_str())?.to_string(),
        instance: item["id"].as_str()?.to_string(),
        summary: text(&item["subject"]).unwrap_or_default(),
        location: text(&item["location"]["displayName"]),
        description: text(&item["bodyPreview"]),
        start,
        end,
        all_day,
        status: Some(if cancelled { "cancelled" } else if show_as == "tentative" { "tentative" } else { "confirmed" }.to_string()),
        organizer: text(&organizer["name"]).or_else(|| text(&organizer["address"])),
        url: text(&item["webLink"]).filter(|u| u.starts_with("https://")),
        busy: show_as != "free" && show_as != "workingElsewhere" && !declined && !cancelled,
        href: None,
        recurring: matches!(item["type"].as_str(), Some("occurrence" | "exception")),
        own,
        series: item["seriesMasterId"].as_str().filter(|s| !s.is_empty()).map(str::to_string),
        response: (!own).then(|| answer_of(item["responseStatus"]["response"].as_str())),
        attendees: attendees(item),
        meeting: {
            let texts = [item["onlineMeeting"]["joinUrl"].as_str(), item["location"]["displayName"].as_str(), item["bodyPreview"].as_str()];
            crate::calendar::meeting_link(texts.into_iter().flatten())
        },
        reminders: match (item["isReminderOn"].as_bool(), item["reminderMinutesBeforeStart"].as_i64()) {
            (Some(true), Some(m)) if m >= 0 => vec![m],
            _ => Vec::new(),
        },
        zone: if all_day { None } else { item["originalStartTimeZone"].as_str().and_then(crate::calendar::event_zone) },
    })
}

fn answer_of(response: Option<&str>) -> String {
    match response {
        Some("accepted" | "organizer") => "accepted",
        Some("tentativelyAccepted") => "tentative",
        Some("declined") => "declined",
        _ => "needs_action",
    }
    .to_string()
}

/// The organizer first, then the guests (rooms and equipment left out). None when nobody is
/// invited.
fn attendees(item: &serde_json::Value) -> Vec<crate::model::EventAttendee> {
    let person = |a: &serde_json::Value| {
        let email = a["emailAddress"]["address"].as_str().map(str::trim).filter(|e| !e.is_empty())?;
        Some(crate::model::EventAttendee {
            email: email.to_string(),
            name: a["emailAddress"]["name"].as_str().map(str::trim).filter(|n| !n.is_empty() && *n != email).map(str::to_string),
            response: Some(answer_of(a["status"]["response"].as_str())),
            organizer: false,
        })
    };
    let guests: Vec<crate::model::EventAttendee> =
        item["attendees"].as_array().into_iter().flatten().filter(|a| a["type"].as_str() != Some("resource")).filter_map(person).collect();
    if guests.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<crate::model::EventAttendee> = person(&item["organizer"])
        .map(|o| crate::model::EventAttendee { organizer: true, response: Some("accepted".into()), ..o })
        .into_iter()
        .collect();
    let organizer = out.first().map(|o| o.email.clone()).unwrap_or_default();
    out.extend(guests.into_iter().filter(|g| !g.email.eq_ignore_ascii_case(&organizer)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guests_merge_into_the_list_outlook_holds() {
        let person = |a: &str, t: &str| serde_json::json!({ "emailAddress": { "address": a, "name": a }, "type": t, "status": { "response": "accepted" } });
        let current = vec![person("ann@example.org", "optional"), person("room@example.org", "resource"), person("bob@example.org", "required")];
        let wanted = vec![
            serde_json::json!({ "emailAddress": { "address": "ann@example.org", "name": "Ann" }, "type": "required" }),
            serde_json::json!({ "emailAddress": { "address": "carl@example.org", "name": "Carl" }, "type": "required" }),
        ];
        let merged = merged_guests(&current, &wanted);
        let emails: Vec<&str> = merged.iter().map(|g| g["emailAddress"]["address"].as_str().unwrap()).collect();
        assert_eq!(emails, ["ann@example.org", "room@example.org", "carl@example.org"]);
        assert_eq!(merged[0]["type"], "optional", "kept as Outlook has it");
        assert!(merged[0].get("status").is_none(), "read-only in Graph");
    }

    #[test]
    fn maps_the_calendar_list() {
        let json = serde_json::json!({ "value": [
            { "id": "AAA", "name": "Calendrier", "hexColor": "#FF8C00", "canEdit": true, "isDefaultCalendar": true },
            { "id": "BBB", "name": "Jours fériés", "hexColor": "", "canEdit": false, "isDefaultCalendar": false }
        ]});
        let list: Vec<_> = json["value"].as_array().unwrap().iter().filter_map(map_calendar).collect();
        assert_eq!((list[0].name.as_str(), list[0].color.as_deref(), list[0].read_only, list[0].enabled), ("Calendrier", Some("#ff8c00"), false, true));
        assert_eq!((list[1].color.as_deref(), list[1].read_only, list[1].enabled), (None, true, false));
    }

    #[test]
    fn maps_events() {
        let timed = serde_json::json!({
            "id": "EV1", "iCalUId": "040000008200E0", "subject": "Point projet", "bodyPreview": "Ordre du jour",
            "location": { "displayName": " Salle 3 " }, "isAllDay": false, "isCancelled": false, "showAs": "busy",
            "start": { "dateTime": "2026-10-20T12:00:00.0000000", "timeZone": "UTC" },
            "end": { "dateTime": "2026-10-20T13:00:00.0000000", "timeZone": "UTC" },
            "organizer": { "emailAddress": { "name": "Marie", "address": "marie@example.org" } },
            "webLink": "https://outlook.live.com/calendar/item/EV1", "responseStatus": { "response": "accepted" }
        });
        let e = map_event(&timed).unwrap();
        assert_eq!((e.uid.as_str(), e.instance.as_str()), ("040000008200E0", "EV1"));
        assert_eq!((e.start, e.end, e.all_day, e.busy), (1_792_497_600, 1_792_501_200, false, true));
        assert_eq!((e.location.as_deref(), e.organizer.as_deref(), e.status.as_deref()), (Some("Salle 3"), Some("Marie"), Some("confirmed")));

        let day = serde_json::json!({ "id": "D", "isAllDay": true, "showAs": "free",
            "start": { "dateTime": "2026-12-24T00:00:00.0000000" }, "end": { "dateTime": "2026-12-26T00:00:00.0000000" } });
        let d = map_event(&day).unwrap();
        assert!(d.all_day && !d.busy);
        assert_eq!((d.start, d.end), (1_798_070_400, 1_798_243_200));

        let declined = serde_json::json!({ "id": "N", "start": { "dateTime": "2026-10-20T12:00:00" }, "responseStatus": { "response": "declined" } });
        assert!(!map_event(&declined).unwrap().busy);
    }

    #[test]
    fn reads_guests_meeting_and_reminders() {
        let item = serde_json::json!({ "id": "M", "start": { "dateTime": "2026-10-20T12:00:00" }, "isOrganizer": true,
            "organizer": { "emailAddress": { "name": "Y", "address": "y@example.org" } },
            "attendees": [
                { "emailAddress": { "name": "paul@example.org", "address": "paul@example.org" }, "type": "required", "status": { "response": "tentativelyAccepted" } },
                { "emailAddress": { "name": "Salle", "address": "salle@example.org" }, "type": "resource", "status": { "response": "accepted" } }],
            "onlineMeeting": { "joinUrl": "https://teams.microsoft.com/l/meetup-join/19%3ameeting" },
            "isReminderOn": true, "reminderMinutesBeforeStart": 15 });
        let e = map_event(&item).unwrap();
        let who: Vec<(&str, Option<&str>, bool, Option<&str>)> =
            e.attendees.iter().map(|a| (a.email.as_str(), a.name.as_deref(), a.organizer, a.response.as_deref())).collect();
        assert_eq!(who, [("y@example.org", Some("Y"), true, Some("accepted")), ("paul@example.org", None, false, Some("tentative"))]);
        assert_eq!(e.meeting.as_deref(), Some("https://teams.microsoft.com/l/meetup-join/19%3ameeting"));
        assert_eq!(e.reminders, [15]);
        let off = serde_json::json!({ "id": "O", "start": { "dateTime": "2026-10-20T12:00:00" }, "isReminderOn": false, "reminderMinutesBeforeStart": 15 });
        let o = map_event(&off).unwrap();
        assert!(o.reminders.is_empty() && o.attendees.is_empty());

        let changes = EventChanges { attendees: Some(e.attendees.clone()), ..Default::default() };
        assert_eq!(body(&changes).unwrap()["attendees"], serde_json::json!([{ "emailAddress": { "address": "paul@example.org", "name": "paul@example.org" }, "type": "required" }]));

        let zoned = EventChanges { times: Some((1_792_497_600, 1_792_501_200, false)), zone: Some("Asia/Tokyo".into()), reminders: Some(vec![10, 60]), ..Default::default() };
        let written = body(&zoned).unwrap();
        assert_eq!(written["start"], serde_json::json!({ "dateTime": "2026-10-20T21:00:00", "timeZone": "Asia/Tokyo" }));
        assert_eq!((written["isReminderOn"].as_bool(), written["reminderMinutesBeforeStart"].as_i64()), (Some(true), Some(60)));
        let none = body(&EventChanges { reminders: Some(Vec::new()), ..Default::default() }).unwrap();
        assert_eq!(none["isReminderOn"].as_bool(), Some(false));
        let item = serde_json::json!({ "id": "Z", "start": { "dateTime": "2026-10-20T12:00:00" }, "originalStartTimeZone": "Tokyo Standard Time" });
        assert_eq!(map_event(&item).unwrap().zone.as_deref(), Some("Asia/Tokyo"));
    }

    #[test]
    fn a_refusal_asks_to_reconnect() {
        let denied = br#"{"error":{"code":"ErrorAccessDenied","message":"Access is denied. Check credentials and try again."}}"#;
        assert!(matches!(api_error(reqwest::StatusCode::FORBIDDEN, denied), Error::Auth(_)));
        assert!(matches!(api_error(reqwest::StatusCode::UNAUTHORIZED, b"{}"), Error::Auth(_)));
        let organizer_only = br#"{"error":{"code":"ErrorAccessDenied","message":"Only the organizer can change this event."}}"#;
        assert!(matches!(api_error(reqwest::StatusCode::FORBIDDEN, organizer_only), Error::Other(_)), "reconnecting would not help");
        assert!(matches!(api_error(reqwest::StatusCode::BAD_REQUEST, br#"{"error":{"message":"x"}}"#), Error::Other(_)));
    }
}
