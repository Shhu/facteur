//! How an event repeats, in the terms the event form offers (every day, week, month or year,
//! every n, until a date or so many times), and its translation to RRULE (RFC 5545, Google,
//! CalDAV) and to Graph's recurrence (Outlook). A rule facteur cannot put in these terms
//! (BYSETPOS, several days of the month…) reads as `None`: changed only in its own app.

use chrono::{Datelike, NaiveDate, NaiveDateTime, TimeZone};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repeat {
    pub freq: Freq,
    pub interval: u32,
    /// Weekly: the days, 0 for Monday to 6 for Sunday; empty: the start's day.
    #[serde(default)]
    pub weekdays: Vec<u8>,
    /// Monthly: on the start's weekday of its week of the month (the 3rd Tuesday, the last
    /// Friday) rather than on its day of the month.
    #[serde(default)]
    pub by_weekday: bool,
    /// The last day it may fall on, `YYYY-MM-DD`.
    #[serde(default)]
    pub until: Option<String>,
    /// How many times in all.
    #[serde(default)]
    pub count: Option<u32>,
}

const CODES: [&str; 7] = ["MO", "TU", "WE", "TH", "FR", "SA", "SU"];
const NAMES: [&str; 7] = ["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"];
const INDEXES: [&str; 5] = ["first", "second", "third", "fourth", "last"];

/// The start's week of the month: 1 to 4, or -1 for the last one (a 5th weekday is the last).
fn week_of_month(d: NaiveDate) -> i32 {
    let n = (d.day() as i32 - 1) / 7 + 1;
    if n == 5 { -1 } else { n }
}

fn until_date(until: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(until.trim(), "%Y-%m-%d").ok()
}

impl Repeat {
    fn days(&self, start: NaiveDate) -> Vec<u8> {
        let mut days: Vec<u8> = self.weekdays.iter().copied().filter(|d| *d < 7).collect();
        if days.is_empty() {
            days.push(start.weekday().num_days_from_monday() as u8);
        }
        days.sort_unstable();
        days.dedup();
        days
    }

    /// The RRULE value for a series starting on `start`. `end_of_until`: the instant the last
    /// day ends at, for a timed series (UNTIL is then in UTC); `None` for whole days.
    pub fn rrule(&self, start: NaiveDate, end_of_until: Option<&dyn Fn(NaiveDate) -> Option<i64>>) -> String {
        let mut parts = vec![format!(
            "FREQ={}",
            match self.freq {
                Freq::Daily => "DAILY",
                Freq::Weekly => "WEEKLY",
                Freq::Monthly => "MONTHLY",
                Freq::Yearly => "YEARLY",
            }
        )];
        if self.interval > 1 {
            parts.push(format!("INTERVAL={}", self.interval));
        }
        match self.freq {
            Freq::Weekly => parts.push(format!("BYDAY={}", self.days(start).iter().map(|d| CODES[*d as usize]).collect::<Vec<_>>().join(","))),
            Freq::Monthly if self.by_weekday => {
                parts.push(format!("BYDAY={}{}", week_of_month(start), CODES[start.weekday().num_days_from_monday() as usize]))
            }
            Freq::Monthly => parts.push(format!("BYMONTHDAY={}", start.day())),
            Freq::Yearly => parts.push(format!("BYMONTH={};BYMONTHDAY={}", start.month(), start.day())),
            Freq::Daily => {}
        }
        if let Some(count) = self.count.filter(|n| *n > 0) {
            parts.push(format!("COUNT={count}"));
        } else if let Some(until) = self.until.as_deref().and_then(until_date) {
            match end_of_until.and_then(|end| end(until)).and_then(|t| chrono::DateTime::from_timestamp(t, 0)) {
                Some(at) => parts.push(format!("UNTIL={}", at.format("%Y%m%dT%H%M%SZ"))),
                None => parts.push(format!("UNTIL={}", until.format("%Y%m%d"))),
            }
        }
        parts.join(";")
    }

    /// The rule in the form's terms, for a series starting on `start`; `None` when it says more
    /// than they can (it then stays as it is).
    pub fn from_rrule(value: &str, start: NaiveDate) -> Option<Repeat> {
        let mut freq = None;
        let mut out = Repeat { freq: Freq::Daily, interval: 1, weekdays: Vec::new(), by_weekday: false, until: None, count: None };
        let (mut by_day, mut by_month_day, mut by_month) = (None, None, None);
        for part in value.trim().trim_start_matches("RRULE:").split(';').filter(|p| !p.is_empty()) {
            let (k, v) = part.split_once('=')?;
            let v = v.trim().to_ascii_uppercase();
            match k.trim().to_ascii_uppercase().as_str() {
                "FREQ" => {
                    freq = Some(match v.as_str() {
                        "DAILY" => Freq::Daily,
                        "WEEKLY" => Freq::Weekly,
                        "MONTHLY" => Freq::Monthly,
                        "YEARLY" => Freq::Yearly,
                        _ => return None,
                    })
                }
                "INTERVAL" => out.interval = v.parse().ok().filter(|n| *n > 0)?,
                "COUNT" => out.count = Some(v.parse().ok()?),
                "UNTIL" => {
                    let date = NaiveDate::parse_from_str(v.get(..8)?, "%Y%m%d").ok()?;
                    // A UTC time: its day where the computer is.
                    let date = match NaiveDateTime::parse_from_str(v.trim_end_matches('Z'), "%Y%m%dT%H%M%S") {
                        Ok(t) if v.ends_with('Z') => t.and_utc().with_timezone(&chrono::Local).date_naive(),
                        _ => date,
                    };
                    out.until = Some(date.format("%Y-%m-%d").to_string());
                }
                "BYDAY" => by_day = Some(v),
                "BYMONTHDAY" => by_month_day = Some(v),
                "BYMONTH" => by_month = Some(v),
                "WKST" => {}
                _ => return None,
            }
        }
        out.freq = freq?;
        let code = |d: &str| CODES.iter().position(|c| *c == d).map(|i| i as u8);
        match out.freq {
            Freq::Daily if by_day.is_none() && by_month_day.is_none() && by_month.is_none() => {}
            Freq::Weekly if by_month_day.is_none() && by_month.is_none() => {
                if let Some(days) = by_day {
                    out.weekdays = days.split(',').map(|d| code(d.trim())).collect::<Option<_>>()?;
                }
            }
            Freq::Monthly if by_month.is_none() => match (by_day, by_month_day) {
                (Some(day), None) => {
                    let wanted = format!("{}{}", week_of_month(start), CODES[start.weekday().num_days_from_monday() as usize]);
                    (day.trim_start_matches('+') == wanted).then_some(())?;
                    out.by_weekday = true;
                }
                (None, Some(d)) => (d.trim().parse::<u32>().ok()? == start.day()).then_some(())?,
                (None, None) => {}
                _ => return None,
            },
            Freq::Yearly if by_day.is_none() => {
                if by_month.is_some_and(|m| m.trim().parse::<u32>().ok() != Some(start.month()))
                    || by_month_day.is_some_and(|d| d.trim().parse::<u32>().ok() != Some(start.day()))
                {
                    return None;
                }
            }
            _ => return None,
        }
        Some(out)
    }

    /// Graph's `patternedRecurrence`, for a series whose first day is `start`.
    pub fn graph(&self, start: NaiveDate, zone: &str) -> serde_json::Value {
        let weekday = NAMES[start.weekday().num_days_from_monday() as usize];
        let pattern = match self.freq {
            Freq::Daily => serde_json::json!({ "type": "daily", "interval": self.interval }),
            Freq::Weekly => serde_json::json!({
                "type": "weekly", "interval": self.interval, "firstDayOfWeek": "monday",
                "daysOfWeek": self.days(start).iter().map(|d| NAMES[*d as usize]).collect::<Vec<_>>(),
            }),
            Freq::Monthly if self.by_weekday => {
                let n = week_of_month(start);
                serde_json::json!({
                    "type": "relativeMonthly", "interval": self.interval, "daysOfWeek": [weekday],
                    "index": INDEXES[if n < 0 { 4 } else { n as usize - 1 }],
                })
            }
            Freq::Monthly => serde_json::json!({ "type": "absoluteMonthly", "interval": self.interval, "dayOfMonth": start.day() }),
            Freq::Yearly => serde_json::json!({ "type": "absoluteYearly", "interval": self.interval, "dayOfMonth": start.day(), "month": start.month() }),
        };
        let first = start.format("%Y-%m-%d").to_string();
        let range = match (self.count.filter(|n| *n > 0), self.until.as_deref().and_then(until_date)) {
            (Some(n), _) => serde_json::json!({ "type": "numbered", "startDate": first, "numberOfOccurrences": n, "recurrenceTimeZone": zone }),
            (None, Some(until)) => serde_json::json!({ "type": "endDate", "startDate": first, "endDate": until.format("%Y-%m-%d").to_string(), "recurrenceTimeZone": zone }),
            (None, None) => serde_json::json!({ "type": "noEnd", "startDate": first, "recurrenceTimeZone": zone }),
        };
        serde_json::json!({ "pattern": pattern, "range": range })
    }

    /// Graph's recurrence in the form's terms (see [`Repeat::from_rrule`]).
    pub fn from_graph(recurrence: &serde_json::Value, start: NaiveDate) -> Option<Repeat> {
        let pattern = &recurrence["pattern"];
        let range = &recurrence["range"];
        let interval = pattern["interval"].as_u64().filter(|n| *n > 0)? as u32;
        let names = |v: &serde_json::Value| -> Option<Vec<u8>> {
            v.as_array()?.iter().map(|d| NAMES.iter().position(|n| Some(*n) == d.as_str()).map(|i| i as u8)).collect()
        };
        let (freq, weekdays, by_weekday) = match pattern["type"].as_str()? {
            "daily" => (Freq::Daily, Vec::new(), false),
            "weekly" => (Freq::Weekly, names(&pattern["daysOfWeek"])?, false),
            "absoluteMonthly" => {
                (pattern["dayOfMonth"].as_u64() == Some(u64::from(start.day()))).then_some(())?;
                (Freq::Monthly, Vec::new(), false)
            }
            "relativeMonthly" => {
                let n = week_of_month(start);
                let index = INDEXES[if n < 0 { 4 } else { n as usize - 1 }];
                (pattern["index"].as_str() == Some(index) && names(&pattern["daysOfWeek"])? == vec![start.weekday().num_days_from_monday() as u8])
                    .then_some(())?;
                (Freq::Monthly, Vec::new(), true)
            }
            "absoluteYearly" => {
                (pattern["dayOfMonth"].as_u64() == Some(u64::from(start.day())) && pattern["month"].as_u64() == Some(u64::from(start.month())))
                    .then_some(())?;
                (Freq::Yearly, Vec::new(), false)
            }
            _ => return None,
        };
        let (until, count) = match range["type"].as_str()? {
            "noEnd" => (None, None),
            "endDate" => (Some(range["endDate"].as_str()?.to_string()), None),
            "numbered" => (None, Some(range["numberOfOccurrences"].as_u64()? as u32)),
            _ => return None,
        };
        Some(Repeat { freq, interval, weekdays, by_weekday, until, count })
    }
}

/// The computer's zone, by IANA name (written into repeating events, whose dates must keep their
/// hour across changes of time); `None` when it has none that chrono-tz knows.
pub fn local_zone() -> Option<(String, chrono_tz::Tz)> {
    let name = iana_time_zone::get_timezone().ok()?;
    let tz: chrono_tz::Tz = name.parse().ok()?;
    Some((name, tz))
}

/// The instant a local day ends at in `tz` (its last second), for a timed series' UNTIL.
pub fn end_of_day(tz: chrono_tz::Tz, day: NaiveDate) -> Option<i64> {
    tz.from_local_datetime(&day.and_hms_opt(23, 59, 59)?).earliest().map(|d| d.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn writes_and_reads_rules() {
        // Tuesday 20 October 2026, the 3rd Tuesday of the month.
        let start = day(2026, 10, 20);
        let weekly = Repeat { freq: Freq::Weekly, interval: 2, weekdays: vec![1, 3], by_weekday: false, until: None, count: Some(10) };
        assert_eq!(weekly.rrule(start, None), "FREQ=WEEKLY;INTERVAL=2;BYDAY=TU,TH;COUNT=10");
        assert_eq!(Repeat::from_rrule("RRULE:FREQ=WEEKLY;INTERVAL=2;BYDAY=TU,TH;COUNT=10", start), Some(weekly));

        let monthly = Repeat { freq: Freq::Monthly, interval: 1, weekdays: vec![], by_weekday: true, until: Some("2027-06-30".into()), count: None };
        assert_eq!(monthly.rrule(start, None), "FREQ=MONTHLY;BYDAY=3TU;UNTIL=20270630");
        assert_eq!(Repeat::from_rrule("FREQ=MONTHLY;BYDAY=3TU;UNTIL=20270630", start), Some(monthly));
        // The last Friday of October 2026 (the 30th, a 5th Friday) is said as the last one.
        let last = Repeat { freq: Freq::Monthly, interval: 1, weekdays: vec![], by_weekday: true, until: None, count: None };
        assert_eq!(last.rrule(day(2026, 10, 30), None), "FREQ=MONTHLY;BYDAY=-1FR");

        assert_eq!(Repeat::from_rrule("FREQ=YEARLY", start).map(|r| r.freq), Some(Freq::Yearly));
        assert_eq!(Repeat::from_rrule("FREQ=DAILY;INTERVAL=3", start).map(|r| r.interval), Some(3));
        // More than the form can say: left as it is.
        assert_eq!(Repeat::from_rrule("FREQ=MONTHLY;BYDAY=TU;BYSETPOS=2", start), None);
        assert_eq!(Repeat::from_rrule("FREQ=MONTHLY;BYMONTHDAY=1,15", start), None);
        assert_eq!(Repeat::from_rrule("FREQ=MONTHLY;BYDAY=2TU", start), None, "not the start's week");
    }

    #[test]
    fn a_timed_series_ends_at_the_end_of_its_last_day() {
        let start = day(2026, 10, 20);
        let daily = Repeat { freq: Freq::Daily, interval: 1, weekdays: vec![], by_weekday: false, until: Some("2026-12-31".into()), count: None };
        let paris = |d: NaiveDate| end_of_day(chrono_tz::Europe::Paris, d);
        assert_eq!(daily.rrule(start, Some(&paris)), "FREQ=DAILY;UNTIL=20261231T225959Z");
    }

    #[test]
    fn graph_recurrence_both_ways() {
        let start = day(2026, 10, 20);
        let weekly = Repeat { freq: Freq::Weekly, interval: 1, weekdays: vec![0, 1], by_weekday: false, until: Some("2027-01-31".into()), count: None };
        let json = weekly.graph(start, "Europe/Paris");
        assert_eq!(json["pattern"]["daysOfWeek"], serde_json::json!(["monday", "tuesday"]));
        assert_eq!((json["range"]["type"].as_str(), json["range"]["endDate"].as_str()), (Some("endDate"), Some("2027-01-31")));
        assert_eq!(Repeat::from_graph(&json, start), Some(weekly));
        let monthly = Repeat { freq: Freq::Monthly, interval: 1, weekdays: vec![], by_weekday: true, until: None, count: Some(5) };
        let json = monthly.graph(start, "Europe/Paris");
        assert_eq!((json["pattern"]["type"].as_str(), json["pattern"]["index"].as_str()), (Some("relativeMonthly"), Some("third")));
        assert_eq!(Repeat::from_graph(&json, start), Some(monthly));
    }
}
