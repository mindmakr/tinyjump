//! `Time`: calendar arithmetic on UTC milliseconds (docs/language.md), on the `time` crate.
//! Times stay plain numbers everywhere (fields, JSON, the HTTP API); these functions only
//! compute. Showing a time in the viewer's language and zone is the host's job (`<Time>`),
//! as is picking one (`<Input type="date">`).

use super::value::{Exc, Value};
use crate::runtime::Runtime;
use std::rc::Rc;
use time::{Date, Duration, Month, OffsetDateTime, Time as Clock};

const MS_PER_DAY: f64 = 86_400_000.0;

fn at(v: &Value, what: &str) -> Result<OffsetDateTime, Exc> {
    let Value::Num(ms) = v else { return Err(Exc::msg(format!("{what} expects a time in milliseconds, got {}", v.type_name()))) };
    if !ms.is_finite() {
        return Err(Exc::msg(format!("{what} expects a time in milliseconds, got {ms}")));
    }
    OffsetDateTime::from_unix_timestamp_nanos(*ms as i128 * 1_000_000).map_err(|_| Exc::msg(format!("{what}: {ms} is outside the years -9999 to 9999")))
}

fn ms_of(t: OffsetDateTime) -> Value {
    Value::Num((t.unix_timestamp_nanos() / 1_000_000) as f64)
}

/// A whole number from a record field (0 when absent).
fn field(r: &Value, name: &str, what: &str) -> Result<i64, Exc> {
    let Value::Rec(r) = r else { return Ok(0) };
    match r.borrow().get(name) {
        None | Some(Value::Null) => Ok(0),
        Some(Value::Num(n)) if n.fract() == 0.0 && n.abs() < 1e12 => Ok(*n as i64),
        Some(v) => Err(Exc::msg(format!("{what}: `{name}` must be a whole number, got {}", v.display()))),
    }
}

/// The same day of the month `months` later, or the month's last day when it is shorter
/// (January 31 plus one month is the last day of February).
fn add_months(d: Date, months: i64) -> Result<Date, Exc> {
    let total = d.year() as i64 * 12 + (d.month() as i64 - 1) + months;
    let (year, month) = (total.div_euclid(12), total.rem_euclid(12) as u8 + 1);
    let month = Month::try_from(month).map_err(|_| Exc::msg("Time.add: month out of range"))?;
    let year = i32::try_from(year).map_err(|_| Exc::msg("Time.add: year out of range"))?;
    let day = d.day().min(month.length(year));
    Date::from_calendar_date(year, month, day).map_err(|_| Exc::msg("Time.add: the result is outside the years -9999 to 9999"))
}

/// `Time.add(ms, { years, months, weeks, days, hours, minutes, seconds })`.
pub fn add(_: &mut Runtime, a: &[Value]) -> Result<Value, Exc> {
    let t = at(a.first().unwrap_or(&Value::Null), "Time.add")?;
    let by = a.get(1).unwrap_or(&Value::Null);
    let months = field(by, "years", "Time.add")? * 12 + field(by, "months", "Time.add")?;
    let date = add_months(t.date(), months)?;
    let days = field(by, "weeks", "Time.add")? * 7 + field(by, "days", "Time.add")?;
    let rest = Duration::days(days) + Duration::hours(field(by, "hours", "Time.add")?) + Duration::minutes(field(by, "minutes", "Time.add")?) + Duration::seconds(field(by, "seconds", "Time.add")?);
    let out = t.replace_date(date).checked_add(rest).ok_or_else(|| Exc::msg("Time.add: the result is outside the years -9999 to 9999"))?;
    Ok(ms_of(out))
}

/// `Time.startOf(ms, "minute" | "hour" | "day" | "week" | "month" | "year")`, in UTC; weeks
/// start on Monday.
pub fn start_of(_: &mut Runtime, a: &[Value]) -> Result<Value, Exc> {
    let t = at(a.first().unwrap_or(&Value::Null), "Time.startOf")?;
    let unit = match a.get(1) {
        Some(Value::Str(u)) => u.clone(),
        _ => return Err(Exc::msg("Time.startOf needs a unit: \"minute\", \"hour\", \"day\", \"week\", \"month\" or \"year\"")),
    };
    let midnight = |d: Date| d.midnight().assume_utc();
    let d = t.date();
    let out = match &*unit {
        "minute" => t.replace_time(Clock::from_hms(t.hour(), t.minute(), 0).expect("valid")),
        "hour" => t.replace_time(Clock::from_hms(t.hour(), 0, 0).expect("valid")),
        "day" => midnight(d),
        "week" => midnight(d - Duration::days(d.weekday().number_days_from_monday() as i64)),
        "month" => midnight(d.replace_day(1).expect("every month has a first day")),
        "year" => midnight(Date::from_calendar_date(d.year(), Month::January, 1).expect("every year has a January 1")),
        other => return Err(Exc::msg(format!("Time.startOf: unknown unit \"{other}\"; use \"minute\", \"hour\", \"day\", \"week\", \"month\" or \"year\""))),
    };
    Ok(ms_of(out))
}

/// `Time.parts(ms)`: `{ year, month, day, hour, minute, second, weekday }` in UTC; months
/// count from 1, and weekdays from 1 (Monday) to 7 (Sunday).
pub fn parts(_: &mut Runtime, a: &[Value]) -> Result<Value, Exc> {
    let t = at(a.first().unwrap_or(&Value::Null), "Time.parts")?;
    let n = |k: &str, v: i64| (Rc::from(k), Value::Num(v as f64));
    Ok(Value::rec(
        [
            n("year", t.year() as i64),
            n("month", t.month() as i64),
            n("day", t.day() as i64),
            n("hour", t.hour() as i64),
            n("minute", t.minute() as i64),
            n("second", t.second() as i64),
            n("weekday", t.weekday().number_from_monday() as i64),
        ]
        .into_iter()
        .collect(),
    ))
}

/// `Time.from({ year, month, day, hour?, minute?, second? })`: the time in UTC, or `null` when
/// the parts are not a real date and time (February 30, 24:00).
pub fn from(_: &mut Runtime, a: &[Value]) -> Result<Value, Exc> {
    let p = a.first().unwrap_or(&Value::Null);
    let (year, month, day) = (field(p, "year", "Time.from")?, field(p, "month", "Time.from")?, field(p, "day", "Time.from")?);
    let (hour, minute, second) = (field(p, "hour", "Time.from")?, field(p, "minute", "Time.from")?, field(p, "second", "Time.from")?);
    let date = (|| {
        let month = Month::try_from(u8::try_from(month).ok()?).ok()?;
        let date = Date::from_calendar_date(i32::try_from(year).ok()?, month, u8::try_from(day).ok()?).ok()?;
        let clock = Clock::from_hms(u8::try_from(hour).ok()?, u8::try_from(minute).ok()?, u8::try_from(second).ok()?).ok()?;
        Some(date.with_time(clock).assume_utc())
    })();
    Ok(date.map_or(Value::Null, ms_of))
}

/// How many whole days from one time to another (negative when `b` is earlier), counting
/// calendar days in UTC.
pub fn days_between(_: &mut Runtime, a: &[Value]) -> Result<Value, Exc> {
    let x = at(a.first().unwrap_or(&Value::Null), "Time.daysBetween")?;
    let y = at(a.get(1).unwrap_or(&Value::Null), "Time.daysBetween")?;
    let (dx, dy) = (x.date().midnight().assume_utc(), y.date().midnight().assume_utc());
    Ok(Value::Num(((dy - dx).whole_milliseconds() as f64 / MS_PER_DAY).round()))
}
