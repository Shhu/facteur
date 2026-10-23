//! iCalendar (RFC 5545) invitations received by mail (iMIP, RFC 6047), replies to them, and the
//! expansion of calendar events.

use std::collections::HashSet;

use chrono::{NaiveDate, NaiveDateTime, TimeZone};
use serde::Serialize;

use crate::model::Address;

/// An unfolded iCalendar line; `raw` is copied as is into the reply.
#[derive(Debug, Clone)]
struct Line {
    name: String,
    params: Vec<(String, String)>,
    value: String,
    raw: String,
}

impl Line {
    fn param(&self, key: &str) -> Option<&str> {
        self.params.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v.as_str())
    }
}

fn unfold(ics: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in ics.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)) {
        match (line.strip_prefix([' ', '\t']), out.last_mut()) {
            (Some(rest), Some(last)) => last.push_str(rest),
            _ if line.is_empty() => {}
            _ => out.push(line.to_string()),
        }
    }
    out
}

fn parse_line(raw: &str) -> Option<Line> {
    // The first `:` outside quotes separates name and parameters from the value.
    let mut quoted = false;
    let split = raw.char_indices().find(|&(_, c)| {
        if c == '"' {
            quoted = !quoted;
        }
        c == ':' && !quoted
    })?;
    let (head, value) = (&raw[..split.0], &raw[split.0 + 1..]);
    let mut parts = head.split(';');
    let name = parts.next()?.trim().to_ascii_uppercase();
    let params = parts
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (k.trim().to_ascii_uppercase(), v.trim().trim_matches('"').to_string()))
        .collect();
    Some(Line { name, params, value: value.to_string(), raw: raw.to_string() })
}

fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n' | 'N') => out.push('\n'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn mailto(v: &str) -> String {
    let v = v.trim();
    v.strip_prefix("mailto:").or_else(|| v.strip_prefix("MAILTO:")).unwrap_or(v).to_string()
}

/// Start or end of an invitation (DTSTART, DTEND): Unix time, and whether it is a whole day
/// (then the computer's local midnight, as the invitation card shows it).
fn when(line: &Line, zones: &Zones) -> Option<(i64, bool)> {
    match stamp_of(line, zones)? {
        Stamp::Date(d) => Some((local_midnight(d)?, true)),
        s => Some((s.timestamp()?, false)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Zone {
    Utc,
    Named(chrono_tz::Tz),
    /// A VTIMEZONE without daylight saving time that no IANA zone matches.
    Fixed(chrono::FixedOffset),
    /// Floating time, or an unknown TZID: the computer's time.
    Local,
}

/// Instant in `tz` for a wall time; in a DST gap (a time that does not exist), an hour later.
fn resolve<T: TimeZone>(tz: &T, t: NaiveDateTime) -> Option<i64> {
    tz.from_local_datetime(&t)
        .earliest()
        .or_else(|| tz.from_local_datetime(&(t + chrono::Duration::hours(1))).earliest())
        .map(|d| d.timestamp())
}

impl Zone {
    fn timestamp(self, t: NaiveDateTime) -> Option<i64> {
        match self {
            Zone::Utc => Some(t.and_utc().timestamp()),
            Zone::Named(tz) => resolve(&tz, t),
            Zone::Fixed(offset) => resolve(&offset, t),
            Zone::Local => resolve(&chrono::Local, t),
        }
    }
}

/// Windows zone names (Outlook, Exchange) and their IANA equivalent: the CLDR windowsZones
/// table (territory 001), plus a few names older Windows versions still send.
const WINDOWS_ZONES: &[(&str, &str)] = &[
    ("Dateline Standard Time", "Etc/GMT+12"), ("UTC-11", "Etc/GMT+11"), ("Aleutian Standard Time", "America/Adak"),
    ("Hawaiian Standard Time", "Pacific/Honolulu"), ("Marquesas Standard Time", "Pacific/Marquesas"),
    ("Alaskan Standard Time", "America/Anchorage"), ("UTC-09", "Etc/GMT+9"), ("Pacific Standard Time (Mexico)", "America/Tijuana"),
    ("UTC-08", "Etc/GMT+8"), ("Pacific Standard Time", "America/Los_Angeles"), ("US Mountain Standard Time", "America/Phoenix"),
    ("Mountain Standard Time (Mexico)", "America/Mazatlan"), ("Mountain Standard Time", "America/Denver"),
    ("Yukon Standard Time", "America/Whitehorse"), ("Central America Standard Time", "America/Guatemala"),
    ("Central Standard Time", "America/Chicago"), ("Easter Island Standard Time", "Pacific/Easter"),
    ("Central Standard Time (Mexico)", "America/Mexico_City"), ("Canada Central Standard Time", "America/Regina"),
    ("SA Pacific Standard Time", "America/Bogota"), ("Eastern Standard Time (Mexico)", "America/Cancun"),
    ("Eastern Standard Time", "America/New_York"), ("Haiti Standard Time", "America/Port-au-Prince"),
    ("Cuba Standard Time", "America/Havana"), ("US Eastern Standard Time", "America/Indiana/Indianapolis"),
    ("Turks And Caicos Standard Time", "America/Grand_Turk"), ("Paraguay Standard Time", "America/Asuncion"),
    ("Atlantic Standard Time", "America/Halifax"), ("Venezuela Standard Time", "America/Caracas"),
    ("Central Brazilian Standard Time", "America/Cuiaba"), ("SA Western Standard Time", "America/La_Paz"),
    ("Pacific SA Standard Time", "America/Santiago"), ("Newfoundland Standard Time", "America/St_Johns"),
    ("Tocantins Standard Time", "America/Araguaina"), ("E. South America Standard Time", "America/Sao_Paulo"),
    ("SA Eastern Standard Time", "America/Cayenne"), ("Argentina Standard Time", "America/Argentina/Buenos_Aires"),
    ("Greenland Standard Time", "America/Nuuk"), ("Montevideo Standard Time", "America/Montevideo"),
    ("Magallanes Standard Time", "America/Punta_Arenas"), ("Saint Pierre Standard Time", "America/Miquelon"),
    ("Bahia Standard Time", "America/Bahia"), ("UTC-02", "Etc/GMT+2"), ("Mid-Atlantic Standard Time", "Etc/GMT+2"),
    ("Azores Standard Time", "Atlantic/Azores"), ("Cape Verde Standard Time", "Atlantic/Cape_Verde"), ("UTC", "Etc/UTC"),
    ("Coordinated Universal Time", "Etc/UTC"), ("GMT Standard Time", "Europe/London"), ("Greenwich Standard Time", "Atlantic/Reykjavik"),
    ("Sao Tome Standard Time", "Africa/Sao_Tome"), ("Morocco Standard Time", "Africa/Casablanca"),
    ("W. Europe Standard Time", "Europe/Berlin"), ("Central Europe Standard Time", "Europe/Budapest"),
    ("Romance Standard Time", "Europe/Paris"), ("Central European Standard Time", "Europe/Warsaw"),
    ("W. Central Africa Standard Time", "Africa/Lagos"), ("Jordan Standard Time", "Asia/Amman"),
    ("GTB Standard Time", "Europe/Bucharest"), ("Middle East Standard Time", "Asia/Beirut"), ("Egypt Standard Time", "Africa/Cairo"),
    ("E. Europe Standard Time", "Europe/Chisinau"), ("Syria Standard Time", "Asia/Damascus"), ("West Bank Standard Time", "Asia/Hebron"),
    ("South Africa Standard Time", "Africa/Johannesburg"), ("FLE Standard Time", "Europe/Kiev"), ("Israel Standard Time", "Asia/Jerusalem"),
    ("South Sudan Standard Time", "Africa/Juba"), ("Kaliningrad Standard Time", "Europe/Kaliningrad"),
    ("Sudan Standard Time", "Africa/Khartoum"), ("Libya Standard Time", "Africa/Tripoli"), ("Namibia Standard Time", "Africa/Windhoek"),
    ("Arabic Standard Time", "Asia/Baghdad"), ("Turkey Standard Time", "Europe/Istanbul"), ("Arab Standard Time", "Asia/Riyadh"),
    ("Belarus Standard Time", "Europe/Minsk"), ("Russian Standard Time", "Europe/Moscow"), ("E. Africa Standard Time", "Africa/Nairobi"),
    ("Volgograd Standard Time", "Europe/Volgograd"), ("Iran Standard Time", "Asia/Tehran"), ("Arabian Standard Time", "Asia/Dubai"),
    ("Astrakhan Standard Time", "Europe/Astrakhan"), ("Azerbaijan Standard Time", "Asia/Baku"), ("Russia Time Zone 3", "Europe/Samara"),
    ("Mauritius Standard Time", "Indian/Mauritius"), ("Saratov Standard Time", "Europe/Saratov"), ("Georgian Standard Time", "Asia/Tbilisi"),
    ("Caucasus Standard Time", "Asia/Yerevan"), ("Armenian Standard Time", "Asia/Yerevan"), ("Afghanistan Standard Time", "Asia/Kabul"),
    ("West Asia Standard Time", "Asia/Tashkent"), ("Ekaterinburg Standard Time", "Asia/Yekaterinburg"),
    ("Pakistan Standard Time", "Asia/Karachi"), ("Qyzylorda Standard Time", "Asia/Qyzylorda"), ("India Standard Time", "Asia/Kolkata"),
    ("Sri Lanka Standard Time", "Asia/Colombo"), ("Nepal Standard Time", "Asia/Kathmandu"), ("Central Asia Standard Time", "Asia/Bishkek"),
    ("Bangladesh Standard Time", "Asia/Dhaka"), ("Omsk Standard Time", "Asia/Omsk"), ("Myanmar Standard Time", "Asia/Yangon"),
    ("SE Asia Standard Time", "Asia/Bangkok"), ("Altai Standard Time", "Asia/Barnaul"), ("W. Mongolia Standard Time", "Asia/Hovd"),
    ("North Asia Standard Time", "Asia/Krasnoyarsk"), ("N. Central Asia Standard Time", "Asia/Novosibirsk"),
    ("Tomsk Standard Time", "Asia/Tomsk"), ("China Standard Time", "Asia/Shanghai"), ("North Asia East Standard Time", "Asia/Irkutsk"),
    ("Singapore Standard Time", "Asia/Singapore"), ("W. Australia Standard Time", "Australia/Perth"), ("Taipei Standard Time", "Asia/Taipei"),
    ("Ulaanbaatar Standard Time", "Asia/Ulaanbaatar"), ("Aus Central W. Standard Time", "Australia/Eucla"),
    ("Transbaikal Standard Time", "Asia/Chita"), ("Tokyo Standard Time", "Asia/Tokyo"), ("North Korea Standard Time", "Asia/Pyongyang"),
    ("Korea Standard Time", "Asia/Seoul"), ("Yakutsk Standard Time", "Asia/Yakutsk"), ("Cen. Australia Standard Time", "Australia/Adelaide"),
    ("AUS Central Standard Time", "Australia/Darwin"), ("E. Australia Standard Time", "Australia/Brisbane"),
    ("AUS Eastern Standard Time", "Australia/Sydney"), ("West Pacific Standard Time", "Pacific/Port_Moresby"),
    ("Tasmania Standard Time", "Australia/Hobart"), ("Vladivostok Standard Time", "Asia/Vladivostok"),
    ("Lord Howe Standard Time", "Australia/Lord_Howe"), ("Bougainville Standard Time", "Pacific/Bougainville"),
    ("Russia Time Zone 10", "Asia/Srednekolymsk"), ("Magadan Standard Time", "Asia/Magadan"), ("Norfolk Standard Time", "Pacific/Norfolk"),
    ("Sakhalin Standard Time", "Asia/Sakhalin"), ("Central Pacific Standard Time", "Pacific/Guadalcanal"),
    ("Russia Time Zone 11", "Asia/Kamchatka"), ("Kamchatka Standard Time", "Asia/Kamchatka"), ("New Zealand Standard Time", "Pacific/Auckland"),
    ("UTC+12", "Etc/GMT-12"), ("Fiji Standard Time", "Pacific/Fiji"), ("Chatham Islands Standard Time", "Pacific/Chatham"),
    ("UTC+13", "Etc/GMT-13"), ("Tonga Standard Time", "Pacific/Tongatapu"), ("Samoa Standard Time", "Pacific/Apia"),
    ("Line Islands Standard Time", "Pacific/Kiritimati"), ("Mexico Standard Time", "America/Mexico_City"),
    ("Mexico Standard Time 2", "America/Chihuahua"),
];

/// IANA zone named by a TZID: as is, after a vendor prefix (`/mozilla.org/20050126_1/Europe/Paris`),
/// or a Windows name.
fn named_zone(tzid: &str) -> Option<chrono_tz::Tz> {
    let tzid = tzid.trim();
    if let Ok(tz) = tzid.parse() {
        return Some(tz);
    }
    if let Some(tz) = tzid.match_indices('/').find_map(|(i, _)| tzid[i + 1..].parse().ok()) {
        return Some(tz);
    }
    WINDOWS_ZONES.iter().find(|(w, _)| w.eq_ignore_ascii_case(tzid)).and_then(|(_, iana)| iana.parse().ok())
}

/// The zones of the VTIMEZONE blocks of one iCalendar object, for the TZIDs no name resolves
/// (an Outlook display name, a custom zone).
#[derive(Default)]
struct Zones(Vec<(String, Zone)>);

impl Zones {
    fn read(lines: &[Line]) -> Zones {
        // An invitation names one zone or two: a few dozen more can only be a message built to
        // make each one computed (see [`ZONE_PERIODS`]).
        const MAX_ZONES: usize = 32;
        const MAX_OBSERVANCES: usize = 16;
        let mut out = Vec::new();
        let mut seen = 0;
        let mut tzid: Option<String> = None;
        let mut observances: Vec<Observance> = Vec::new();
        let mut current: Option<Vec<&Line>> = None;
        for l in lines {
            let value = l.value.trim().to_ascii_uppercase();
            match (l.name.as_str(), value.as_str()) {
                ("BEGIN", "VTIMEZONE") => {
                    tzid = None;
                    observances.clear();
                    seen += 1;
                }
                ("BEGIN", "STANDARD" | "DAYLIGHT") => current = Some(Vec::new()),
                ("END", "STANDARD" | "DAYLIGHT") => {
                    let read = current.take().and_then(|c| Observance::read(&c));
                    if observances.len() < MAX_OBSERVANCES {
                        observances.extend(read);
                    }
                }
                ("END", "VTIMEZONE") => {
                    if seen <= MAX_ZONES
                        && let Some(id) = tzid.take()
                        && named_zone(&id).is_none()
                        && let Some(zone) = Observance::zone(&observances)
                    {
                        out.push((id, zone));
                    }
                }
                ("TZID", _) if current.is_none() => tzid = Some(l.value.trim().to_string()),
                _ => {
                    if let Some(c) = current.as_mut() {
                        c.push(l);
                    }
                }
            }
        }
        Zones(out)
    }

    /// The zone of a TZID parameter (`None`: floating time, the computer's).
    fn of(&self, tzid: Option<&str>) -> Zone {
        let Some(tzid) = tzid else { return Zone::Local };
        named_zone(tzid)
            .map(Zone::Named)
            .or_else(|| self.0.iter().find(|(id, _)| id == tzid.trim()).map(|(_, z)| *z))
            .unwrap_or(Zone::Local)
    }
}

/// A STANDARD or DAYLIGHT part of a VTIMEZONE: from its onset (DTSTART, then RRULE and RDATE,
/// wall times in the offset before it), the offset is `to` (seconds east of UTC).
struct Observance {
    start: NaiveDateTime,
    from: i32,
    to: i32,
    rule: Option<Rule>,
    rdates: Vec<NaiveDateTime>,
}

/// `+0200`, `-0500`, `+053000`: seconds east of UTC.
fn utc_offset(v: &str) -> Option<i32> {
    let v = v.trim();
    let (sign, digits) = match v.split_at_checked(1)? {
        ("+", d) => (1, d),
        ("-", d) => (-1, d),
        _ => return None,
    };
    if !matches!(digits.len(), 4 | 6) || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n = |r: std::ops::Range<usize>| digits.get(r).map_or(0, |d| d.parse::<i32>().unwrap_or(0));
    Some(sign * (n(0..2) * 3600 + n(2..4) * 60 + n(4..6)))
}

impl Observance {
    fn read(lines: &[&Line]) -> Option<Observance> {
        let get = |name: &str| lines.iter().find(|l| l.name == name);
        let naive = |v: &str| NaiveDateTime::parse_from_str(v.trim().trim_end_matches(['Z', 'z']), "%Y%m%dT%H%M%S").ok();
        Some(Observance {
            start: naive(&get("DTSTART")?.value)?,
            from: utc_offset(&get("TZOFFSETFROM")?.value)?,
            to: utc_offset(&get("TZOFFSETTO")?.value)?,
            rule: get("RRULE").and_then(|l| Rule::parse(&l.value)),
            rdates: lines.iter().filter(|l| l.name == "RDATE").flat_map(|l| l.value.split(',')).filter_map(naive).collect(),
        })
    }

    fn last_onset(&self, at: i64) -> Option<i64> {
        let utc = |t: NaiveDateTime| t.and_utc().timestamp() - i64::from(self.from);
        let wall = at + i64::from(self.from);
        let mut onsets: Vec<i64> = std::iter::once(self.start).chain(self.rdates.iter().copied()).map(utc).collect();
        if let Some(rule) = &self.rule {
            // Wall times, computed as if in UTC; two years back are enough for a yearly change.
            let starts = rule.occurrences_within(Stamp::Time(self.start, Zone::Utc), wall - 800 * 86400, wall + 1, ZONE_PERIODS);
            onsets.extend(starts.into_iter().map(|s| utc(s.naive())));
        }
        onsets.into_iter().filter(|t| *t <= at).max()
    }

    /// Offset of a VTIMEZONE at `at`: that of its last onset (before them all, the first one's
    /// offset before it).
    fn offset_at(observances: &[Observance], at: i64) -> Option<i32> {
        observances
            .iter()
            .filter_map(|o| o.last_onset(at).map(|t| (t, o.to)))
            .max_by_key(|(t, _)| *t)
            .map(|(_, to)| to)
            .or_else(|| observances.iter().min_by_key(|o| o.start).map(|o| o.from))
    }

    /// The IANA zone keeping the same time as a VTIMEZONE through this year (sampled weekly),
    /// or the one closest to it; a fixed offset when no zone has its offsets.
    fn zone(observances: &[Observance]) -> Option<Zone> {
        use chrono::{Datelike, Offset};
        let year = chrono::Utc::now().year();
        let jan = NaiveDate::from_ymd_opt(year, 1, 1)?.and_time(chrono::NaiveTime::MIN).and_utc().timestamp();
        let samples: Vec<(i64, i32)> =
            (0..53).filter_map(|w| Some((jan + w * 7 * 86400, Self::offset_at(observances, jan + w * 7 * 86400)?))).collect();
        let first = samples.first()?.1;
        if samples.iter().all(|(_, o)| *o == first) {
            return chrono::FixedOffset::east_opt(first).map(Zone::Fixed);
        }
        let offset = |tz: &chrono_tz::Tz, t: i64| {
            chrono::DateTime::from_timestamp(t, 0).map(|d| tz.offset_from_utc_datetime(&d.naive_utc()).fix().local_minus_utc())
        };
        let (winter, summer) = (samples[0], samples[samples.len() / 2]);
        chrono_tz::TZ_VARIANTS
            .iter()
            .filter(|tz| offset(tz, winter.0) == Some(winter.1) && offset(tz, summer.0) == Some(summer.1))
            .map(|tz| (tz, samples.iter().filter(|(t, o)| offset(tz, *t) == Some(*o)).count()))
            // The first of the best: `max_by_key` would keep the last.
            .fold(None::<(&chrono_tz::Tz, usize)>, |best, c| if best.is_none_or(|b| c.1 > b.1) { Some(c) } else { best })
            .map(|(tz, _)| Zone::Named(*tz))
            .or_else(|| chrono::FixedOffset::east_opt(winter.1).map(Zone::Fixed))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Stamp {
    Date(NaiveDate),
    Time(NaiveDateTime, Zone),
}

/// Unix time of a calendar day for storage: its UTC midnight, the same whatever the computer's
/// zone (the store turns it into the local midnight when read).
fn day_stamp(d: NaiveDate) -> i64 {
    d.and_time(chrono::NaiveTime::MIN).and_utc().timestamp()
}

impl Stamp {
    /// `zone`: that of the TZID parameter, for a time without `Z`.
    fn parse(value: &str, zone: Zone, date: bool) -> Option<Stamp> {
        let v = value.trim();
        if date || v.len() == 8 {
            return NaiveDate::parse_from_str(v, "%Y%m%d").ok().map(Stamp::Date);
        }
        if let Some(utc) = v.strip_suffix(['Z', 'z']) {
            return NaiveDateTime::parse_from_str(utc, "%Y%m%dT%H%M%S").ok().map(|t| Stamp::Time(t, Zone::Utc));
        }
        let t = NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S").ok()?;
        Some(Stamp::Time(t, zone))
    }

    /// Unix time; a day is its UTC midnight (see [`day_stamp`]).
    fn timestamp(self) -> Option<i64> {
        match self {
            Stamp::Date(d) => Some(day_stamp(d)),
            Stamp::Time(t, zone) => zone.timestamp(t),
        }
    }

    fn naive(self) -> NaiveDateTime {
        match self {
            Stamp::Date(d) => d.and_time(chrono::NaiveTime::MIN),
            Stamp::Time(t, _) => t,
        }
    }

    fn with(self, t: NaiveDateTime) -> Stamp {
        match self {
            Stamp::Date(_) => Stamp::Date(t.date()),
            Stamp::Time(_, zone) => Stamp::Time(t, zone),
        }
    }

    /// A floating time (no `Z`, no TZID) read as one in the zone of `start` (DTSTART): EXDATE,
    /// RDATE and UNTIL values are meant in it.
    fn in_zone_of(self, start: Stamp) -> Stamp {
        match (start, self) {
            (Stamp::Time(_, zone), Stamp::Time(t, Zone::Local)) => Stamp::Time(t, zone),
            _ => self,
        }
    }
}

fn stamp_of(line: &Line, zones: &Zones) -> Option<Stamp> {
    let date = line.param("VALUE").is_some_and(|v| v.eq_ignore_ascii_case("DATE"));
    Stamp::parse(&line.value, zones.of(line.param("TZID")), date)
}

fn stamps_of(line: &Line, zones: &Zones) -> Vec<Stamp> {
    let date = line.param("VALUE").is_some_and(|v| v.eq_ignore_ascii_case("DATE"));
    let zone = zones.of(line.param("TZID"));
    line.value.split(',').filter_map(|v| Stamp::parse(v, zone, date)).collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct Attendee {
    pub address: Address,
    pub partstat: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Invite {
    pub method: String,
    pub uid: String,
    pub summary: String,
    pub location: Option<String>,
    pub description: Option<String>,
    pub start: Option<i64>,
    pub end: Option<i64>,
    pub all_day: bool,
    /// Zone the invitation names, when not UTC.
    pub tzid: Option<String>,
    pub organizer: Option<Address>,
    pub attendees: Vec<Attendee>,
    pub recurring: bool,
    pub cancelled: bool,
    /// Revision of the event (SEQUENCE): above 0, the invitation is an update.
    pub sequence: u32,
    /// Reply already sent from facteur.
    pub my_response: Option<String>,
    /// A recurring timed invitation: its next occurrence not over yet (start, end), when the
    /// rule is understood. `None` otherwise.
    pub next_start: Option<i64>,
    pub next_end: Option<i64>,
}

pub fn parse(ics: &str) -> Option<Invite> {
    let lines: Vec<Line> = unfold(ics).iter().filter_map(|l| parse_line(l)).collect();
    let method = lines
        .iter()
        .take_while(|l| !(l.name == "BEGIN" && l.value.eq_ignore_ascii_case("VEVENT")))
        .find(|l| l.name == "METHOD")
        .map(|l| l.value.trim().to_ascii_uppercase())
        .unwrap_or_else(|| "PUBLISH".into());
    let event = event_lines(&lines)?;
    let zones = Zones::read(&lines);
    let get = |name: &str| event.iter().find(|l| l.name == name);
    let start = get("DTSTART").and_then(|l| when(l, &zones));
    let end = get("DTEND").and_then(|l| when(l, &zones)).map(|(t, _)| t).or_else(|| {
        get("DURATION")?;
        let shape = Shape::read(&event, &zones)?;
        match (shape.start, shape.length) {
            (Stamp::Date(d), Length::Days(n)) => local_midnight(d.checked_add_signed(chrono::Duration::try_days(n)?)?),
            (at, length) => shape.end(at, length),
        }
    });
    let uid = get("UID")?.value.trim().to_string();
    let recurring = get("RRULE").is_some();
    let next = (recurring && start.is_some_and(|(_, day)| !day))
        .then(|| {
            let now = chrono::Utc::now().timestamp();
            instances(ics, now, now + 2 * 366 * 86400, &[]).into_iter().filter(|i| i.uid == uid && i.end > now).min_by_key(|i| i.start)
        })
        .flatten();
    let address = |l: &Line| Address { name: l.param("CN").map(str::to_string).filter(|n| !n.is_empty()), email: mailto(&l.value) };
    Some(Invite {
        uid,
        summary: get("SUMMARY").map(|l| unescape(&l.value)).unwrap_or_default(),
        location: get("LOCATION").map(|l| unescape(&l.value)).filter(|s| !s.trim().is_empty()),
        description: get("DESCRIPTION").map(|l| unescape(&l.value)).filter(|s| !s.trim().is_empty()),
        start: start.map(|(t, _)| t),
        end,
        all_day: start.is_some_and(|(_, d)| d),
        tzid: get("DTSTART").and_then(|l| l.param("TZID")).map(str::to_string),
        organizer: get("ORGANIZER").map(address),
        attendees: event
            .iter()
            .filter(|l| l.name == "ATTENDEE")
            .map(|l| Attendee { address: address(l), partstat: l.param("PARTSTAT").map(str::to_ascii_uppercase) })
            .collect(),
        recurring,
        next_start: next.as_ref().map(|i| i.start),
        next_end: next.as_ref().map(|i| i.end),
        sequence: get("SEQUENCE").and_then(|l| l.value.trim().parse().ok()).unwrap_or(0),
        cancelled: method == "CANCEL" || get("STATUS").is_some_and(|l| l.value.trim().eq_ignore_ascii_case("CANCELLED")),
        method,
        my_response: None,
    })
}

fn event_lines(lines: &[Line]) -> Option<Vec<Line>> {
    event_blocks(lines).into_iter().next()
}

/// An event's alarms come as lines of this name (their TRIGGER, its parameters kept), for those
/// shown on screen or rung (not those sending a mail).
const ALARM: &str = "X-FACTEUR-ALARM";

fn event_blocks(lines: &[Line]) -> Vec<Vec<Line>> {
    let mut blocks = Vec::new();
    let mut current: Option<Vec<Line>> = None;
    let mut depth = 0;
    // Inside a VALARM: its TRIGGER, and whether it only sends a mail.
    let mut alarm: Option<(Option<Line>, bool)> = None;
    for l in lines {
        let value = l.value.trim().to_ascii_uppercase();
        let Some(out) = current.as_mut() else {
            if l.name == "BEGIN" && value == "VEVENT" {
                current = Some(Vec::new());
                depth = 0;
            }
            continue;
        };
        match (l.name.as_str(), value.as_str()) {
            ("END", "VEVENT") if depth == 0 => blocks.extend(current.take()),
            ("BEGIN", "VALARM") if depth == 0 => {
                depth += 1;
                alarm = Some((None, false));
            }
            ("END", "VALARM") if depth == 1 => {
                depth -= 1;
                if let Some((Some(trigger), false)) = alarm.take() {
                    out.push(Line { name: ALARM.into(), ..trigger });
                }
            }
            ("BEGIN", _) => depth += 1,
            ("END", _) => depth -= 1,
            _ if depth == 0 => out.push(l.clone()),
            ("TRIGGER", _) if depth == 1 => {
                if let Some(a) = alarm.as_mut() {
                    a.0 = Some(l.clone());
                }
            }
            ("ACTION", action) if depth == 1 => {
                if let Some(a) = alarm.as_mut() {
                    a.1 = matches!(action, "EMAIL" | "NONE");
                }
            }
            _ => {}
        }
    }
    // Unterminated last event: kept, as the invitation reader always did.
    blocks.extend(current);
    blocks
}

// ── Calendar events ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Instance {
    pub uid: String,
    /// Tells the occurrences of a series apart: the original start of the occurrence (Unix
    /// time, as text), or a provider's own instance id; empty for a single event.
    pub instance: String,
    pub summary: String,
    pub location: Option<String>,
    pub description: Option<String>,
    /// Unix time. A whole-day event starts at the UTC midnight of its first day (the store
    /// turns it into the local midnight when read).
    pub start: i64,
    /// Exclusive. For a whole-day event, the UTC midnight after its last day.
    pub end: i64,
    pub all_day: bool,
    pub status: Option<String>,
    /// The organizer's name, or address when the name is not given.
    pub organizer: Option<String>,
    pub url: Option<String>,
    /// Takes the time (free/busy): not shown as free (TRANSP, Google's transparency), not
    /// declined by the account, not cancelled.
    pub busy: bool,
    /// The CalDAV resource holding the event.
    pub href: Option<String>,
    /// An occurrence of a series.
    pub recurring: bool,
    /// Organized by the account (or by nobody): someone else's invitation is answered, not edited.
    pub own: bool,
    /// The provider's id of the whole series an occurrence belongs to (Google, Outlook).
    pub series: Option<String>,
    /// The account's answer to someone else's invitation: `needs_action`, `accepted`,
    /// `tentative` or `declined`; `None` when it is not invited.
    pub response: Option<String>,
    pub attendees: Vec<crate::model::EventAttendee>,
    pub meeting: Option<String>,
    /// Minutes before the start.
    pub reminders: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RemoteCalendar {
    /// Google calendar id, or the CalDAV collection's address.
    pub remote_id: String,
    pub name: String,
    pub color: Option<String>,
    pub read_only: bool,
    /// Shown by default when first seen (Google: the calendar is selected in its web interface).
    pub enabled: bool,
    /// Changes whenever an event of the calendar changes (CalDAV getctag or sync-token).
    pub ctag: Option<String>,
}

/// `#rrggbb` from `#RRGGBB` or `#RRGGBBAA` (Apple's calendar-color).
pub(crate) fn hex_color(v: &str) -> Option<String> {
    let hex = v.trim().strip_prefix('#')?;
    (matches!(hex.len(), 6 | 8) && hex.chars().all(|c| c.is_ascii_hexdigit())).then(|| format!("#{}", hex[..6].to_ascii_lowercase()))
}

/// How long an event lasts: whole days (for a day event), or, for a timed one, days on the wall
/// clock (a day across a change of time is 23 or 25 hours) then seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Length {
    Days(i64),
    Timed { days: i64, secs: i64 },
}

impl Length {
    fn max_secs(self) -> i64 {
        match self {
            Length::Days(n) => n * 86400,
            Length::Timed { days, secs } => days * 86400 + secs + 3600,
        }
    }
}

const MAX_DURATION_DAYS: i64 = 36_600;

/// `P1D`, `PT1H30M`, `P2W`, `-PT15M` (RFC 5545 §3.3.6): (days, seconds), weeks counted as
/// 7 days. Days are nominal (wall clock), seconds exact.
fn duration(v: &str) -> Option<(i64, i64)> {
    let v = v.trim();
    let (sign, v) = match v.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, v.strip_prefix('+').unwrap_or(v)),
    };
    let mut rest = v.strip_prefix(['P', 'p'])?;
    let (mut days, mut secs) = (0i64, 0i64);
    let mut time = false;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix(['T', 't']) {
            time = true;
            rest = r;
            continue;
        }
        let digits = rest.find(|c: char| !c.is_ascii_digit())?;
        let n: i64 = rest[..digits].parse().ok()?;
        // Checked: the value comes from strangers, and an overflow would panic further on.
        match (rest.as_bytes()[digits].to_ascii_uppercase(), time) {
            (b'W', false) => days = days.checked_add(n.checked_mul(7)?)?,
            (b'D', false) => days = days.checked_add(n)?,
            (b'H', true) => secs = secs.checked_add(n.checked_mul(3600)?)?,
            (b'M', true) => secs = secs.checked_add(n.checked_mul(60)?)?,
            (b'S', true) => secs = secs.checked_add(n)?,
            _ => return None,
        }
        // `digits` is the byte of an ASCII letter, so the slice stays on a char boundary.
        rest = &rest[digits + 1..];
    }
    // About a century at most: longer is no real event, and would overflow date arithmetic.
    if days > MAX_DURATION_DAYS || secs > MAX_DURATION_DAYS * 86400 {
        return None;
    }
    Some((sign * days, sign * secs))
}

/// An iCalendar PARTSTAT as facteur names answers.
fn response_of(partstat: Option<&str>) -> String {
    match partstat.map(str::to_ascii_uppercase).as_deref() {
        Some("ACCEPTED") => "accepted",
        Some("TENTATIVE") => "tentative",
        Some("DECLINED") => "declined",
        _ => "needs_action",
    }
    .to_string()
}

fn local_midnight(d: NaiveDate) -> Option<i64> {
    resolve(&chrono::Local, d.and_hms_opt(0, 0, 0)?)
}

struct Shape<'a> {
    lines: &'a [Line],
    start: Stamp,
    length: Length,
    busy: bool,
    own: bool,
    response: Option<String>,
}

impl<'a> Shape<'a> {
    fn read(lines: &'a [Line], zones: &Zones) -> Option<Self> {
        Self::read_for(lines, zones, &[])
    }

    /// `me`: the account's addresses, whose PARTSTAT tells whether the event was declined.
    fn read_for(lines: &'a [Line], zones: &Zones, me: &[String]) -> Option<Self> {
        let get = |name: &str| lines.iter().find(|l| l.name == name);
        let start = stamp_of(get("DTSTART")?, zones)?;
        let end = get("DTEND").and_then(|l| stamp_of(l, zones));
        let dur = get("DURATION").and_then(|l| duration(&l.value));
        let length = match (start, end) {
            (Stamp::Date(s), Some(Stamp::Date(e))) => Length::Days((e - s).num_days().max(1)),
            (Stamp::Date(_), _) => Length::Days(dur.map_or(1, |(d, s)| (d + s.div_euclid(86400)).max(1))),
            (_, Some(e)) => Length::Timed { days: 0, secs: (e.timestamp()? - start.timestamp()?).max(0) },
            (_, None) => dur.map_or(Length::Timed { days: 0, secs: 0 }, |(days, secs)| Length::Timed { days, secs }),
        };
        let transparent = get("TRANSP").is_some_and(|l| l.value.trim().eq_ignore_ascii_case("TRANSPARENT"));
        let cancelled = get("STATUS").is_some_and(|l| l.value.trim().eq_ignore_ascii_case("CANCELLED"));
        let declined = lines.iter().filter(|l| l.name == "ATTENDEE").any(|l| {
            let address = mailto(&l.value);
            me.iter().any(|m| m.eq_ignore_ascii_case(&address)) && l.param("PARTSTAT").is_some_and(|p| p.eq_ignore_ascii_case("DECLINED"))
        });
        let own = get("ORGANIZER").is_none_or(|l| {
            let address = mailto(&l.value);
            address.is_empty() || me.iter().any(|m| m.eq_ignore_ascii_case(&address))
        });
        let response = lines
            .iter()
            .filter(|l| l.name == "ATTENDEE")
            .find(|l| me.iter().any(|m| m.eq_ignore_ascii_case(&mailto(&l.value))))
            .map(|l| response_of(l.param("PARTSTAT")));
        Some(Shape { lines, start, length, busy: !transparent && !cancelled && !declined, own, response })
    }

    fn get(&self, name: &str) -> Option<&Line> {
        self.lines.iter().find(|l| l.name == name)
    }

    fn text(&self, name: &str) -> Option<String> {
        self.get(name).map(|l| unescape(&l.value)).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    }

    fn end(&self, at: Stamp, length: Length) -> Option<i64> {
        let start = at.timestamp()?;
        Some(match (at, length) {
            (Stamp::Date(d), Length::Days(n)) => day_stamp(d.checked_add_signed(chrono::Duration::try_days(n)?)?),
            (Stamp::Date(_), Length::Timed { days, secs }) => start + (days + (secs.max(0) + 86399) / 86400).max(1) * 86400,
            (Stamp::Time(..), Length::Days(n)) => start + n * 86400,
            (Stamp::Time(t, zone), Length::Timed { days, secs }) => {
                let wall = if days == 0 { start } else { zone.timestamp(t.checked_add_signed(chrono::Duration::try_days(days)?)?)? };
                (wall + secs).max(start)
            }
        })
    }

    fn instance(&self, uid: &str, key: String, at: Stamp, recurring: bool) -> Option<Instance> {
        let start = at.timestamp()?;
        let end = self.end(at, self.length)?;
        let organizer = self.get("ORGANIZER").map(|l| {
            l.param("CN").map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(|| mailto(&l.value))
        });
        Some(Instance {
            uid: uid.to_string(),
            instance: key,
            summary: self.text("SUMMARY").unwrap_or_default(),
            location: self.text("LOCATION"),
            description: self.text("DESCRIPTION"),
            start,
            end,
            all_day: matches!(at, Stamp::Date(_)),
            status: self.get("STATUS").map(|l| l.value.trim().to_ascii_lowercase()).filter(|s| !s.is_empty()),
            organizer: organizer.filter(|o| !o.is_empty()),
            url: self.get("URL").map(|l| l.value.trim().to_string()).filter(|u| u.starts_with("https://") || u.starts_with("http://")),
            busy: self.busy,
            href: None,
            recurring,
            own: self.own,
            series: None,
            response: if self.own { None } else { self.response.clone() },
            attendees: self.attendees(),
            meeting: self.meeting(),
            reminders: self.reminders(start),
        })
    }

    /// The organizer first, then the guests (rooms and equipment left out).
    fn attendees(&self) -> Vec<crate::model::EventAttendee> {
        let person = |l: &Line| crate::model::EventAttendee {
            email: mailto(&l.value),
            name: l.param("CN").map(str::trim).filter(|n| !n.is_empty()).map(str::to_string),
            response: Some(response_of(l.param("PARTSTAT"))),
            organizer: false,
        };
        let mut out: Vec<crate::model::EventAttendee> = Vec::new();
        if let Some(o) = self.get("ORGANIZER").map(person).filter(|o| !o.email.is_empty()) {
            out.push(crate::model::EventAttendee { organizer: true, response: Some("accepted".into()), ..o });
        }
        for l in self.lines.iter().filter(|l| l.name == "ATTENDEE") {
            if l.param("CUTYPE").is_some_and(|t| t.eq_ignore_ascii_case("ROOM") || t.eq_ignore_ascii_case("RESOURCE")) {
                continue;
            }
            let a = person(l);
            if a.email.is_empty() {
                continue;
            }
            match out.iter_mut().find(|o| o.email.eq_ignore_ascii_case(&a.email)) {
                Some(o) => o.response = a.response,
                None => out.push(a),
            }
        }
        // An organizer alone is no guest list.
        if out.len() == 1 && out[0].organizer { Vec::new() } else { out }
    }

    fn meeting(&self) -> Option<String> {
        let own = ["X-GOOGLE-CONFERENCE", "X-MICROSOFT-SKYPETEAMSMEETINGURL", "X-MICROSOFT-ONLINEMEETINGCONFLINK", "URL", "LOCATION", "DESCRIPTION"];
        let texts: Vec<String> = own.iter().filter_map(|name| self.get(name)).map(|l| unescape(&l.value)).collect();
        meeting_link(texts.iter().map(String::as_str))
    }

    /// Minutes before `start` of its on-screen alarms (relative to the start, or at a date).
    fn reminders(&self, start: i64) -> Vec<i64> {
        let mut out: Vec<i64> = self
            .lines
            .iter()
            .filter(|l| l.name == ALARM && !l.param("RELATED").is_some_and(|r| r.eq_ignore_ascii_case("END")))
            .filter_map(|l| {
                let before = if l.param("VALUE").is_some_and(|v| v.eq_ignore_ascii_case("DATE-TIME")) {
                    start - parse_utc(&l.value)?
                } else {
                    let (days, secs) = duration(&l.value)?;
                    -(days * 86400 + secs)
                };
                (0..=28 * 86400).contains(&before).then_some(before / 60)
            })
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// An absolute alarm time (always in UTC, RFC 5545).
fn parse_utc(v: &str) -> Option<i64> {
    let v = v.trim();
    let v = v.strip_suffix('Z').or_else(|| v.strip_suffix('z'))?;
    Some(chrono::NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S").ok()?.and_utc().timestamp())
}

/// The video meeting link found in these texts (a provider's own field first): Google Meet,
/// Teams, Zoom, Webex, Whereby, Jitsi, GoTo.
pub(crate) fn meeting_link<'a>(texts: impl IntoIterator<Item = &'a str>) -> Option<String> {
    const HOSTS: [&str; 9] = ["meet.google.com", "teams.microsoft.com", "teams.live.com", "zoom.us", "webex.com", "whereby.com", "meet.jit.si", "gotomeeting.com", "meet.goto.com"];
    for text in texts {
        let mut rest = text;
        while let Some(at) = rest.find("https://") {
            let candidate = &rest[at..];
            let end = candidate.find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | ')' | ']' | '|')).unwrap_or(candidate.len());
            let link = candidate[..end].trim_end_matches(['.', ',', ';', ':']);
            rest = &candidate[end..];
            let Ok(url) = reqwest::Url::parse(link) else { continue };
            let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
            if HOSTS.iter().any(|h| host == *h || host.ends_with(&format!(".{h}"))) {
                return Some(url.to_string());
            }
        }
    }
    None
}

/// Occurrences of the events of an iCalendar object (one CalDAV resource) overlapping
/// [`from`, `to`). Already expanded objects (CalDAV `expand`: one VEVENT per occurrence, each
/// with its RECURRENCE-ID) are read as they are; otherwise RRULE, RDATE and EXDATE are expanded
/// here (see [`Rule`] for what is understood), and overridden occurrences are replaced.
/// `me`: the account's addresses (an event they declined does not take the time).
pub fn instances(ics: &str, from: i64, to: i64, me: &[String]) -> Vec<Instance> {
    let lines: Vec<Line> = unfold(ics).iter().filter_map(|l| parse_line(l)).collect();
    let zones = Zones::read(&lines);
    let blocks = event_blocks(&lines);
    let mut out = Vec::new();
    let mut overridden: HashSet<(String, i64)> = HashSet::new();
    let uid_of = |b: &[Line]| b.iter().find(|l| l.name == "UID").map(|l| l.value.trim().to_string());
    // Overrides first: the series skips the occurrences they replace.
    for block in &blocks {
        let (Some(uid), Some(rid)) = (uid_of(block), block.iter().find(|l| l.name == "RECURRENCE-ID")) else { continue };
        let Some(shape) = Shape::read_for(block, &zones, me) else { continue };
        let Some(original) = stamp_of(rid, &zones).map(|s| s.in_zone_of(shape.start)).and_then(Stamp::timestamp) else { continue };
        overridden.insert((uid.clone(), original));
        out.extend(shape.instance(&uid, original.to_string(), shape.start, true));
    }
    for block in &blocks {
        if block.iter().any(|l| l.name == "RECURRENCE-ID") {
            continue;
        }
        let (Some(uid), Some(shape)) = (uid_of(block), Shape::read_for(block, &zones, me)) else { continue };
        let rule = shape.get("RRULE").map(|l| Rule::parse(&l.value));
        let extra: Vec<Stamp> = block.iter().filter(|l| l.name == "RDATE").flat_map(|l| stamps_of(l, &zones)).collect();
        if rule.is_none() && extra.is_empty() {
            out.extend(shape.instance(&uid, String::new(), shape.start, false));
            continue;
        }
        let excluded: HashSet<i64> = block
            .iter()
            .filter(|l| l.name == "EXDATE")
            .flat_map(|l| stamps_of(l, &zones))
            .filter_map(|s| s.in_zone_of(shape.start).timestamp())
            .collect();
        // Occurrences over before `from` are not built (a margin for a change of time).
        let after = from.saturating_sub(shape.length.max_secs() + 86400);
        // A rule not understood: only its first occurrence is shown.
        let mut starts = match rule {
            Some(Some(rule)) => rule.occurrences(shape.start, after, to),
            _ => vec![shape.start],
        };
        starts.extend(extra.into_iter().map(|s| s.in_zone_of(shape.start)));
        let mut seen = HashSet::new();
        for at in starts {
            let Some(ts) = at.timestamp() else { continue };
            if ts < after || ts >= to || excluded.contains(&ts) || overridden.contains(&(uid.clone(), ts)) || !seen.insert(ts) {
                continue;
            }
            out.extend(shape.instance(&uid, ts.to_string(), at, true));
        }
    }
    // A zero-length event at `from` still counts.
    out.retain(|i| i.start < to && (i.end > from || i.start >= from));
    out
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

/// The part of RRULE understood here: FREQ (daily to yearly), INTERVAL, COUNT, UNTIL, WKST,
/// BYDAY (with an ordinal in monthly and yearly-with-BYMONTH rules), BYMONTHDAY and BYMONTH.
/// Other parts (BYSETPOS, BYWEEKNO, BYYEARDAY, BYHOUR…) make the rule not understood.
#[derive(Debug, Clone, PartialEq)]
struct Rule {
    freq: Freq,
    interval: i64,
    count: Option<usize>,
    until: Option<Stamp>,
    wkst: chrono::Weekday,
    by_day: Vec<(Option<i32>, chrono::Weekday)>,
    by_month_day: Vec<i32>,
    by_month: Vec<u32>,
}

/// Safety bound on the periods walked through (the walk also stops at the end of the window).
const MAX_PERIODS: i64 = 200_000;
/// For a VTIMEZONE's change rule, computed for each zone an invitation carries: a real one
/// changes yearly from 1601 at the earliest, and a rule with COUNT is walked from its start.
const ZONE_PERIODS: i64 = 5_000;

fn weekday(code: &str) -> Option<chrono::Weekday> {
    use chrono::Weekday::*;
    Some(match code {
        "MO" => Mon,
        "TU" => Tue,
        "WE" => Wed,
        "TH" => Thu,
        "FR" => Fri,
        "SA" => Sat,
        "SU" => Sun,
        _ => return None,
    })
}

fn days_from(wd: chrono::Weekday, wkst: chrono::Weekday) -> i64 {
    i64::from((wd.num_days_from_monday() + 7 - wkst.num_days_from_monday()) % 7)
}

fn month_length(year: i32, month: u32) -> u32 {
    let next = if month == 12 { NaiveDate::from_ymd_opt(year + 1, 1, 1) } else { NaiveDate::from_ymd_opt(year, month + 1, 1) };
    next.and_then(|n| n.pred_opt()).map_or(28, |d| chrono::Datelike::day(&d))
}

/// The `n`th `wd` of a month; negative: counted from the end.
fn nth_weekday(year: i32, month: u32, wd: chrono::Weekday, n: i32) -> Option<NaiveDate> {
    use chrono::Datelike;
    if n > 0 {
        return NaiveDate::from_weekday_of_month_opt(year, month, wd, u8::try_from(n).ok()?);
    }
    let last = NaiveDate::from_ymd_opt(year, month, month_length(year, month))?;
    let back = (last.weekday().num_days_from_monday() + 7 - wd.num_days_from_monday()) % 7;
    let d = last.checked_sub_signed(chrono::Duration::try_days(i64::from(back) + 7 * (-i64::from(n) - 1))?)?;
    (d.month() == month).then_some(d)
}

impl Rule {
    fn parse(v: &str) -> Option<Rule> {
        let mut rule = Rule {
            freq: Freq::Daily,
            interval: 1,
            count: None,
            until: None,
            wkst: chrono::Weekday::Mon,
            by_day: vec![],
            by_month_day: vec![],
            by_month: vec![],
        };
        let mut freq = None;
        for part in v.trim().split(';').filter(|p| !p.is_empty()) {
            let (k, val) = part.split_once('=')?;
            let val = val.trim().to_ascii_uppercase();
            match k.trim().to_ascii_uppercase().as_str() {
                "FREQ" => {
                    freq = Some(match val.as_str() {
                        "DAILY" => Freq::Daily,
                        "WEEKLY" => Freq::Weekly,
                        "MONTHLY" => Freq::Monthly,
                        "YEARLY" => Freq::Yearly,
                        _ => return None,
                    })
                }
                "INTERVAL" => rule.interval = val.parse::<i64>().ok().filter(|n| *n > 0)?,
                "COUNT" => rule.count = Some(val.parse().ok()?),
                // Without `Z`: in DTSTART's zone (see `occurrences`).
                "UNTIL" => rule.until = Some(Stamp::parse(&val, Zone::Local, false)?),
                "WKST" => rule.wkst = weekday(&val)?,
                "BYDAY" => {
                    for d in val.split(',') {
                        let d = d.trim();
                        // Checked: a non-ASCII value would put the split inside a character.
                        let (ord, code) = d.split_at_checked(d.len().checked_sub(2)?)?;
                        let ord = if ord.is_empty() { None } else { Some(ord.parse::<i32>().ok().filter(|n| *n != 0 && n.abs() <= 53)?) };
                        rule.by_day.push((ord, weekday(code)?));
                    }
                }
                "BYMONTHDAY" => {
                    rule.by_month_day = val.split(',').map(|d| d.trim().parse::<i32>().ok().filter(|n| *n != 0 && n.abs() <= 31)).collect::<Option<_>>()?
                }
                "BYMONTH" => rule.by_month = val.split(',').map(|m| m.trim().parse::<u32>().ok().filter(|m| (1..=12).contains(m))).collect::<Option<_>>()?,
                _ => return None,
            }
        }
        rule.freq = freq?;
        // Ordinals only make sense within a month here.
        let ordinals = rule.by_day.iter().any(|(o, _)| o.is_some());
        if ordinals && !(rule.freq == Freq::Monthly || (rule.freq == Freq::Yearly && !rule.by_month.is_empty())) {
            return None;
        }
        Some(rule)
    }

    /// Days of one month matching BYMONTHDAY and BYDAY (`fallback`: the day of DTSTART).
    fn month_days(&self, year: i32, month: u32, fallback: u32) -> Vec<NaiveDate> {
        use chrono::Datelike;
        let len = month_length(year, month) as i32;
        let mut days: Vec<NaiveDate> = if !self.by_month_day.is_empty() {
            self.by_month_day
                .iter()
                .map(|&d| if d > 0 { d } else { len + d + 1 })
                .filter(|d| (1..=len).contains(d))
                .filter_map(|d| NaiveDate::from_ymd_opt(year, month, d as u32))
                .filter(|d| self.by_day.is_empty() || self.by_day.iter().any(|(_, wd)| *wd == d.weekday()))
                .collect()
        } else if !self.by_day.is_empty() {
            self.by_day
                .iter()
                .flat_map(|&(ord, wd)| match ord {
                    Some(n) => nth_weekday(year, month, wd, n).into_iter().collect::<Vec<_>>(),
                    None => (1..=5).filter_map(|n| nth_weekday(year, month, wd, n)).collect(),
                })
                .collect()
        } else {
            // A month without that day (the 31st, February 30th) is skipped.
            NaiveDate::from_ymd_opt(year, month, fallback).into_iter().collect()
        };
        days.sort();
        days.dedup();
        days
    }

    /// First day of the `k`th period after the one of `first` (the walk stops past the window).
    fn period_start(&self, first: NaiveDate, k: i64) -> Option<NaiveDate> {
        use chrono::Datelike;
        let step = k.checked_mul(self.interval)?;
        match self.freq {
            Freq::Daily => first.checked_add_signed(chrono::Duration::try_days(step)?),
            Freq::Weekly => {
                let week = first - chrono::Duration::days(days_from(first.weekday(), self.wkst));
                week.checked_add_signed(chrono::Duration::try_weeks(step)?)
            }
            Freq::Monthly => {
                let months = i64::from(first.year()) * 12 + i64::from(first.month0()) + step;
                NaiveDate::from_ymd_opt(i32::try_from(months.div_euclid(12)).ok()?, months.rem_euclid(12) as u32 + 1, 1)
            }
            Freq::Yearly => NaiveDate::from_ymd_opt(first.year().checked_add(i32::try_from(step).ok()?)?, 1, 1),
        }
    }

    /// Periods between the one of `first` and the one holding `day` (0 when before it).
    fn periods_until(&self, first: NaiveDate, day: NaiveDate) -> i64 {
        use chrono::Datelike;
        if day <= first {
            return 0;
        }
        let units = match self.freq {
            Freq::Daily => (day - first).num_days(),
            Freq::Weekly => (day - first).num_days() / 7,
            Freq::Monthly => (i64::from(day.year()) - i64::from(first.year())) * 12 + i64::from(day.month0()) - i64::from(first.month0()),
            Freq::Yearly => i64::from(day.year()) - i64::from(first.year()),
        };
        units / self.interval
    }

    fn period(&self, first: NaiveDate, k: i64) -> Vec<NaiveDate> {
        use chrono::Datelike;
        let Some(begin) = self.period_start(first, k) else { return vec![] };
        let in_months = |d: &NaiveDate| self.by_month.is_empty() || self.by_month.contains(&d.month());
        let mut days = match self.freq {
            Freq::Daily => {
                let d = begin;
                let ok = (self.by_day.is_empty() || self.by_day.iter().any(|(_, wd)| *wd == d.weekday()))
                    && (self.by_month_day.is_empty() || self.month_days(d.year(), d.month(), d.day()).contains(&d));
                if ok { vec![d] } else { vec![] }
            }
            Freq::Weekly => {
                let at = |wd: chrono::Weekday| begin + chrono::Duration::days(days_from(wd, self.wkst));
                if self.by_day.is_empty() { vec![at(first.weekday())] } else { self.by_day.iter().map(|(_, wd)| at(*wd)).collect() }
            }
            Freq::Monthly => self.month_days(begin.year(), begin.month(), first.day()),
            Freq::Yearly => {
                let year = begin.year();
                let plain = self.by_day.is_empty() && self.by_month_day.is_empty();
                // BYMONTHDAY or BYDAY without BYMONTH: in every month of the year.
                let months: Vec<u32> = match (self.by_month.is_empty(), plain) {
                    (false, _) => self.by_month.clone(),
                    (true, true) => vec![first.month()],
                    (true, false) => (1..=12).collect(),
                };
                months
                    .into_iter()
                    .flat_map(|m| if plain { NaiveDate::from_ymd_opt(year, m, first.day()).into_iter().collect() } else { self.month_days(year, m, first.day()) })
                    .collect()
            }
        };
        days.retain(in_months);
        days.sort();
        days.dedup();
        days
    }

    /// Starts of the occurrences of a series beginning at `start`, from `after` up to `to`
    /// (exclusive, Unix times), COUNT and UNTIL applied. DTSTART is always the first occurrence.
    /// Without COUNT the walk jumps to the periods near `after`; with it, the occurrences before
    /// `after` are counted but not returned.
    fn occurrences(&self, start: Stamp, after: i64, to: i64) -> Vec<Stamp> {
        self.occurrences_within(start, after, to, MAX_PERIODS)
    }

    /// [`occurrences`](Self::occurrences), walking at most `max_periods` periods.
    fn occurrences_within(&self, start: Stamp, after: i64, to: i64, max_periods: i64) -> Vec<Stamp> {
        let first = start.naive();
        let until = self.until.map(|u| u.in_zone_of(start));
        let limit = |s: Stamp| -> bool {
            match until {
                None => true,
                Some(Stamp::Date(d)) => s.naive().date() <= d,
                Some(until) => match (s.timestamp(), until.timestamp()) {
                    (Some(a), Some(b)) => a <= b,
                    _ => true,
                },
            }
        };
        let wanted = |s: Stamp| s.timestamp().is_some_and(|t| t >= after);
        // Wall dates, a day of margin for the zone.
        let date = |t: i64| chrono::DateTime::from_timestamp(t.clamp(-62_135_596_800, 253_402_300_799), 0).map(|d| d.date_naive());
        let Some(last_day) = date(to).and_then(|d| d.succ_opt()) else { return vec![] };
        let skip = match (self.count, date(after)) {
            (None, Some(day)) => (self.periods_until(first.date(), day - chrono::Duration::days(1)) - 1).max(0),
            _ => 0,
        };
        let mut found = 1;
        let mut out = if wanted(start) { vec![start] } else { vec![] };
        'periods: for k in skip..skip.saturating_add(max_periods) {
            if self.period_start(first.date(), k).is_none_or(|d| d > last_day) {
                break;
            }
            for day in self.period(first.date(), k) {
                let at = start.with(day.and_time(first.time()));
                if at.naive() <= first {
                    continue;
                }
                if self.count.is_some_and(|n| found >= n) || !limit(at) || at.timestamp().is_none_or(|t| t >= to) {
                    break 'periods;
                }
                found += 1;
                if wanted(at) {
                    out.push(at);
                }
            }
        }
        out
    }
}

/// Original VTIMEZONE blocks, copied so the reply's times stay understood.
fn timezones(ics: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current: Option<Vec<String>> = None;
    for line in unfold(ics) {
        let upper = line.to_ascii_uppercase();
        if upper == "BEGIN:VTIMEZONE" {
            current = Some(Vec::new());
        }
        if let Some(block) = current.as_mut() {
            block.push(line.clone());
        }
        if upper == "END:VTIMEZONE"
            && let Some(block) = current.take()
        {
            out.push(block.join("\r\n"));
        }
    }
    out
}

/// Folds at 75 octets (RFC 5545 §3.1), without splitting a character.
fn fold(line: &str) -> String {
    let mut out = String::new();
    let mut width = 0;
    for c in line.chars() {
        if width + c.len_utf8() > 75 {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(c);
        width += c.len_utf8();
    }
    out
}

fn escape(v: &str) -> String {
    v.replace('\\', "\\\\").replace(';', "\\;").replace(',', "\\,").replace('\n', "\\n")
}

/// `partstat`: `ACCEPTED`, `TENTATIVE` or `DECLINED`.
pub fn reply(ics: &str, me: &Address, partstat: &str, now: i64) -> Option<String> {
    let lines: Vec<Line> = unfold(ics).iter().filter_map(|l| parse_line(l)).collect();
    let event = event_lines(&lines)?;
    let copy = ["UID", "SEQUENCE", "RECURRENCE-ID", "DTSTART", "DTEND", "DURATION", "SUMMARY", "ORGANIZER"];
    let stamp = chrono::DateTime::from_timestamp(now, 0)?.format("%Y%m%dT%H%M%SZ");
    let cn = me.name.as_deref().filter(|n| !n.is_empty()).map(|n| format!(";CN=\"{}\"", n.replace('"', ""))).unwrap_or_default();
    let mut out = vec![
        "BEGIN:VCALENDAR".to_string(),
        "PRODID:-//facteur//FR".into(),
        "VERSION:2.0".into(),
        "METHOD:REPLY".into(),
    ];
    out.extend(timezones(ics));
    out.push("BEGIN:VEVENT".into());
    out.extend(event.iter().filter(|l| copy.contains(&l.name.as_str())).map(|l| l.raw.clone()));
    out.push(format!("DTSTAMP:{stamp}"));
    out.push(format!("ATTENDEE;PARTSTAT={partstat}{cn}:mailto:{}", escape(&me.email)));
    out.push("END:VEVENT".into());
    out.push("END:VCALENDAR".into());
    Some(out.iter().map(|l| if l.contains("\r\n") { l.clone() } else { fold(l) }).collect::<Vec<_>>().join("\r\n") + "\r\n")
}

// ── Events written by facteur ────────────────────────────────────────────

/// What an edit changes; `None`: left as it is.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EventChanges {
    pub summary: Option<String>,
    pub location: Option<Option<String>>,
    pub description: Option<Option<String>>,
    /// Start, end, whole day: as in [`crate::model::EventDraft`].
    pub times: Option<(i64, i64, bool)>,
    /// Who is invited (see [`EventChanges::organized_by`]); empty: nobody any more.
    pub attendees: Option<Vec<crate::model::EventAttendee>>,
}

/// Guests as given: valid addresses, each once, the organizer left out.
fn guests(list: &[crate::model::EventAttendee]) -> Vec<crate::model::EventAttendee> {
    let mut seen = HashSet::new();
    list.iter()
        .filter(|a| !a.organizer)
        .map(|a| crate::model::EventAttendee { email: a.email.trim().to_string(), name: clean(a.name.as_deref()), ..a.clone() })
        .filter(|a| a.email.contains('@') && !a.email.contains(char::is_whitespace) && seen.insert(a.email.to_lowercase()))
        .collect()
}

fn clean(v: Option<&str>) -> Option<String> {
    v.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

impl EventChanges {
    /// Everything a new event sets.
    pub fn all(draft: &crate::model::EventDraft) -> Self {
        EventChanges {
            summary: Some(draft.summary.trim().to_string()),
            location: Some(clean(draft.location.as_deref())),
            description: Some(clean(draft.description.as_deref())),
            times: Some((draft.start, draft.end, draft.all_day)),
            attendees: draft.attendees.as_ref().map(|list| guests(list)).filter(|list| !list.is_empty()),
        }
    }

    /// Only what differs: a provider may hold more than facteur shows (Outlook's description is a
    /// preview of the event's body), which an unchanged field must not overwrite.
    pub fn between(old: &crate::model::CalendarEvent, draft: &crate::model::EventDraft) -> Self {
        let summary = draft.summary.trim();
        let location = clean(draft.location.as_deref());
        let description = clean(draft.description.as_deref());
        let times = (draft.start, draft.end, draft.all_day);
        EventChanges {
            summary: (summary != old.summary.trim()).then(|| summary.to_string()),
            location: (location != clean(old.location.as_deref())).then_some(location),
            description: (description != clean(old.description.as_deref())).then_some(description),
            times: (times != (old.start, old.end, old.all_day)).then_some(times),
            // Guests invited already keep their answer.
            attendees: draft.attendees.as_ref().map(|list| {
                guests(list)
                    .into_iter()
                    .map(|mut a| {
                        if a.response.is_none() {
                            a.response = old.attendees.iter().find(|o| o.email.eq_ignore_ascii_case(&a.email)).and_then(|o| o.response.clone());
                        }
                        a
                    })
                    .collect::<Vec<_>>()
            }).filter(|new| {
                let emails = |list: &[crate::model::EventAttendee]| {
                    let mut e: Vec<String> = list.iter().filter(|a| !a.organizer).map(|a| a.email.to_lowercase()).collect();
                    e.sort();
                    e
                };
                emails(new) != emails(&old.attendees)
            }),
        }
    }

    /// Guests given, their organizer named first (the account), as written to the provider.
    pub fn organized_by(mut self, organizer: crate::model::EventAttendee) -> Self {
        if let Some(list) = self.attendees.as_mut().filter(|l| !l.is_empty()) {
            list.retain(|a| !a.organizer && !a.email.eq_ignore_ascii_case(&organizer.email));
            list.insert(0, crate::model::EventAttendee { organizer: true, response: Some("accepted".into()), ..organizer });
        }
        self
    }

    /// The guests written, the organizer left out.
    pub fn guests(&self) -> Vec<&crate::model::EventAttendee> {
        self.attendees.iter().flatten().filter(|a| !a.organizer).collect()
    }

    pub fn is_empty(&self) -> bool {
        *self == EventChanges::default()
    }
}

/// The day of a whole-day bound, given as the computer's local midnight.
pub fn local_date(t: i64) -> Option<NaiveDate> {
    chrono::DateTime::from_timestamp(t, 0).map(|d| d.with_timezone(&chrono::Local).date_naive())
}

/// First day and the day after the last of a whole-day event; at least one day.
pub fn days_of(start: i64, end: i64) -> Option<(NaiveDate, NaiveDate)> {
    let first = local_date(start)?;
    let after = local_date(end)?.max(first.succ_opt()?);
    Some((first, after))
}

fn utc_stamp(t: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(t, 0).map(|d| d.format("%Y%m%dT%H%M%SZ").to_string())
}

fn time_lines(start: i64, end: i64, all_day: bool) -> Option<[String; 2]> {
    if all_day {
        let (first, after) = days_of(start, end)?;
        return Some([format!("DTSTART;VALUE=DATE:{}", first.format("%Y%m%d")), format!("DTEND;VALUE=DATE:{}", after.format("%Y%m%d"))]);
    }
    Some([format!("DTSTART:{}", utc_stamp(start)?), format!("DTEND:{}", utc_stamp(end.max(start))?)])
}

fn text_line(name: &str, v: &str) -> String {
    format!("{name}:{}", escape(v))
}

fn serialize(lines: &[String]) -> String {
    lines.iter().map(|l| fold(l)).collect::<Vec<_>>().join("\r\n") + "\r\n"
}

/// A repeating timed series is written in the computer's zone (with its VTIMEZONE), so that its
/// dates keep their hour across changes of time; anything else in UTC or as days.
fn series_lines(start: i64, end: i64, all_day: bool, repeat: &crate::repeat::Repeat) -> Option<(Vec<String>, Vec<String>)> {
    if all_day {
        let (first, _) = days_of(start, end)?;
        let mut lines = time_lines(start, end, all_day)?.to_vec();
        lines.push(format!("RRULE:{}", repeat.rrule(first, None)));
        return Some((Vec::new(), lines));
    }
    let Some((name, tz)) = crate::repeat::local_zone() else {
        let first = local_date(start)?;
        let mut lines = time_lines(start, end, all_day)?.to_vec();
        let utc_end = |d: NaiveDate| resolve(&chrono::Local, d.and_hms_opt(23, 59, 59)?);
        lines.push(format!("RRULE:{}", repeat.rrule(first, Some(&utc_end))));
        return Some((Vec::new(), lines));
    };
    let wall = |t: i64| Zone::Named(tz).wall(t).map(|w| w.format("%Y%m%dT%H%M%S").to_string());
    let first = Zone::Named(tz).wall(start)?.date();
    let until = |d: NaiveDate| crate::repeat::end_of_day(tz, d);
    let tzid = if name.contains([';', ':', ',']) { format!("\"{name}\"") } else { name.clone() };
    let lines = vec![
        format!("DTSTART;TZID={tzid}:{}", wall(start)?),
        format!("DTEND;TZID={tzid}:{}", wall(end.max(start))?),
        format!("RRULE:{}", repeat.rrule(first, Some(&until))),
    ];
    Some((vtimezone(&name, tz), lines))
}

/// A VTIMEZONE for an IANA zone, with this year's changes of time as yearly rules (the last
/// Sunday of March…), as calendar servers expect one for each TZID.
fn vtimezone(name: &str, tz: chrono_tz::Tz) -> Vec<String> {
    use chrono::{Datelike, Offset};
    let offset = |t: i64| chrono::DateTime::from_timestamp(t, 0).map_or(0, |d| tz.offset_from_utc_datetime(&d.naive_utc()).fix().local_minus_utc());
    let hhmm = |o: i32| format!("{}{:02}{:02}", if o < 0 { '-' } else { '+' }, o.abs() / 3600, o.abs() % 3600 / 60);
    let year = chrono::Utc::now().year();
    let from = chrono::NaiveDate::from_ymd_opt(year, 1, 1).map_or(0, |d| d.and_time(chrono::NaiveTime::MIN).and_utc().timestamp());
    let mut out = vec!["BEGIN:VTIMEZONE".to_string(), format!("TZID:{name}")];
    let mut before = offset(from);
    let mut changes = 0;
    let mut t = from;
    while t < from + 366 * 86400 {
        t += 3600;
        let now = offset(t);
        if now == before {
            continue;
        }
        let Some(wall) = chrono::DateTime::from_timestamp(t + i64::from(before), 0).map(|d| d.naive_utc()) else { break };
        let day = wall.date();
        let last = day.checked_add_signed(chrono::Duration::days(7)).is_none_or(|d| d.month() != day.month());
        let week = if last { -1 } else { (day.day() as i32 - 1) / 7 + 1 };
        let code = ["MO", "TU", "WE", "TH", "FR", "SA", "SU"][day.weekday().num_days_from_monday() as usize];
        let kind = if now > before { "DAYLIGHT" } else { "STANDARD" };
        out.push(format!("BEGIN:{kind}"));
        out.push(format!("DTSTART:{}", wall.format("%Y%m%dT%H%M%S")));
        out.push(format!("RRULE:FREQ=YEARLY;BYMONTH={};BYDAY={week}{code}", day.month()));
        out.push(format!("TZOFFSETFROM:{}", hhmm(before)));
        out.push(format!("TZOFFSETTO:{}", hhmm(now)));
        out.push(format!("END:{kind}"));
        before = now;
        changes += 1;
    }
    if changes == 0 {
        out.extend(["BEGIN:STANDARD".to_string(), "DTSTART:19700101T000000".into(), format!("TZOFFSETFROM:{}", hhmm(before)), format!("TZOFFSETTO:{}", hhmm(before)), "END:STANDARD".into()]);
    }
    out.push("END:VTIMEZONE".into());
    out
}

/// A new event, in a calendar object of its own; repeating as `draft.repeat` says.
/// `organizer`: the account, named when the event has guests.
pub fn new_event(uid: &str, draft: &crate::model::EventDraft, organizer: Option<crate::model::EventAttendee>, now: i64) -> Option<String> {
    let changes = EventChanges::all(draft);
    let changes = match organizer {
        Some(o) => changes.organized_by(o),
        None => changes,
    };
    let (start, end, all_day) = changes.times?;
    let (zone, times) = match &draft.repeat {
        Some(repeat) => series_lines(start, end, all_day, repeat)?,
        None => (Vec::new(), time_lines(start, end, all_day)?.to_vec()),
    };
    let mut out = vec!["BEGIN:VCALENDAR".to_string(), "VERSION:2.0".into(), "PRODID:-//facteur//EN".into()];
    out.extend(zone);
    out.extend([
        "BEGIN:VEVENT".into(),
        format!("UID:{uid}"),
        format!("DTSTAMP:{}", utc_stamp(now)?),
        format!("CREATED:{}", utc_stamp(now)?),
    ]);
    out.extend(times);
    out.push(text_line("SUMMARY", changes.summary.as_deref().unwrap_or_default()));
    out.extend(changes.location.flatten().map(|l| text_line("LOCATION", &l)));
    out.extend(changes.description.flatten().map(|d| text_line("DESCRIPTION", &d)));
    out.extend(attendee_lines(changes.attendees.as_deref().unwrap_or_default()));
    out.push("END:VEVENT".into());
    out.push("END:VCALENDAR".into());
    Some(serialize(&out))
}

/// A part of a calendar object: a VEVENT block (from its BEGIN to its END, alarms included) or
/// a line around them, kept as it is. Lines are unfolded.
enum Part {
    Line(String),
    Event(Vec<String>),
}

fn split(ics: &str) -> Vec<Part> {
    let mut out = Vec::new();
    let mut current: Option<Vec<String>> = None;
    let mut depth = 0;
    for raw in unfold(ics) {
        let (name, value) = parse_line(&raw).map(|l| (l.name, l.value.trim().to_ascii_uppercase())).unwrap_or_default();
        if let Some(mut block) = current.take() {
            block.push(raw);
            match name.as_str() {
                "BEGIN" => depth += 1,
                "END" if depth > 0 => depth -= 1,
                "END" => {
                    out.push(Part::Event(block));
                    continue;
                }
                _ => {}
            }
            current = Some(block);
        } else if name == "BEGIN" && value == "VEVENT" {
            current = Some(vec![raw]);
            depth = 0;
        } else {
            out.push(Part::Line(raw));
        }
    }
    out.extend(current.map(Part::Event));
    out
}

fn join(parts: &[Part]) -> String {
    let lines: Vec<String> = parts
        .iter()
        .flat_map(|p| match p {
            Part::Line(l) => vec![l.clone()],
            Part::Event(b) => b.clone(),
        })
        .collect();
    serialize(&lines)
}

/// The block's own properties (not its alarms'), with their index in it.
fn own_props(block: &[String]) -> Vec<(usize, Line)> {
    let mut out = Vec::new();
    let mut depth = 0;
    for (i, raw) in block.iter().enumerate().skip(1) {
        let Some(l) = parse_line(raw) else { continue };
        match l.name.as_str() {
            "BEGIN" => depth += 1,
            "END" => depth -= 1,
            _ if depth == 0 => out.push((i, l)),
            _ => {}
        }
    }
    out
}

fn prop(block: &[String], name: &str) -> Option<Line> {
    own_props(block).into_iter().map(|(_, l)| l).find(|l| l.name == name)
}

/// What comes before the value: the name and the parameters, as written.
fn head(l: &Line) -> &str {
    &l.raw[..l.raw.len() - l.value.len() - 1]
}

/// The parameters of a property, as written (`;TZID=Europe/Paris`).
fn params_of(l: &Line) -> &str {
    &head(l)[l.name.len()..]
}

fn quote_param(v: &str) -> String {
    if v.contains([':', ';', ',']) { format!("\"{}\"", v.replace('"', "")) } else { v.to_string() }
}

/// `block` without its own properties named in `drop`, with `add` at its end and the change
/// marks renewed (DTSTAMP, LAST-MODIFIED, SEQUENCE one higher).
fn rework(block: &[String], drop: &[&str], add: Vec<String>, now: i64) -> Option<Vec<String>> {
    let props = own_props(block);
    let sequence: i64 = props.iter().find(|(_, l)| l.name == "SEQUENCE").and_then(|(_, l)| l.value.trim().parse().ok()).unwrap_or(0);
    let marks = ["DTSTAMP", "LAST-MODIFIED", "SEQUENCE"];
    let gone: HashSet<usize> = props.iter().filter(|(_, l)| drop.contains(&l.name.as_str()) || marks.contains(&l.name.as_str())).map(|(i, _)| *i).collect();
    let mut out: Vec<String> = block.iter().enumerate().filter(|(i, _)| !gone.contains(i)).map(|(_, l)| l.clone()).collect();
    if out.last().is_some_and(|l| l.eq_ignore_ascii_case("END:VEVENT")) {
        out.pop();
    }
    out.push(format!("DTSTAMP:{}", utc_stamp(now)?));
    out.push(format!("LAST-MODIFIED:{}", utc_stamp(now)?));
    out.push(format!("SEQUENCE:{}", sequence + 1));
    out.extend(add);
    out.push("END:VEVENT".into());
    Some(out)
}

/// The text properties `changes` sets on `block`: those to drop, and their new lines.
fn text_changes(changes: &EventChanges, block: &[String]) -> (Vec<&'static str>, Vec<String>) {
    let mut drop = Vec::new();
    let mut add = Vec::new();
    if let Some(s) = &changes.summary {
        drop.push("SUMMARY");
        add.push(text_line("SUMMARY", s));
    }
    if let Some(l) = &changes.location {
        drop.push("LOCATION");
        add.extend(l.as_deref().map(|l| text_line("LOCATION", l)));
    }
    if let Some(d) = &changes.description {
        drop.push("DESCRIPTION");
        add.extend(d.as_deref().map(|d| text_line("DESCRIPTION", d)));
    }
    if let Some(list) = &changes.attendees {
        drop.extend(["ORGANIZER", "ATTENDEE"]);
        add.extend(merged_attendees(block, list));
    }
    (drop, add)
}

/// The guest list `list` asks for, merged into the one the server holds in `block`: a guest who
/// stays keeps their line as it is (their latest answer, role, delegation), rooms and resources
/// (never shown, never edited) stay, the removed go and the new ones are added.
fn merged_attendees(block: &[String], list: &[crate::model::EventAttendee]) -> Vec<String> {
    let wanted: HashSet<String> = list.iter().filter(|a| !a.organizer).map(|a| a.email.to_lowercase()).collect();
    let mut organizer = None;
    let mut kept = Vec::new();
    let mut present = HashSet::new();
    for (i, l) in own_props(block) {
        match l.name.as_str() {
            "ORGANIZER" => organizer = Some(block[i].clone()),
            "ATTENDEE" => {
                let email = mailto(&l.value).to_lowercase();
                let resource = l.param("CUTYPE").is_some_and(|t| t.eq_ignore_ascii_case("ROOM") || t.eq_ignore_ascii_case("RESOURCE"));
                if resource || wanted.contains(&email) {
                    present.insert(email);
                    kept.push(block[i].clone());
                }
            }
            _ => {}
        }
    }
    let new: Vec<crate::model::EventAttendee> = list.iter().filter(|a| !a.organizer && !present.contains(&a.email.to_lowercase())).cloned().collect();
    if kept.is_empty() && new.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = match organizer {
        Some(line) => vec![line],
        None => attendee_lines(list).into_iter().filter(|l| l.starts_with("ORGANIZER")).collect(),
    };
    out.extend(kept);
    out.extend(attendee_lines(&new));
    out
}

/// ORGANIZER and ATTENDEE lines; none without guests.
fn attendee_lines(list: &[crate::model::EventAttendee]) -> Vec<String> {
    if !list.iter().any(|a| !a.organizer) {
        return Vec::new();
    }
    let cn = |a: &crate::model::EventAttendee| a.name.as_deref().map(|n| format!(";CN={}", quote_param(n))).unwrap_or_default();
    list.iter()
        .map(|a| {
            if a.organizer {
                return format!("ORGANIZER{}:mailto:{}", cn(a), a.email);
            }
            let partstat = match a.response.as_deref() {
                Some("accepted") => "ACCEPTED",
                Some("tentative") => "TENTATIVE",
                Some("declined") => "DECLINED",
                _ => "NEEDS-ACTION",
            };
            format!("ATTENDEE{};ROLE=REQ-PARTICIPANT;PARTSTAT={partstat};RSVP=TRUE:mailto:{}", cn(a), a.email)
        })
        .collect()
}

/// The calendar object as an iTIP message (RFC 5546) for guests: `REQUEST` (an invitation, or
/// its update) or `CANCEL` (the event, or their place in it, withdrawn).
pub fn invitation(ics: &str, method: &str, now: i64) -> Option<String> {
    let mut parts = split(ics);
    parts.retain(|p| !matches!(p, Part::Line(l) if l.to_ascii_uppercase().starts_with("METHOD:")));
    let at = parts.iter().position(|p| matches!(p, Part::Line(l) if l.eq_ignore_ascii_case("BEGIN:VCALENDAR")))?;
    parts.insert(at + 1, Part::Line(format!("METHOD:{method}")));
    if method == "CANCEL" {
        for part in parts.iter_mut() {
            if let Part::Event(block) = part {
                *block = rework(block, &["STATUS"], vec!["STATUS:CANCELLED".into()], now)?;
            }
        }
    }
    Some(join(&parts))
}

const TIMES: [&str; 3] = ["DTSTART", "DTEND", "DURATION"];

fn is_series(block: &[String]) -> bool {
    own_props(block).iter().any(|(_, l)| matches!(l.name.as_str(), "RRULE" | "RDATE"))
}

fn is_override(block: &[String]) -> bool {
    prop(block, "RECURRENCE-ID").is_some()
}

/// The calendar object of a single event with `changes` applied; everything else (alarms,
/// attendees, properties facteur does not know) is kept. `None` for a series or several events
/// (see [`edit_series`] and [`edit_occurrence`]).
pub fn edit_event(ics: &str, changes: &EventChanges, now: i64) -> Option<String> {
    let mut parts = split(ics);
    let mut events = parts.iter_mut().filter_map(|p| match p {
        Part::Event(b) => Some(b),
        Part::Line(_) => None,
    });
    let block = events.next()?;
    if events.next().is_some() || is_series(block) || is_override(block) {
        return None;
    }
    let (mut drop, mut add) = text_changes(changes, block);
    if let Some((start, end, all_day)) = changes.times {
        drop.extend(TIMES);
        add.extend(time_lines(start, end, all_day)?);
    }
    *block = rework(block, &drop, add, now)?;
    Some(join(&parts))
}

/// The wall time of an instant in a zone named by IANA or Windows (Outlook); `None`: unknown.
pub(crate) fn wall_in(tzid: &str, t: i64) -> Option<NaiveDateTime> {
    Zone::Named(named_zone(tzid)?).wall(t)
}

/// A DATE, floating, zoned or UTC value moved by `secs` of wall time (whole days for a date).
fn shift_value(v: &str, secs: i64) -> Option<String> {
    let v = v.trim();
    if v.len() == 8 {
        let d = NaiveDate::parse_from_str(v, "%Y%m%d").ok()?;
        let days = (secs as f64 / 86400.0).round() as i64;
        return Some(d.checked_add_signed(chrono::Duration::try_days(days)?)?.format("%Y%m%d").to_string());
    }
    let (body, utc) = match v.strip_suffix(['Z', 'z']) {
        Some(b) => (b, "Z"),
        None => (v, ""),
    };
    let t = NaiveDateTime::parse_from_str(body, "%Y%m%dT%H%M%S").ok()?.checked_add_signed(chrono::Duration::try_seconds(secs)?)?;
    Some(format!("{}{utc}", t.format("%Y%m%dT%H%M%S")))
}

/// The same property moved by `secs` (every value of a list).
fn shift_line(l: &Line, secs: i64) -> Option<String> {
    let values: Option<Vec<String>> = l.value.split(',').map(|v| shift_value(v, secs)).collect();
    Some(format!("{}:{}", head(l), values?.join(",")))
}

/// RRULE with its UNTIL moved along.
fn shift_rule(l: &Line, secs: i64) -> Option<String> {
    let parts: Option<Vec<String>> = l
        .value
        .split(';')
        .map(|p| match p.split_once('=') {
            Some((k, v)) if k.eq_ignore_ascii_case("UNTIL") => shift_value(v, secs).map(|v| format!("{k}={v}")),
            _ => Some(p.to_string()),
        })
        .collect();
    Some(format!("{}:{}", head(l), parts?.join(";")))
}

/// Context to read the instants of one calendar object: its zones and its series' first start.
struct Series {
    zones: Zones,
    start: Option<Stamp>,
}

impl Series {
    fn read(parts: &[Part]) -> Series {
        let lines: Vec<Line> = parts
            .iter()
            .flat_map(|p| match p {
                Part::Line(l) => vec![l.clone()],
                Part::Event(b) => b.clone(),
            })
            .filter_map(|l| parse_line(&l))
            .collect();
        let zones = Zones::read(&lines);
        let start = parts.iter().find_map(|p| match p {
            Part::Event(b) if !is_override(b) => prop(b, "DTSTART").and_then(|l| stamp_of(&l, &zones)),
            _ => None,
        });
        Series { zones, start }
    }

    /// The instant a RECURRENCE-ID or EXDATE stands for (floating values in the series' zone).
    fn instant(&self, l: &Line) -> Option<i64> {
        let s = stamp_of(l, &self.zones)?;
        self.start.map_or(s, |start| s.in_zone_of(start)).timestamp()
    }

    /// A property naming the occurrence that started at `original`, written as the series'
    /// DTSTART is (a date, a zoned or a UTC time).
    fn naming(&self, name: &str, master: &[String], original: i64) -> Option<String> {
        let start = prop(master, "DTSTART")?;
        let params = params_of(&start);
        let value = start.value.trim();
        let at = chrono::DateTime::from_timestamp(original, 0)?;
        if value.len() == 8 {
            return Some(format!("{name}{params}:{}", at.date_naive().format("%Y%m%d")));
        }
        if value.ends_with(['Z', 'z']) {
            return Some(format!("{name}{params}:{}", at.format("%Y%m%dT%H%M%SZ")));
        }
        let wall = self.zones.of(start.param("TZID")).wall(original)?;
        Some(format!("{name}{params}:{}", wall.format("%Y%m%dT%H%M%S")))
    }
}

impl Zone {
    /// The wall time of an instant in this zone.
    fn wall(self, t: i64) -> Option<NaiveDateTime> {
        let utc = chrono::DateTime::from_timestamp(t, 0)?;
        Some(match self {
            Zone::Utc => utc.naive_utc(),
            Zone::Named(tz) => utc.with_timezone(&tz).naive_local(),
            Zone::Fixed(offset) => utc.with_timezone(&offset).naive_local(),
            Zone::Local => utc.with_timezone(&chrono::Local).naive_local(),
        })
    }
}

/// The series with `changes` applied to all of it. Its dates stay (the caller refuses a change
/// of day, which the repetition rule would not follow): every date takes the time of day of the
/// new start, in the series' own zone, and the new length. The result does not depend on the
/// series' current time, so a change sent twice gives the same series. Occurrences changed alone
/// move along, and keep the texts they were given. `None`: not a series, or the change turns it
/// into whole days or back (left to the calendar's own app).
pub fn edit_series(ics: &str, changes: &EventChanges, current: (i64, i64, bool), now: i64) -> Option<String> {
    let mut parts = split(ics);
    let series = Series::read(&parts);
    let master_index = parts.iter().position(|p| matches!(p, Part::Event(b) if is_series(b) && !is_override(b)))?;
    let master = match &parts[master_index] {
        Part::Event(b) => b.clone(),
        Part::Line(_) => return None,
    };
    // Seconds of wall time the series' first date moves by, and its new length.
    let (delta, duration) = match changes.times {
        Some((start, end, all_day)) if all_day == current.2 => {
            let first = prop(&master, "DTSTART")?;
            let value = first.value.trim();
            if value.len() == 8 {
                (0, end - start)
            } else {
                let zone = if value.ends_with(['Z', 'z']) { Zone::Utc } else { series.zones.of(first.param("TZID")) };
                let old = NaiveDateTime::parse_from_str(value.trim_end_matches(['Z', 'z']), "%Y%m%dT%H%M%S").ok()?;
                let new = old.date().and_time(zone.wall(start)?.time());
                ((new - old).num_seconds(), (end - start).max(0))
            }
        }
        Some(_) => return None,
        None => (0, 0),
    };
    let old_text = |b: &[String], name: &str| prop(b, name).map(|l| unescape(&l.value).trim().to_string()).filter(|s| !s.is_empty());
    for part in parts.iter_mut() {
        let Part::Event(block) = part else { continue };
        let master_block = !is_override(block);
        let (mut drop, mut add) = (Vec::new(), Vec::new());
        // An occurrence keeps a text it was given apart from the series.
        for (name, new) in [("SUMMARY", changes.summary.clone().map(Some)), ("LOCATION", changes.location.clone()), ("DESCRIPTION", changes.description.clone())] {
            let Some(new) = new else { continue };
            if master_block || old_text(block, name) == old_text(&master, name) {
                drop.push(name);
                add.extend(new.map(|v| text_line(name, &v)));
            }
        }
        if changes.times.is_some() && delta != 0 || (master_block && changes.times.is_some()) {
            let props = own_props(block);
            for (_, l) in &props {
                match l.name.as_str() {
                    "DTSTART" if master_block => {
                        let start = shift_value(&l.value, delta)?;
                        let length = if start.len() == 8 { duration.max(86400) } else { duration };
                        add.push(format!("{}:{start}", head(l)));
                        add.push(format!("DTEND{}:{}", params_of(l), shift_value(&start, length)?));
                    }
                    "DTEND" | "DURATION" if master_block => continue,
                    "DTSTART" | "DTEND" | "RECURRENCE-ID" | "EXDATE" | "RDATE" => add.push(shift_line(l, delta)?),
                    "RRULE" => add.push(shift_rule(l, delta)?),
                    _ => continue,
                }
                drop.push(match l.name.as_str() {
                    "DTSTART" => "DTSTART",
                    "DTEND" => "DTEND",
                    "RECURRENCE-ID" => "RECURRENCE-ID",
                    "EXDATE" => "EXDATE",
                    "RDATE" => "RDATE",
                    _ => "RRULE",
                });
            }
            if master_block {
                drop.extend(["DTEND", "DURATION"]);
            }
        }
        if drop.is_empty() {
            continue;
        }
        *block = rework(block, &drop, add, now)?;
    }
    Some(join(&parts))
}

/// The new first occurrence of a series rebuilt from the occurrence the user changed: its first
/// day moved by as many days as that occurrence moved, at the occurrence's new time of day and
/// length (the computer's zone); for whole days, as many days long. Returns start, end (as
/// [`EventChanges`] times) and the first day.
pub fn series_first(master_first: NaiveDate, occurrence_start: i64, draft: &crate::model::EventDraft) -> Option<(i64, i64, NaiveDate)> {
    let shift = (local_date(draft.start)? - local_date(occurrence_start)?).num_days();
    let first = master_first.checked_add_signed(chrono::Duration::try_days(shift)?)?;
    if draft.all_day {
        let days = (((draft.end - draft.start) as f64 / 86400.0).round() as i64).max(1);
        let start = local_midnight(first)?;
        let end = local_midnight(first.checked_add_signed(chrono::Duration::try_days(days)?)?)?;
        return Some((start, end, first));
    }
    let time = chrono::DateTime::from_timestamp(draft.start, 0)?.with_timezone(&chrono::Local).time();
    let start = resolve(&chrono::Local, first.and_time(time))?;
    Some((start, start + (draft.end - draft.start).max(0), first))
}

/// The series' rule (RRULE value, `None` when it does not repeat) and its first day, here.
pub fn series_rule(ics: &str) -> Option<(Option<String>, NaiveDate)> {
    let parts = split(ics);
    let series = Series::read(&parts);
    let master = parts.iter().find_map(|p| match p {
        Part::Event(b) if !is_override(b) => Some(b),
        _ => None,
    })?;
    let first = local_date(series.start?.timestamp()?)?;
    let first = match series.start? {
        Stamp::Date(d) => d,
        _ => first,
    };
    Some((prop(master, "RRULE").map(|l| l.value.trim().to_string()), first))
}

/// The series rebuilt: texts as `changes` says, its first occurrence at `first` (start, end,
/// whole day, see [`series_first`]) repeating as `repeat` (`None`: a single event from then on).
/// The dates changed or deleted alone are dropped: they named the old schedule's dates.
pub fn rebuild_series(ics: &str, changes: &EventChanges, first: (i64, i64, bool), repeat: Option<&crate::repeat::Repeat>, now: i64) -> Option<String> {
    let mut parts = split(ics);
    parts.retain(|p| !matches!(p, Part::Event(b) if is_override(b)));
    let (start, end, all_day) = first;
    let (zone, times) = match repeat {
        Some(repeat) => series_lines(start, end, all_day, repeat)?,
        None => (Vec::new(), time_lines(start, end, all_day)?.to_vec()),
    };
    let index = parts.iter().position(|p| matches!(p, Part::Event(_)))?;
    let Part::Event(master) = &parts[index] else { return None };
    let (mut drop, mut add) = text_changes(changes, master);
    drop.extend(["DTSTART", "DTEND", "DURATION", "RRULE", "RDATE", "EXDATE", "EXRULE"]);
    add.extend(times);
    let master = rework(master, &drop, add, now)?;
    parts[index] = Part::Event(master);
    // The zone written, unless the object has it already.
    if let Some(tzid) = zone.get(1).and_then(|l| l.strip_prefix("TZID:")) {
        let known = parts.iter().any(|p| matches!(p, Part::Line(l) if l.trim().eq_ignore_ascii_case(&format!("TZID:{tzid}"))));
        if !known {
            for (i, line) in zone.into_iter().enumerate() {
                parts.insert(index + i, Part::Line(line));
            }
        }
    }
    Some(join(&parts))
}

/// The block standing for the occurrence that started at `original`: its own when it was changed
/// alone already, else a new one copied from the series, without times yet (`true`).
fn occurrence_block(parts: &mut Vec<Part>, series: &Series, original: i64) -> Option<(usize, bool)> {
    let found = parts.iter().position(|p| match p {
        Part::Event(b) => prop(b, "RECURRENCE-ID").is_some_and(|l| series.instant(&l) == Some(original)),
        Part::Line(_) => false,
    });
    if let Some(index) = found {
        return Some((index, false));
    }
    let master_index = parts.iter().position(|p| matches!(p, Part::Event(b) if is_series(b) && !is_override(b)))?;
    let Part::Event(master) = &parts[master_index] else { return None };
    let skipped: HashSet<usize> = own_props(master)
        .into_iter()
        .filter(|(_, l)| matches!(l.name.as_str(), "RRULE" | "RDATE" | "EXDATE" | "EXRULE" | "RECURRENCE-ID" | "DTSTART" | "DTEND" | "DURATION"))
        .map(|(i, _)| i)
        .collect();
    let mut block: Vec<String> = master.iter().enumerate().filter(|(i, _)| !skipped.contains(i)).map(|(_, l)| l.clone()).collect();
    let end = block.pop()?;
    block.push(series.naming("RECURRENCE-ID", master, original)?);
    block.push(end);
    parts.insert(master_index + 1, Part::Event(block));
    Some((master_index + 1, true))
}

/// The occurrence of a series that started at `original` changed alone; `current`: its start,
/// end and whole day now.
pub fn edit_occurrence(ics: &str, original: i64, current: (i64, i64, bool), changes: &EventChanges, now: i64) -> Option<String> {
    let mut parts = split(ics);
    let series = Series::read(&parts);
    let (index, _) = occurrence_block(&mut parts, &series, original)?;
    let Part::Event(block) = &parts[index] else { return None };
    let (mut drop, mut add) = text_changes(changes, block);
    let (start, end, all_day) = changes.times.unwrap_or(current);
    drop.extend(TIMES);
    add.extend(time_lines(start, end, all_day)?);
    let reworked = rework(block, &drop, add, now)?;
    parts[index] = Part::Event(reworked);
    Some(join(&parts))
}

/// The occurrence that started at `original` taken out of its series.
pub fn delete_occurrence(ics: &str, original: i64, now: i64) -> Option<String> {
    let mut parts = split(ics);
    let series = Series::read(&parts);
    parts.retain(|p| match p {
        Part::Event(b) => prop(b, "RECURRENCE-ID").is_none_or(|l| series.instant(&l) != Some(original)),
        Part::Line(_) => true,
    });
    let master = parts.iter_mut().find_map(|p| match p {
        Part::Event(b) if is_series(b) && !is_override(b) => Some(b),
        _ => None,
    })?;
    let exdate = series.naming("EXDATE", master, original)?;
    *master = rework(master, &[], vec![exdate], now)?;
    Some(join(&parts))
}

/// The calendar object reduced to the occurrence that started at `original` (its own block), for
/// a reply about that date alone.
pub fn occurrence_ics(ics: &str, original: i64) -> Option<String> {
    let parts = split(ics);
    let series = Series::read(&parts);
    let keep = |p: &Part| match p {
        Part::Event(b) => prop(b, "RECURRENCE-ID").is_some_and(|l| series.instant(&l) == Some(original)),
        Part::Line(_) => true,
    };
    let kept: Vec<Part> = parts.into_iter().filter(keep).collect();
    kept.iter().any(|p| matches!(p, Part::Event(_))).then(|| join(&kept))
}

/// The account's answer to an invitation (`partstat`: ACCEPTED, TENTATIVE or DECLINED) on its
/// own attendee lines (`me`: its addresses): of the whole object, or of the occurrence that
/// started at `original` (with `current`, its times now). The server tells the organizer
/// (CalDAV scheduling, RFC 6638). `None`: the account is not invited.
pub fn respond_event(ics: &str, me: &[String], partstat: &str, original: Option<(i64, (i64, i64, bool))>, now: i64) -> Option<String> {
    let mut parts = split(ics);
    let only = match original {
        Some((original, current)) => {
            let series = Series::read(&parts);
            let (index, new) = occurrence_block(&mut parts, &series, original)?;
            if new && let Part::Event(b) = &parts[index] {
                let block = rework(b, &[], time_lines(current.0, current.1, current.2)?.to_vec(), now)?;
                parts[index] = Part::Event(block);
            }
            Some(index)
        }
        None => None,
    };
    let mut answered = false;
    for (i, part) in parts.iter_mut().enumerate() {
        let Part::Event(block) = part else { continue };
        if only.is_some_and(|o| o != i) {
            continue;
        }
        for (index, l) in own_props(block) {
            if l.name != "ATTENDEE" || !me.iter().any(|m| m.eq_ignore_ascii_case(&mailto(&l.value))) {
                continue;
            }
            let params: String = l
                .params
                .iter()
                .filter(|(k, _)| k != "PARTSTAT" && k != "RSVP")
                .map(|(k, v)| format!(";{k}={}", quote_param(v)))
                .collect();
            block[index] = format!("ATTENDEE{params};PARTSTAT={partstat}:{}", l.value);
            answered = true;
        }
        if let Some((index, _)) = own_props(block).into_iter().find(|(_, l)| l.name == "DTSTAMP") {
            block[index] = format!("DTSTAMP:{}", utc_stamp(now)?);
        }
    }
    answered.then(|| join(&parts))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flood_of_crafted_time_zones_is_read_quickly() {
        let mut ics = String::from("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REQUEST\r\n");
        for i in 0..1000 {
            ics.push_str(&format!(
                "BEGIN:VTIMEZONE\r\nTZID:x{i}\r\nBEGIN:STANDARD\r\nDTSTART:00010101T000000\r\nRRULE:FREQ=DAILY;COUNT=999999999\r\n\
TZOFFSETFROM:+0100\r\nTZOFFSETTO:+0100\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\n"
            ));
        }
        ics.push_str("BEGIN:VEVENT\r\nUID:u\r\nDTSTART;TZID=x1:20261110T090000\r\nDTEND;TZID=x1:20261110T100000\r\nSUMMARY:x\r\n\
ORGANIZER:mailto:a@example.org\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n");
        let started = std::time::Instant::now();
        let _ = parse(&ics);
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "took {:?}", started.elapsed());
    }

    #[test]
    fn editing_the_guests_keeps_rooms_and_answers_from_the_server() {
        let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:u1\r\nDTSTART:20261110T090000Z\r\nDTEND:20261110T100000Z\r\n\
ORGANIZER;CN=Me:mailto:me@example.org\r\n\
ATTENDEE;ROLE=OPT-PARTICIPANT;PARTSTAT=ACCEPTED;CN=Ann:mailto:ann@example.org\r\n\
ATTENDEE;CUTYPE=ROOM;PARTSTAT=ACCEPTED;CN=Room 1:mailto:room1@example.org\r\n\
ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:bob@example.org\r\nSUMMARY:Meeting\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let guest = |email: &str, response: Option<&str>| crate::model::EventAttendee { email: email.into(), name: None, response: response.map(Into::into), organizer: false };
        // Bob removed, Carl added; the cached answer of Ann is stale (none), the server's is kept.
        let changes = EventChanges { attendees: Some(vec![guest("ann@example.org", None), guest("carl@example.org", None)]), ..Default::default() };
        let out = edit_event(ics, &changes, 1_800_000_000).unwrap().replace("\r\n ", "");
        assert!(out.contains("ORGANIZER;CN=Me:mailto:me@example.org"), "{out}");
        assert!(out.contains("ATTENDEE;ROLE=OPT-PARTICIPANT;PARTSTAT=ACCEPTED;CN=Ann:mailto:ann@example.org"), "Ann's line as it was: {out}");
        assert!(out.contains("CUTYPE=ROOM"), "the room stays: {out}");
        assert!(!out.contains("bob@example.org"), "{out}");
        assert!(out.contains("PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:carl@example.org"), "{out}");
        // Everyone removed but the room: it stays, with the organizer.
        let out = edit_event(ics, &EventChanges { attendees: Some(vec![]), ..Default::default() }, 1_800_000_000).unwrap().replace("\r\n ", "");
        assert!(out.contains("CUTYPE=ROOM") && out.contains("ORGANIZER") && !out.contains("ann@example.org"), "{out}");
    }

    const INVITE: &str = "BEGIN:VCALENDAR\r\nPRODID:-//Google Inc//Google Calendar 70.9054//EN\r\nVERSION:2.0\r\nMETHOD:REQUEST\r\n\
BEGIN:VTIMEZONE\r\nTZID:Europe/Paris\r\nBEGIN:STANDARD\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\nDTSTART:19701025T030000\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\n\
BEGIN:VEVENT\r\nDTSTART;TZID=Europe/Paris:20261020T140000\r\nDTEND;TZID=Europe/Paris:20261020T150000\r\nDTSTAMP:20261015T090000Z\r\n\
ORGANIZER;CN=Marie Dupont:mailto:marie@example.org\r\nUID:abc123@google.com\r\n\
ATTENDEE;CUTYPE=INDIVIDUAL;ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;CN=y@example.org:mailto:y@example.org\r\n\
SEQUENCE:2\r\nSUMMARY:Point projet\\, facteur\r\nLOCATION:Salle 3\r\nDESCRIPTION:Ordre du jour :\\n- avancement\r\n \\n- suite\r\n\
BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-P0DT0H10M0S\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    #[test]
    fn reads_an_invitation() {
        let i = parse(INVITE).unwrap();
        assert_eq!((i.method.as_str(), i.uid.as_str(), i.summary.as_str()), ("REQUEST", "abc123@google.com", "Point projet, facteur"));
        assert_eq!(i.location.as_deref(), Some("Salle 3"));
        assert_eq!(i.description.as_deref(), Some("Ordre du jour :\n- avancement\n- suite"));
        assert_eq!(i.organizer.as_ref().map(|o| (o.name.as_deref(), o.email.as_str())), Some((Some("Marie Dupont"), "marie@example.org")));
        assert_eq!(i.attendees[0].partstat.as_deref(), Some("NEEDS-ACTION"));
        assert_eq!(i.end.unwrap() - i.start.unwrap(), 3600);
        assert_eq!(i.tzid.as_deref(), Some("Europe/Paris"));
        assert!(!i.cancelled && !i.recurring && !i.all_day);

        let day = parse("BEGIN:VCALENDAR\nMETHOD:CANCEL\nBEGIN:VEVENT\nUID:x\nDTSTART;VALUE=DATE:20261224\nEND:VEVENT\nEND:VCALENDAR").unwrap();
        assert!(day.all_day && day.cancelled);
    }

    #[test]
    fn writes_a_new_event() {
        let draft = crate::model::EventDraft {
            summary: " Point, projet ".into(),
            location: Some("Salle 3".into()),
            description: Some("Ordre du jour :\n- suite".into()),
            start: 1_792_497_600,
            end: 1_792_501_200,
            all_day: false,
            repeat: None,
            attendees: None,
        };
        let ics = new_event("u1@facteur", &draft, None, 1_792_400_000).unwrap();
        assert!(ics.contains("UID:u1@facteur\r\n") && ics.contains("DTSTART:20261020T120000Z\r\n") && ics.contains("DTEND:20261020T130000Z\r\n"));
        assert!(ics.contains("SUMMARY:Point\\, projet\r\n") && ics.contains("DESCRIPTION:Ordre du jour :\\n- suite\r\n"));
        let back = instances(&ics, 1_792_000_000, 1_793_000_000, &[]);
        assert_eq!(back.len(), 1);
        assert_eq!((back[0].summary.as_str(), back[0].start, back[0].end, back[0].own, back[0].recurring), ("Point, projet", 1_792_497_600, 1_792_501_200, true, false));

        let local = |d: u32| chrono::Local.with_ymd_and_hms(2026, 12, d, 0, 0, 0).earliest().unwrap().timestamp();
        let day = crate::model::EventDraft { summary: "Noël".into(), start: local(24), end: local(26), all_day: true, ..Default::default() };
        let ics = new_event("u2@facteur", &day, None, 1_792_400_000).unwrap();
        assert!(ics.contains("DTSTART;VALUE=DATE:20261224\r\n") && ics.contains("DTEND;VALUE=DATE:20261226\r\n"));
        assert!(!ics.contains("LOCATION") && !ics.contains("DESCRIPTION"));
        // A whole day ending where it starts still lasts a day.
        let one = crate::model::EventDraft { end: local(24), ..day };
        assert!(new_event("u3@facteur", &one, None, 0).unwrap().contains("DTEND;VALUE=DATE:20261225\r\n"));
    }

    #[test]
    fn writes_and_rebuilds_a_series() {
        use crate::repeat::{Freq, Repeat};
        let at = |d: u32, h: u32| chrono::Local.with_ymd_and_hms(2026, 10, d, h, 0, 0).earliest().unwrap().timestamp();
        let weekly = Repeat { freq: Freq::Weekly, interval: 1, weekdays: vec![0, 2], by_weekday: false, until: None, count: Some(4) };
        let draft = crate::model::EventDraft { summary: "Sport".into(), start: at(19, 18), end: at(19, 19), repeat: Some(weekly), ..Default::default() };
        let ics = new_event("s1@facteur", &draft, None, 1_792_400_000).unwrap();
        assert!(ics.contains("RRULE:FREQ=WEEKLY;BYDAY=MO,WE;COUNT=4\r\n"), "{ics}");
        let dates = instances(&ics, at(1, 0), at(31, 0), &[]);
        assert_eq!(dates.iter().map(|d| d.start).collect::<Vec<_>>(), vec![at(19, 18), at(21, 18), at(26, 18), at(28, 18)]);
        assert!(dates.iter().all(|d| d.recurring && d.end - d.start == 3600));
        assert_eq!(series_rule(&ics), Some((Some("FREQ=WEEKLY;BYDAY=MO,WE;COUNT=4".into()), chrono::NaiveDate::from_ymd_opt(2026, 10, 19).unwrap())));

        // One date moved alone, then the series rebuilt on Tuesdays, every day long: the date
        // moved alone named the old schedule and goes.
        let moved = edit_occurrence(&ics, at(21, 18), (at(21, 18), at(21, 19), false), &EventChanges { times: Some((at(21, 20), at(21, 21), false)), ..Default::default() }, 1_792_400_000).unwrap();
        assert!(moved.contains("RECURRENCE-ID"));
        let tuesdays = Repeat { freq: Freq::Weekly, interval: 1, weekdays: vec![1], by_weekday: false, until: Some("2026-10-27".into()), count: None };
        let first = series_first(chrono::NaiveDate::from_ymd_opt(2026, 10, 19).unwrap(), at(21, 18), &crate::model::EventDraft { start: at(22, 0), end: at(23, 0), all_day: true, ..Default::default() }).unwrap();
        assert_eq!(first, (at(20, 0), at(21, 0), chrono::NaiveDate::from_ymd_opt(2026, 10, 20).unwrap()));
        let rebuilt = rebuild_series(&moved, &EventChanges::default(), (first.0, first.1, true), Some(&tuesdays), 1_792_500_000).unwrap();
        assert!(!rebuilt.contains("RECURRENCE-ID") && rebuilt.contains("DTSTART;VALUE=DATE:20261020\r\n") && rebuilt.contains("SEQUENCE:1\r\n"), "{rebuilt}");
        let dates = instances(&rebuilt, at(1, 0), at(31, 0), &[]);
        assert_eq!(dates.iter().map(|d| (d.start, d.all_day)).collect::<Vec<_>>(), vec![(day_stamp(first.2), true), (day_stamp(first.2) + 7 * 86400, true)]);

        // And back to a single timed event.
        let single = rebuild_series(&rebuilt, &EventChanges::default(), (at(20, 9), at(20, 10), false), None, 1_792_600_000).unwrap();
        assert!(!single.contains("RRULE:FREQ=WEEKLY") && single.contains("SEQUENCE:2\r\n"), "{single}");
        assert_eq!(instances(&single, at(1, 0), at(31, 0), &[]).iter().map(|d| (d.start, d.recurring)).collect::<Vec<_>>(), vec![(at(20, 9), false)]);

        // And a single event made a series again, its zone written once.
        let daily = Repeat { freq: Freq::Daily, interval: 1, weekdays: vec![], by_weekday: false, until: None, count: Some(3) };
        let again = rebuild_series(&single, &EventChanges::default(), (at(20, 9), at(20, 10), false), Some(&daily), 1_792_700_000).unwrap();
        assert_eq!(instances(&again, at(1, 0), at(31, 0), &[]).iter().map(|d| (d.start, d.recurring)).collect::<Vec<_>>(), vec![(at(20, 9), true), (at(21, 9), true), (at(22, 9), true)]);
        assert_eq!(again.matches("BEGIN:VTIMEZONE").count(), 1);
    }

    #[test]
    fn edits_an_event_and_keeps_the_rest() {
        let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Other//EN\r\nBEGIN:VEVENT\r\nUID:e1\r\nDTSTAMP:20261001T000000Z\r\n\
DTSTART;TZID=Europe/Paris:20261020T140000\r\nDTEND;TZID=Europe/Paris:20261020T150000\r\nSUMMARY:Ancien\r\nLOCATION:Salle 1\r\n\
SEQUENCE:3\r\nX-OTHER:kept\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nDESCRIPTION:Rappel\r\nTRIGGER:-PT10M\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let changes = EventChanges { summary: Some("Nouveau".into()), location: Some(None), ..Default::default() };
        let out = edit_event(ics, &changes, 1_792_400_000).unwrap();
        assert!(out.contains("SUMMARY:Nouveau\r\n") && !out.contains("Ancien") && !out.contains("LOCATION"));
        assert!(out.contains("DTSTART;TZID=Europe/Paris:20261020T140000\r\n"), "times untouched");
        assert!(out.contains("SEQUENCE:4\r\n") && !out.contains("SEQUENCE:3") && !out.contains("20261001T000000Z"));
        assert!(out.contains("X-OTHER:kept\r\n") && out.contains("BEGIN:VALARM\r\nACTION:DISPLAY\r\nDESCRIPTION:Rappel\r\n"), "the alarm keeps its description");

        let moved = EventChanges { times: Some((1_792_501_200, 1_792_504_800, false)), ..Default::default() };
        let out = edit_event(ics, &moved, 1_792_400_000).unwrap();
        assert!(out.contains("DTSTART:20261020T130000Z\r\nDTEND:20261020T140000Z\r\n") && !out.contains("TZID=Europe/Paris:20261020T140000"));
        assert_eq!(instances(&out, 1_792_000_000, 1_793_000_000, &[])[0].start, 1_792_501_200);

        let series = ics.replace("SEQUENCE:3\r\n", "RRULE:FREQ=WEEKLY\r\n");
        assert!(edit_event(&series, &changes, 0).is_none());
    }

    const WEEKLY: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Other//EN\r\n\
BEGIN:VTIMEZONE\r\nTZID:Europe/Paris\r\nBEGIN:STANDARD\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\nDTSTART:19701025T030000\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\n\
BEGIN:VEVENT\r\nUID:w1\r\nDTSTAMP:20261001T000000Z\r\nDTSTART;TZID=Europe/Paris:20261005T140000\r\nDTEND;TZID=Europe/Paris:20261005T150000\r\n\
RRULE:FREQ=WEEKLY;UNTIL=20261231T130000Z\r\nEXDATE;TZID=Europe/Paris:20261012T140000\r\nSUMMARY:Point\r\nLOCATION:Salle 1\r\n\
ORGANIZER:mailto:marie@example.org\r\nATTENDEE;CN=Y;RSVP=TRUE;PARTSTAT=NEEDS-ACTION:mailto:y@example.org\r\n\
BEGIN:VALARM\r\nACTION:DISPLAY\r\nDESCRIPTION:Rappel\r\nTRIGGER:-PT10M\r\nEND:VALARM\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:w1\r\nRECURRENCE-ID;TZID=Europe/Paris:20261019T140000\r\nDTSTART;TZID=Europe/Paris:20261019T160000\r\n\
DTEND;TZID=Europe/Paris:20261019T170000\r\nSUMMARY:Point (décalé)\r\nLOCATION:Salle 1\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    fn paris(y: i32, m: u32, d: u32, h: u32) -> i64 {
        ts(y, m, d, h, 0, chrono_tz::Europe::Paris)
    }

    fn occurrences(ics: &str) -> Vec<(i64, i64, String)> {
        let mut v: Vec<_> = instances(ics, paris(2026, 10, 1, 0), paris(2026, 11, 3, 0), &[]).into_iter().map(|i| (i.start, i.end, i.summary)).collect();
        v.sort();
        v
    }

    #[test]
    fn a_series_moves_and_is_renamed_as_a_whole() {
        let before = occurrences(WEEKLY);
        assert_eq!(before.len(), 4, "5, 19 (moved), 26 Oct and 2 Nov; the 12th excluded");
        // The 26th moved an hour later and lasting two: the whole series does.
        let current = (paris(2026, 10, 26, 14), paris(2026, 10, 26, 15), false);
        let changes = EventChanges { summary: Some("Point hebdo".into()), times: Some((paris(2026, 10, 26, 15), paris(2026, 10, 26, 17), false)), ..Default::default() };
        let out = edit_series(WEEKLY, &changes, current, 1_792_400_000).unwrap();
        assert!(out.contains("DTSTART;TZID=Europe/Paris:20261005T150000\r\nDTEND;TZID=Europe/Paris:20261005T170000\r\n"));
        assert!(out.contains("EXDATE;TZID=Europe/Paris:20261012T150000\r\n") && out.contains("UNTIL=20261231T140000Z"));
        assert!(out.contains("RECURRENCE-ID;TZID=Europe/Paris:20261019T150000\r\n"), "the occurrence changed alone follows");
        assert!(out.contains("BEGIN:VALARM\r\nACTION:DISPLAY\r\nDESCRIPTION:Rappel"));
        let after = occurrences(&out);
        assert_eq!(after.len(), 4);
        assert_eq!(after[0], (paris(2026, 10, 5, 15), paris(2026, 10, 5, 17), "Point hebdo".into()));
        // Wall time kept across the change of time on 25 October.
        assert_eq!(after[2].0, paris(2026, 10, 26, 15));
        assert_eq!(after[1].2, "Point (décalé)", "a title given apart is kept");
        // Into whole days: refused.
        let days = EventChanges { times: Some((paris(2026, 10, 26, 0), paris(2026, 10, 27, 0), true)), ..Default::default() };
        assert!(edit_series(WEEKLY, &days, current, 0).is_none());
        // Sent again (an answer lost on the way), the same change gives the same series.
        let again = edit_series(&out, &changes, current, 1_792_400_000).unwrap();
        assert_eq!(occurrences(&again), after);
    }

    #[test]
    fn one_occurrence_changes_or_goes_alone() {
        let current = (paris(2026, 10, 26, 14), paris(2026, 10, 26, 15), false);
        let changes = EventChanges { summary: Some("Exceptionnel".into()), ..Default::default() };
        let out = edit_occurrence(WEEKLY, paris(2026, 10, 26, 14), current, &changes, 1_792_400_000).unwrap();
        assert!(out.contains("RECURRENCE-ID;TZID=Europe/Paris:20261026T140000\r\n"));
        let after = occurrences(&out);
        assert_eq!(after.len(), 4);
        assert_eq!(after[2], (paris(2026, 10, 26, 14), paris(2026, 10, 26, 15), "Exceptionnel".into()));
        assert_eq!(after[3].2, "Point", "the others stay");
        // The one changed already is changed in place.
        let moved = EventChanges { times: Some((paris(2026, 10, 19, 17), paris(2026, 10, 19, 18), false)), ..Default::default() };
        let out = edit_occurrence(WEEKLY, paris(2026, 10, 19, 14), (paris(2026, 10, 19, 16), paris(2026, 10, 19, 17), false), &moved, 0).unwrap();
        assert_eq!(out.matches("RECURRENCE-ID").count(), 1);
        assert_eq!(occurrences(&out)[1], (paris(2026, 10, 19, 17), paris(2026, 10, 19, 18), "Point (décalé)".into()));

        let out = delete_occurrence(WEEKLY, paris(2026, 10, 19, 14), 0).unwrap();
        assert!(!out.contains("RECURRENCE-ID") && out.contains("EXDATE;TZID=Europe/Paris:20261019T140000\r\n"));
        assert_eq!(occurrences(&out).len(), 3);
        let out = delete_occurrence(WEEKLY, paris(2026, 10, 26, 14), 0).unwrap();
        assert_eq!(occurrences(&out).iter().map(|o| o.0).collect::<Vec<_>>(), [paris(2026, 10, 5, 14), paris(2026, 10, 19, 16), paris(2026, 11, 2, 14)]);
    }

    #[test]
    fn answers_an_invitation_in_the_calendar() {
        let me = ["Y@example.org".to_string()];
        let out = respond_event(WEEKLY, &me, "ACCEPTED", None, 1_792_400_000).unwrap();
        assert!(out.contains("ATTENDEE;CN=Y;PARTSTAT=ACCEPTED:mailto:y@example.org\r\n") && !out.contains("NEEDS-ACTION"));
        assert!(out.contains("ORGANIZER:mailto:marie@example.org"));
        assert!(respond_event(WEEKLY, &["z@example.org".into()], "ACCEPTED", None, 0).is_none(), "not invited");
        // One occurrence: a block of its own, at its times.
        let current = (paris(2026, 10, 26, 14), paris(2026, 10, 26, 15), false);
        let out = respond_event(WEEKLY, &me, "DECLINED", Some((paris(2026, 10, 26, 14), current)), 0).unwrap();
        assert_eq!(out.matches("PARTSTAT=DECLINED").count(), 1);
        assert_eq!(out.matches("PARTSTAT=NEEDS-ACTION").count(), 1, "the series still waits");
        let declined = instances(&out, paris(2026, 10, 26, 0), paris(2026, 10, 27, 0), &me);
        assert_eq!((declined.len(), declined[0].start, declined[0].busy), (1, paris(2026, 10, 26, 14), false));
        // A mailed reply about that date alone carries only its block.
        let alone = occurrence_ics(&out, paris(2026, 10, 26, 14)).unwrap();
        assert_eq!(alone.matches("BEGIN:VEVENT").count(), 1);
        assert!(alone.contains("RECURRENCE-ID;TZID=Europe/Paris:20261026T140000") && alone.contains("BEGIN:VTIMEZONE"));
    }

    #[test]
    fn only_what_changed() {
        let old = crate::model::CalendarEvent {
            id: 1, calendar_id: 1, account_id: 1, uid: "u".into(), instance: String::new(), summary: "Point".into(),
            location: Some("Salle".into()), description: Some("Long text cut by the provider".into()), start: 100, end: 200,
            all_day: false, status: None, organizer: None, url: None, busy: true, recurring: false, editable: true, response: None, respondable: false,
            attendees: Vec::new(), meeting: None, reminders: Vec::new(),
        };
        let same = crate::model::EventDraft {
            summary: "Point ".into(), location: Some("Salle".into()), description: Some("Long text cut by the provider".into()), start: 100, end: 200, all_day: false, repeat: None, attendees: None,
        };
        assert!(EventChanges::between(&old, &same).is_empty());
        let draft = crate::model::EventDraft { location: Some("  ".into()), end: 300, ..same };
        assert_eq!(EventChanges::between(&old, &draft), EventChanges { location: Some(None), times: Some((100, 300, false)), ..Default::default() });
    }

    #[test]
    fn reads_guests_reminders_and_meeting_links() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:g\r\nDTSTART:20261020T120000Z\r\nDTEND:20261020T130000Z\r\n\
ORGANIZER;CN=Marie:mailto:marie@example.org\r\n\
ATTENDEE;CN=Marie;PARTSTAT=ACCEPTED:mailto:marie@example.org\r\n\
ATTENDEE;PARTSTAT=TENTATIVE:mailto:Y@example.org\r\n\
ATTENDEE;CUTYPE=ROOM;PARTSTAT=ACCEPTED:mailto:salle@example.org\r\n\
DESCRIPTION:Rejoindre : https://teams.microsoft.com/l/meetup-join/abc).\\nMerci\r\n\
BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT10M\r\nEND:VALARM\r\n\
BEGIN:VALARM\r\nACTION:EMAIL\r\nTRIGGER:-P1D\r\nEND:VALARM\r\n\
BEGIN:VALARM\r\nACTION:AUDIO\r\nTRIGGER;VALUE=DATE-TIME:20261020T110000Z\r\nEND:VALARM\r\n\
BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER;RELATED=END:-PT5M\r\nEND:VALARM\r\n\
END:VEVENT\r\nEND:VCALENDAR\r\n";
        let e = &instances(ics, 1_792_000_000, 1_793_000_000, &["y@example.org".into()])[0];
        let who: Vec<(&str, Option<&str>, bool)> = e.attendees.iter().map(|a| (a.email.as_str(), a.response.as_deref(), a.organizer)).collect();
        assert_eq!(who, [("marie@example.org", Some("accepted"), true), ("Y@example.org", Some("tentative"), false)]);
        assert_eq!(e.attendees[0].name.as_deref(), Some("Marie"));
        assert_eq!(e.reminders, [10, 60], "the mail and the end-related ones left out");
        assert_eq!(e.meeting.as_deref(), Some("https://teams.microsoft.com/l/meetup-join/abc"));
        // An alarm's own text is no event text.
        assert_eq!(e.description.as_deref(), Some("Rejoindre : https://teams.microsoft.com/l/meetup-join/abc).\nMerci"));
        assert_eq!(meeting_link(["Salle 3", "voir https://example.org/meet.google.com et https://meet.google.com/abc-defg-hij"]).as_deref(), Some("https://meet.google.com/abc-defg-hij"));
        assert_eq!(meeting_link(["https://us02web.zoom.us/j/123?pwd=x"]).as_deref(), Some("https://us02web.zoom.us/j/123?pwd=x"));
        assert_eq!(meeting_link(["https://evil.example/zoom.us"]), None);
    }

    #[test]
    fn invites_guests_and_tells_them() {
        use crate::model::EventAttendee;
        let guest = |email: &str| EventAttendee { email: email.into(), ..Default::default() };
        let me = EventAttendee { email: "y@example.org".into(), name: Some("Y, moi".into()), organizer: true, ..Default::default() };
        let draft = crate::model::EventDraft {
            summary: "Point".into(), start: 1_792_497_600, end: 1_792_501_200,
            attendees: Some(vec![guest(" marie@example.org "), guest("Marie@example.org"), guest("pas une adresse"), guest("y@example.org")]),
            ..Default::default()
        };
        let unfolded = |t: &str| t.replace("\r\n ", "");
        let ics = unfolded(&new_event("i1@facteur", &draft, Some(me.clone()), 1_792_400_000).unwrap());
        assert!(ics.contains("ORGANIZER;CN=\"Y, moi\":mailto:y@example.org\r\n"), "{ics}");
        assert_eq!(ics.matches("ATTENDEE").count(), 1, "each guest once, valid, the organizer left out: {ics}");
        assert!(ics.contains("ATTENDEE;ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:marie@example.org\r\n"));
        // Without guests, no organizer either.
        let alone = new_event("i2@facteur", &crate::model::EventDraft { attendees: None, ..draft.clone() }, Some(me.clone()), 0).unwrap();
        assert!(!alone.contains("ORGANIZER") && !alone.contains("ATTENDEE"));

        // Marie answered; Paul is added: Marie keeps her answer.
        let answered = ics.replace("PARTSTAT=NEEDS-ACTION", "PARTSTAT=ACCEPTED");
        let old = &instances(&answered, 1_792_000_000, 1_793_000_000, &["y@example.org".into()])[0];
        let old = crate::model::CalendarEvent {
            id: 1, calendar_id: 1, account_id: 1, uid: old.uid.clone(), instance: String::new(), summary: old.summary.clone(), location: None, description: None,
            start: old.start, end: old.end, all_day: false, status: None, organizer: None, url: None, busy: true, recurring: false, editable: true,
            response: None, respondable: false, attendees: old.attendees.clone(), meeting: None, reminders: Vec::new(),
        };
        let more = crate::model::EventDraft { attendees: Some(vec![guest("marie@example.org"), guest("paul@example.org")]), ..draft.clone() };
        let changes = EventChanges::between(&old, &more).organized_by(me.clone());
        let out = unfolded(&edit_event(&answered, &changes, 1_792_450_000).unwrap());
        assert!(out.contains("PARTSTAT=ACCEPTED;RSVP=TRUE:mailto:marie@example.org") && out.contains("PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:paul@example.org"), "{out}");
        assert_eq!(out.matches("ORGANIZER").count(), 1);
        let same = crate::model::EventDraft { attendees: Some(vec![guest("MARIE@example.org")]), ..draft.clone() };
        assert!(EventChanges::between(&old, &same).attendees.is_none(), "the same guests");

        let request = invitation(&out, "REQUEST", 1_792_460_000).unwrap();
        assert!(request.starts_with("BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\n") && !request.contains("STATUS:CANCELLED"));
        let cancel = invitation(&request, "CANCEL", 1_792_460_000).unwrap();
        assert!(cancel.contains("METHOD:CANCEL\r\n") && !cancel.contains("METHOD:REQUEST") && cancel.contains("STATUS:CANCELLED\r\n"));
        assert_eq!(parse(&cancel).map(|i| (i.method, i.cancelled)), Some(("CANCEL".to_string(), true)));
    }

    #[test]
    fn who_organizes_an_event() {
        let ics = |organizer: &str| format!("BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:x\r\n{organizer}DTSTART:20261020T120000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n");
        let me = ["y@example.org".to_string()];
        let own = |o: &str| instances(&ics(o), 1_792_000_000, 1_793_000_000, &me)[0].own;
        assert!(own(""));
        assert!(own("ORGANIZER;CN=Y:mailto:Y@example.org\r\n"));
        assert!(!own("ORGANIZER:mailto:marie@example.org\r\n"));
    }

    #[test]
    fn replies_to_the_organizer() {
        let me = Address { name: Some("Yann".into()), email: "y@example.org".into() };
        let r = reply(INVITE, &me, "ACCEPTED", 1_800_000_000).unwrap();
        assert!(r.contains("METHOD:REPLY\r\n"));
        assert!(r.contains("BEGIN:VTIMEZONE\r\nTZID:Europe/Paris"));
        assert!(r.contains("UID:abc123@google.com\r\n") && r.contains("SEQUENCE:2\r\n"));
        assert!(r.contains("DTSTART;TZID=Europe/Paris:20261020T140000\r\n"));
        assert!(r.contains("ATTENDEE;PARTSTAT=ACCEPTED;CN=\"Yann\":mailto:y@example.org"));
        assert!(!r.contains("VALARM") && !r.contains("LOCATION"), "only what identifies the event");
        assert!(r.lines().all(|l| l.len() <= 76));
        let back = parse(&r).unwrap();
        assert_eq!((back.method.as_str(), back.attendees[0].partstat.as_deref()), ("REPLY", Some("ACCEPTED")));
    }

    fn ts(y: i32, m: u32, d: u32, h: u32, min: u32, tz: chrono_tz::Tz) -> i64 {
        tz.with_ymd_and_hms(y, m, d, h, min, 0).unwrap().timestamp()
    }

    fn utc_day(y: i32, m: u32, d: u32) -> i64 {
        chrono::Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap().timestamp()
    }

    #[test]
    fn reads_events_in_their_time_zone() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VTIMEZONE\r\nTZID:Europe/Paris\r\nEND:VTIMEZONE\r\n\
BEGIN:VEVENT\r\nUID:a\r\nDTSTART;TZID=Europe/Paris:20260704T140000\r\nDURATION:PT1H30M\r\nSUMMARY:Été\r\n\
ORGANIZER;CN=Marie:mailto:marie@example.org\r\nSTATUS:TENTATIVE\r\nURL:https://cal.example.org/e/a\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:b\r\nDTSTART;TZID=\"/mozilla.org/20050126_1/America/New_York\":20261220T090000\r\n\
DTEND;TZID=America/New_York:20261220T100000\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:c\r\nDTSTART;TZID=Romance Standard Time:20261220T090000\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:day\r\nDTSTART;VALUE=DATE:20261224\r\nDTEND;VALUE=DATE:20261226\r\nSUMMARY:Noël\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let all = instances(ics, 0, i64::MAX, &[]);
        let get = |uid: &str| all.iter().find(|i| i.uid == uid).unwrap();
        let a = get("a");
        assert_eq!((a.start, a.end - a.start, a.all_day), (ts(2026, 7, 4, 14, 0, chrono_tz::Europe::Paris), 5400, false));
        assert_eq!((a.organizer.as_deref(), a.status.as_deref(), a.url.as_deref()), (Some("Marie"), Some("tentative"), Some("https://cal.example.org/e/a")));
        assert_eq!(get("b").start, ts(2026, 12, 20, 9, 0, chrono_tz::America::New_York));
        assert_eq!(get("b").end - get("b").start, 3600);
        assert_eq!(get("c").start, ts(2026, 12, 20, 9, 0, chrono_tz::Europe::Paris));
        let day = get("day");
        assert!(day.all_day);
        // Days are stored as their UTC midnights, whatever the computer's zone.
        assert_eq!((day.start, day.end), (utc_day(2026, 12, 24), utc_day(2026, 12, 26)));
        assert_eq!(day.instance, "");
        // Window: only what overlaps it.
        assert_eq!(instances(ics, utc_day(2026, 12, 25), utc_day(2026, 12, 26), &[]).iter().map(|i| i.uid.as_str()).collect::<Vec<_>>(), ["day"]);
    }

    #[test]
    fn expands_simple_recurrences() {
        let paris = chrono_tz::Europe::Paris;
        // Every Tuesday and Thursday at 10:00 Paris time, 6 times, one removed, one moved.
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:w\r\nDTSTART;TZID=Europe/Paris:20261020T100000\r\n\
DTEND;TZID=Europe/Paris:20261020T110000\r\nRRULE:FREQ=WEEKLY;BYDAY=TU,TH;COUNT=6\r\nSUMMARY:Point\r\n\
EXDATE;TZID=Europe/Paris:20261022T100000\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:w\r\nRECURRENCE-ID;TZID=Europe/Paris:20261027T100000\r\nDTSTART;TZID=Europe/Paris:20261027T150000\r\n\
DTEND;TZID=Europe/Paris:20261027T160000\r\nSUMMARY:Point (déplacé)\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let mut got = instances(ics, 0, i64::MAX, &[]);
        got.sort_by_key(|i| i.start);
        let starts: Vec<i64> = got.iter().map(|i| i.start).collect();
        assert_eq!(
            starts,
            [
                ts(2026, 10, 20, 10, 0, paris),
                ts(2026, 10, 27, 15, 0, paris),
                ts(2026, 10, 29, 10, 0, paris),
                ts(2026, 11, 3, 10, 0, paris),
                ts(2026, 11, 5, 10, 0, paris),
            ]
        );
        // Across the change to winter time, still 10:00 in Paris, an hour long.
        assert!(got.iter().all(|i| i.end - i.start == 3600));
        assert_eq!(got[1].summary, "Point (déplacé)");
        assert_eq!(got[1].instance, ts(2026, 10, 27, 10, 0, paris).to_string());

        // Monthly on the last Friday, until a date; yearly whole-day; daily with an interval.
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:m\r\nDTSTART:20260130T170000Z\r\nRRULE:FREQ=MONTHLY;BYDAY=-1FR;UNTIL=20260501T000000Z\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:y\r\nDTSTART;VALUE=DATE:20200229\r\nRRULE:FREQ=YEARLY\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:d\r\nDTSTART:20261001T080000Z\r\nRRULE:FREQ=DAILY;INTERVAL=10\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:x\r\nDTSTART:20261001T080000Z\r\nRRULE:FREQ=MONTHLY;BYSETPOS=-1;BYDAY=MO,TU\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let utc = chrono_tz::UTC;
        let got = instances(ics, 0, ts(2027, 1, 1, 0, 0, utc), &[]);
        let of = |uid: &str| got.iter().filter(|i| i.uid == uid).map(|i| i.start).collect::<Vec<_>>();
        assert_eq!(of("m"), [ts(2026, 1, 30, 17, 0, utc), ts(2026, 2, 27, 17, 0, utc), ts(2026, 3, 27, 17, 0, utc), ts(2026, 4, 24, 17, 0, utc)]);
        assert_eq!(of("y"), [utc_day(2020, 2, 29), utc_day(2024, 2, 29)], "only leap years have a February 29th");
        assert_eq!(of("d").len(), 10);
        assert_eq!(of("x"), [ts(2026, 10, 1, 8, 0, utc)], "a rule not understood shows its first occurrence");
    }

    #[test]
    fn reads_durations() {
        assert_eq!(duration("PT1H30M"), Some((0, 5400)));
        assert_eq!(duration("P1W"), Some((7, 0)));
        assert_eq!(duration("P1DT2H"), Some((1, 7200)));
        assert_eq!(duration("-PT15M"), Some((0, -900)));
        assert_eq!(duration("P1H"), None);

        let paris = chrono_tz::Europe::Paris;
        // A day on the wall clock: across the change to winter time, 25 hours.
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nDTSTART;TZID=Europe/Paris:20261024T100000\r\nDURATION:P1DT1H\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let e = &instances(ics, 0, i64::MAX, &[])[0];
        assert_eq!(e.end, ts(2026, 10, 25, 11, 0, paris));
        // An invitation with a DURATION has an end.
        let invite = "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:i\r\nDTSTART;TZID=Europe/Paris:20261020T140000\r\nDURATION:PT45M\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let i = parse(invite).unwrap();
        assert_eq!(i.end.unwrap() - i.start.unwrap(), 2700);
        let day = parse("BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:d\nDTSTART;VALUE=DATE:20261224\nDURATION:P2D\nEND:VEVENT\nEND:VCALENDAR").unwrap();
        assert!(day.all_day);
        assert!((2 * 86400 - 3600..=2 * 86400 + 3600).contains(&(day.end.unwrap() - day.start.unwrap())));
    }

    #[test]
    fn every_windows_zone_is_known() {
        for (windows, iana) in WINDOWS_ZONES {
            assert!(iana.parse::<chrono_tz::Tz>().is_ok(), "{windows} → {iana}");
        }
        assert_eq!(named_zone("pacific standard time"), Some(chrono_tz::America::Los_Angeles));
        assert_eq!(named_zone("Tasmania Standard Time"), Some(chrono_tz::Australia::Hobart));
    }

    #[test]
    fn reads_a_zone_from_its_vtimezone() {
        let paris = chrono_tz::Europe::Paris;
        // Outlook names the zone by its display name: its rules come from the VTIMEZONE.
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VTIMEZONE\r\nTZID:(UTC+01:00) Bruxelles\\, Copenhague\\, Madrid\\, Paris\r\n\
BEGIN:STANDARD\r\nDTSTART:16010101T030000\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\nRRULE:FREQ=YEARLY;BYDAY=-1SU;BYMONTH=10\r\nEND:STANDARD\r\n\
BEGIN:DAYLIGHT\r\nDTSTART:16010101T020000\r\nTZOFFSETFROM:+0100\r\nTZOFFSETTO:+0200\r\nRRULE:FREQ=YEARLY;BYDAY=-1SU;BYMONTH=3\r\nEND:DAYLIGHT\r\nEND:VTIMEZONE\r\n\
BEGIN:VEVENT\r\nUID:s\r\nDTSTART;TZID=\"(UTC+01:00) Bruxelles\\, Copenhague\\, Madrid\\, Paris\":20260704T140000\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:w\r\nDTSTART;TZID=\"(UTC+01:00) Bruxelles\\, Copenhague\\, Madrid\\, Paris\":20261220T090000\r\nEND:VEVENT\r\n\
BEGIN:VTIMEZONE\r\nTZID:Custom fixed\r\nBEGIN:STANDARD\r\nDTSTART:19700101T000000\r\nTZOFFSETFROM:+0530\r\nTZOFFSETTO:+0530\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\n\
BEGIN:VEVENT\r\nUID:f\r\nDTSTART;TZID=Custom fixed:20261220T090000\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let all = instances(ics, 0, i64::MAX, &[]);
        let get = |uid: &str| all.iter().find(|i| i.uid == uid).unwrap().start;
        assert_eq!(get("s"), ts(2026, 7, 4, 14, 0, paris));
        assert_eq!(get("w"), ts(2026, 12, 20, 9, 0, paris));
        assert_eq!(get("f"), ts(2026, 12, 20, 9, 0, chrono_tz::Asia::Kolkata));
    }

    #[test]
    fn weeks_start_on_wkst() {
        let utc = chrono_tz::UTC;
        // RFC 5545 §3.8.5.3: the same rule, weeks starting on Monday or on Sunday.
        let rule = |wkst: &str| {
            format!("BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:r\r\nDTSTART:19970805T090000Z\r\nRRULE:FREQ=WEEKLY;INTERVAL=2;COUNT=4;BYDAY=TU,SU;WKST={wkst}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n")
        };
        let days = |ics: &str| {
            let mut v: Vec<i64> = instances(ics, 0, i64::MAX, &[]).iter().map(|i| i.start).collect();
            v.sort();
            v
        };
        let at = |d: u32| ts(1997, 8, d, 9, 0, utc);
        assert_eq!(days(&rule("MO")), [at(5), at(10), at(19), at(24)]);
        assert_eq!(days(&rule("SU")), [at(5), at(17), at(19), at(31)]);
    }

    #[test]
    fn floating_exdate_and_until_follow_dtstart() {
        let ny = chrono_tz::America::New_York;
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:x\r\nDTSTART;TZID=America/New_York:20261005T090000\r\n\
RRULE:FREQ=DAILY;UNTIL=20261008T090000\r\nEXDATE:20261006T090000\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let mut got: Vec<i64> = instances(ics, 0, i64::MAX, &[]).iter().map(|i| i.start).collect();
        got.sort();
        assert_eq!(got, [ts(2026, 10, 5, 9, 0, ny), ts(2026, 10, 7, 9, 0, ny), ts(2026, 10, 8, 9, 0, ny)]);
    }

    #[test]
    fn old_series_still_show() {
        let utc = chrono_tz::UTC;
        // Daily since 1980 (more than 20 000 days), no end: the window still has its days.
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:o\r\nDTSTART:19800101T080000Z\r\nDTEND:19800101T090000Z\r\nRRULE:FREQ=DAILY\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:c\r\nDTSTART:19800101T080000Z\r\nRRULE:FREQ=WEEKLY;COUNT=5000\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let (from, to) = (ts(2026, 10, 1, 0, 0, utc), ts(2026, 10, 8, 0, 0, utc));
        let got = instances(ics, from, to, &[]);
        assert_eq!(got.iter().filter(|i| i.uid == "o").count(), 7);
        assert_eq!(got.iter().find(|i| i.uid == "o").unwrap().instance, ts(2026, 10, 1, 8, 0, utc).to_string());
        // With COUNT, the occurrences before are counted: 5000 weeks reach 2075.
        assert_eq!(got.iter().filter(|i| i.uid == "c").count(), 1);
    }

    #[test]
    fn yearly_by_month_day_runs_every_month() {
        let utc = chrono_tz::UTC;
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:y\r\nDTSTART:20260115T080000Z\r\nRRULE:FREQ=YEARLY;BYMONTHDAY=15;COUNT=4\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let mut got: Vec<i64> = instances(ics, 0, i64::MAX, &[]).iter().map(|i| i.start).collect();
        got.sort();
        assert_eq!(got, [ts(2026, 1, 15, 8, 0, utc), ts(2026, 2, 15, 8, 0, utc), ts(2026, 3, 15, 8, 0, utc), ts(2026, 4, 15, 8, 0, utc)]);
    }

    #[test]
    fn free_or_busy() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:busy\r\nDTSTART:20261020T080000Z\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:free\r\nDTSTART:20261020T080000Z\r\nTRANSP:TRANSPARENT\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:declined\r\nDTSTART:20261020T080000Z\r\nATTENDEE;PARTSTAT=DECLINED:mailto:Y@example.org\r\n\
ATTENDEE;PARTSTAT=ACCEPTED:mailto:marie@example.org\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:other-declined\r\nDTSTART:20261020T080000Z\r\nATTENDEE;PARTSTAT=DECLINED:mailto:marie@example.org\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let all = instances(ics, 0, i64::MAX, &["y@example.org".to_string()]);
        let busy = |uid: &str| all.iter().find(|i| i.uid == uid).unwrap().busy;
        assert!(busy("busy") && busy("other-declined"));
        assert!(!busy("free") && !busy("declined"));
    }

    #[test]
    fn recurring_invitation_knows_its_next_time() {
        let i = parse("BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:r\r\nDTSTART:20200106T090000Z\r\nDTEND:20200106T100000Z\r\n\
RRULE:FREQ=WEEKLY\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n").unwrap();
        let now = chrono::Utc::now().timestamp();
        let (start, end) = (i.next_start.unwrap(), i.next_end.unwrap());
        assert!(end > now && start <= now + 7 * 86400 && end - start == 3600);
        assert_eq!((start - i.start.unwrap()) % (7 * 86400), 0);
        assert!(parse(INVITE).unwrap().next_start.is_none());
    }

    #[test]
    fn hostile_values_do_not_panic() {
        let wrap = |body: &str| format!("BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\n{body}END:VCALENDAR\r\n");
        let zone = "BEGIN:VTIMEZONE\r\nTZID:X\r\nBEGIN:STANDARD\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\n\
DTSTART:19701025T030000\r\nRRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=€\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\n\
BEGIN:VEVENT\r\nUID:z\r\nDTSTART;TZID=X:20261020T140000\r\nDTEND;TZID=X:20261020T150000\r\nEND:VEVENT\r\n";
        let monthly = "BEGIN:VEVENT\r\nUID:m\r\nDTSTART:20261005T090000Z\r\nRRULE:FREQ=MONTHLY;BYDAY=-100000000MO\r\nEND:VEVENT\r\n";
        let day = "BEGIN:VEVENT\r\nUID:d\r\nDTSTART;VALUE=DATE:20261224\r\nDURATION:P100000000D\r\nEND:VEVENT\r\n";
        let timed = "BEGIN:VEVENT\r\nUID:t\r\nDTSTART:20261020T080000Z\r\nDURATION:P100000000DT9999999999999999H\r\nEND:VEVENT\r\n";
        for body in [zone, monthly, day, timed] {
            let ics = wrap(body);
            assert!(parse(&ics).is_some(), "{body}");
            assert_eq!(instances(&ics, 0, i64::MAX, &[]).len(), 1, "{body}");
        }
        assert_eq!(Rule::parse("FREQ=YEARLY;BYDAY=€"), None);
        assert_eq!(Rule::parse("FREQ=MONTHLY;BYDAY=-100000000MO"), None);
        assert_eq!(nth_weekday(2026, 10, chrono::Weekday::Mon, -100_000_000), None);
        assert_eq!((duration("P100000000D"), duration("PT9999999999999999H"), duration("P2W")), (None, None, Some((14, 0))));
        // A day event with an unusable DURATION lasts one day.
        let d = instances(&wrap(day), 0, i64::MAX, &[]);
        assert_eq!(d[0].end - d[0].start, 86400);
    }
}
