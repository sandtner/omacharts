//! Price alerts, checked wherever a fresh price turns up.
//!
//! There is no timer here, and there must not be one: the app is told about
//! prices, it does not go looking. Three things bring one in, and each calls
//! into this module with what it has:
//!
//! - the bar widget's poll, `omacharts watchlist --refresh`, every couple of
//!   minutes with or without a window — which is what makes an alert work
//!   with the app closed;
//! - a loader reply in the window, a batch of closes or a chart's bars;
//! - a tick off a live stream, which is what makes an alert on a chart that
//!   is streaming fire on the tick that crosses it.
//!
//! A fired alert is written down at once, so whichever of the three gets there
//! first is the only one that notifies.

use omacharts_engine::alert::{Alert, Fired};
use omacharts_engine::{price_decimals, Instrument, Provider, SearchIndex};

use crate::store::Store;

/// The instruments the alerts that are still armed are on: what a refresh
/// has to fetch, beside the watchlist, for them to be checked at all.
pub fn instruments(store: &Store, index: &SearchIndex) -> Vec<Instrument> {
    let mut seen = std::collections::HashSet::new();
    store
        .alerts()
        .into_iter()
        .filter(Alert::is_armed)
        .filter(|a| seen.insert((a.symbol.clone(), a.suffix.clone())))
        .filter_map(|a| index.find(&a.symbol, a.suffix.as_deref()).cloned())
        .collect()
}

/// Check the armed alerts against the latest cached quote of each, which is
/// what a refresh has just written. `keys`, when given, limits the check to
/// alerts on those cache keys — the ones a reply brought prices for.
pub fn check_quotes(
    store: &Store,
    provider: &dyn Provider,
    index: &SearchIndex,
    keys: Option<&[String]>,
) -> Vec<Fired> {
    check(store, provider, index, |key, instrument| {
        if keys.is_some_and(|keys| !keys.iter().any(|k| k == key)) {
            return None;
        }
        crate::cli::quote(store, provider, instrument).map(|q| q.last)
    })
}

/// Check the armed alerts on one cache key against a price just seen there:
/// a live tick, or the newest bar of a reply.
pub fn check_price(
    store: &Store,
    provider: &dyn Provider,
    index: &SearchIndex,
    key: &str,
    price: f64,
) -> Vec<Fired> {
    check(store, provider, index, |k, _| (k == key).then_some(price))
}

/// Put a price to every armed alert the closure has one for, write down what
/// changed, and notify what fired.
fn check(
    store: &Store,
    provider: &dyn Provider,
    index: &SearchIndex,
    price_of: impl Fn(&str, &Instrument) -> Option<f64>,
) -> Vec<Fired> {
    let mut alerts = store.alerts();
    if !alerts.iter().any(Alert::is_armed) {
        return Vec::new();
    }
    let now = chrono::Utc::now().timestamp();
    let mut fired = Vec::new();
    let mut changed = false;
    for alert in alerts.iter_mut().filter(|a| a.is_armed()) {
        let Some(instrument) = index.find(&alert.symbol, alert.suffix.as_deref()) else { continue };
        let Some(key) = crate::cli::cache_key(provider, instrument) else { continue };
        let Some(price) = price_of(&key, instrument) else { continue };
        // An alert on a line follows the line. A line that is gone takes
        // the alert's level with it, so it stays where it last was.
        if let Some(id) = alert.drawing
            && let Some((_, _, drawing)) = store.drawing_by_id(id)
            && drawing.from.price != alert.level
        {
            alert.level = drawing.from.price;
            changed = true;
        }
        // Which side of each bound: a channel has two, a level one twice.
        let side = |p: Option<f64>, (lo, hi): (f64, f64)| p.map(|p| (p.total_cmp(&lo), p.total_cmp(&hi)));
        let before = side(alert.last, alert.bounds());
        let decimals = price_decimals(0.0, alert.level, Some(instrument.kind));
        if let Some(hit) = alert.check(price, now, decimals) {
            fired.push(hit);
            changed = true;
        } else if side(alert.last, alert.bounds()) != before {
            // Written only when the price changes sides of the level, which
            // is all a crossing is measured by: a tick a second would
            // otherwise be a write a second.
            changed = true;
        }
    }
    if changed {
        store.set_alerts(&alerts);
    }
    for hit in &fired {
        notify(hit);
    }
    fired
}

/// Tell the desktop. Fire and forget: no notification daemon is not a
/// reason for a check to fail, and the alert is written down as fired
/// either way.
pub fn notify(fired: &Fired) {
    let _ = std::process::Command::new("notify-send")
        .args([
            "--app-name=Omacharts",
            "--icon=com.jorgemanrubia.Omacharts",
            "--urgency=critical",
            "--category=omacharts.alert",
            &fired.title,
            &fired.body,
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// The levels of the armed alerts on a symbol, for a chart to draw: both
/// bounds of a channel.
pub fn levels(store: &Store, symbol: &str, suffix: Option<&str>) -> Vec<f64> {
    store
        .alerts()
        .into_iter()
        .filter(|a| a.is_armed() && a.is_for(symbol, suffix))
        .flat_map(|a| {
            let level = match a.drawing.and_then(|id| store.drawing_by_id(id)) {
                Some((_, _, drawing)) => drawing.from.price,
                None => a.level,
            };
            std::iter::once(level).chain(a.upper.filter(|_| a.condition.is_channel()))
        })
        .collect()
}

/// When an alert stops, from how a person says it: `never`, `90m`, `12h`,
/// `3d`, `2w`, `2026-12-31` (the end of that day, local time) or
/// `2026-12-31T16:00` / `2026-12-31 16:00` (local time). `None` is never.
pub fn parse_expiry(text: &str, now: i64) -> Result<Option<i64>, String> {
    use chrono::{Local, NaiveDate, NaiveDateTime, TimeZone};
    let text = text.trim();
    if text.is_empty() || text.eq_ignore_ascii_case("never") || text.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    let bad = || format!("an expiry is never, 90m, 12h, 3d, 2w, 2026-12-31 or 2026-12-31T16:00, not {text:?}");
    if let Some(unit) = text.chars().last().filter(char::is_ascii_alphabetic) {
        let count: i64 = text[..text.len() - 1].trim().parse().map_err(|_| bad())?;
        let seconds = match unit.to_ascii_lowercase() {
            'm' => 60,
            'h' => 3600,
            'd' => 86_400,
            'w' => 7 * 86_400,
            _ => return Err(bad()),
        };
        if count <= 0 {
            return Err(bad());
        }
        return Ok(Some(now + count * seconds));
    }
    let local = |at: NaiveDateTime| Local.from_local_datetime(&at).earliest().map(|t| t.timestamp());
    let at = ["%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M"]
        .iter()
        .find_map(|f| NaiveDateTime::parse_from_str(text, f).ok())
        .and_then(local)
        .or_else(|| {
            NaiveDate::parse_from_str(text, "%Y-%m-%d").ok().and_then(|d| d.and_hms_opt(23, 59, 59)).and_then(local)
        })
        .ok_or_else(bad)?;
    if at <= now {
        return Err(format!("{text} is already past"));
    }
    Ok(Some(at))
}

/// An expiry as the editor shows it and [`parse_expiry`] reads it back.
pub fn spell_expiry(at: i64) -> String {
    use chrono::{Local, TimeZone};
    Local.timestamp_opt(at, 0).single().map(|t| t.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default()
}

/// `AAPL` or `NESN.SW` into a symbol and a suffix, through the catalogue so
/// that a ticker with a dot in it is not split.
pub fn resolve_symbol(index: &SearchIndex, text: &str) -> Option<(String, Option<String>)> {
    let upper = text.trim().to_uppercase();
    if upper.is_empty() {
        return None;
    }
    if let Some(found) = index.find(&upper, None) {
        return Some((found.symbol.clone(), found.suffix.clone()));
    }
    let (symbol, suffix) = upper.rsplit_once('.')?;
    let found = index.find(symbol, Some(suffix))?;
    Some((found.symbol.clone(), found.suffix.clone()))
}

/// Every firing, newest first, as the alert and the firing. One from before
/// there was a log is still a firing, with no price.
pub fn hits(alerts: &[Alert]) -> Vec<(&Alert, omacharts_engine::alert::Hit)> {
    use omacharts_engine::alert::Hit;
    let mut hits: Vec<_> = alerts
        .iter()
        .flat_map(|a| {
            let old = a.log.is_empty().then_some(a.fired).flatten().map(|at| Hit { at, price: f64::NAN });
            a.log.iter().copied().chain(old).map(move |h| (a, h))
        })
        .collect();
    hits.sort_by_key(|(_, x)| std::cmp::Reverse(x.at));
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expiries_read_the_way_people_write_them() {
        assert_eq!(parse_expiry("never", 0), Ok(None));
        assert_eq!(parse_expiry("90m", 100), Ok(Some(100 + 90 * 60)));
        assert_eq!(parse_expiry("2w", 0), Ok(Some(14 * 86_400)));
        assert!(parse_expiry("3x", 0).is_err());
        assert!(parse_expiry("-1d", 0).is_err());
        assert!(parse_expiry("2000-01-01", 1_900_000_000).is_err(), "past");
        let at = parse_expiry("2099-12-31 16:00", 0).unwrap().unwrap();
        assert_eq!(spell_expiry(at), "2099-12-31 16:00");
        assert_eq!(parse_expiry("2099-12-31T16:00", 0).unwrap(), Some(at));
        assert!(parse_expiry("2099-12-31", 0).unwrap().unwrap() > at);
    }
}
