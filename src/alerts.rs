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
        let side = |p: Option<f64>, level: f64| p.map(|p| p.total_cmp(&level));
        let before = side(alert.last, alert.level);
        let decimals = price_decimals(0.0, alert.level, Some(instrument.kind));
        if let Some(hit) = alert.check(price, now, decimals) {
            fired.push(hit);
            changed = true;
        } else if side(alert.last, alert.level) != before {
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

/// The levels of the armed alerts on a symbol, for a chart to draw.
pub fn levels(store: &Store, symbol: &str, suffix: Option<&str>) -> Vec<f64> {
    store
        .alerts()
        .into_iter()
        .filter(|a| a.is_armed() && a.is_for(symbol, suffix))
        .map(|a| match a.drawing.and_then(|id| store.drawing_by_id(id)) {
            Some((_, _, drawing)) => drawing.from.price,
            None => a.level,
        })
        .collect()
}
