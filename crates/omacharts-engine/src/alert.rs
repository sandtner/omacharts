//! Price alerts: a level on a symbol, and the rule that says when a price has
//! reached it.
//!
//! Nothing here fetches or notifies. The app asks [`Alert::check`] with the
//! freshest price it has — a quote the bar widget's poll brought in, a bar a
//! loader reply landed, a tick off a live stream — and acts on the answer.
//! Keeping the rule in one pure function is what lets the window and a
//! process with no window agree on exactly when an alert fires.

use serde::{Deserialize, Serialize};

/// The settings key the alerts are stored under, as one JSON list.
pub const SETTING: &str = "alerts";

/// What a price has to do to fire an alert.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Condition {
    /// At or above the level, however it got there.
    Above,
    /// At or below the level.
    Below,
    /// Through the level, either way, since the last price seen. The one to
    /// put on a line on the chart: a line moved to the other side of the
    /// price keeps meaning "tell me when it gets here".
    Cross,
}

impl Condition {
    pub const ALL: [Condition; 3] = [Condition::Above, Condition::Below, Condition::Cross];

    pub fn key(self) -> &'static str {
        match self {
            Condition::Above => "above",
            Condition::Below => "below",
            Condition::Cross => "cross",
        }
    }

    pub fn from_key(key: &str) -> Option<Condition> {
        Condition::ALL.into_iter().find(|c| c.key().eq_ignore_ascii_case(key))
    }

    /// How a fired alert says what happened: `rose to`, `crossed`.
    fn verb(self, from: Option<f64>, level: f64) -> &'static str {
        match self {
            Condition::Above => "rose to",
            Condition::Below => "fell to",
            Condition::Cross => match from {
                Some(from) if from > level => "fell through",
                _ => "rose through",
            },
        }
    }
}

/// One alert on one symbol.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Alert {
    pub id: i64,
    pub symbol: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
    pub condition: Condition,
    /// The price. For an alert on a drawing this is where the line was when
    /// last looked at; the line is what counts, see [`Alert::drawing`].
    pub level: f64,
    /// A horizontal line this alert follows: move the line and the alert
    /// moves with it. The drawing's id in the store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drawing: Option<i64>,
    /// Words to put in the notification.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// When it was set, in seconds.
    pub created: i64,
    /// The last price it was checked against, which is what a crossing is
    /// measured from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<f64>,
    /// When it fired. A fired alert is done until it is armed again: a level
    /// that fires every two minutes while the price sits on it is noise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fired: Option<i64>,
}

/// What a check found.
#[derive(Clone, PartialEq, Debug)]
pub struct Fired {
    pub id: i64,
    /// `AAPL rose through 195.00`.
    pub title: String,
    /// `now 195.12 · breakout`.
    pub body: String,
}

impl Alert {
    pub fn new(symbol: &str, suffix: Option<&str>, condition: Condition, level: f64, created: i64) -> Alert {
        Alert {
            id: 0,
            symbol: symbol.to_string(),
            suffix: suffix.filter(|s| !s.is_empty()).map(str::to_string),
            condition,
            level,
            drawing: None,
            note: String::new(),
            created,
            last: None,
            fired: None,
        }
    }

    pub fn is_armed(&self) -> bool {
        self.fired.is_none()
    }

    /// The symbol as people write it: `AAPL`, `NESN.SW`.
    pub fn spell_symbol(&self) -> String {
        match &self.suffix {
            Some(suffix) => format!("{}.{suffix}", self.symbol),
            None => self.symbol.clone(),
        }
    }

    pub fn is_for(&self, symbol: &str, suffix: Option<&str>) -> bool {
        self.symbol == symbol && self.suffix.as_deref() == suffix.filter(|s| !s.is_empty())
    }

    /// Put a price to the alert, at `now`, and say whether that fired it.
    ///
    /// Remembers the price either way, so the next check measures a crossing
    /// from here. A crossing alert seeing its first price only remembers
    /// it: with nothing before, there is nothing to have crossed from.
    pub fn check(&mut self, price: f64, now: i64, decimals: usize) -> Option<Fired> {
        if !price.is_finite() || !self.is_armed() {
            return None;
        }
        let from = self.last.replace(price);
        let level = self.level;
        let hit = match self.condition {
            Condition::Above => price >= level,
            Condition::Below => price <= level,
            Condition::Cross => from.is_some_and(|from| {
                (from < level && price >= level) || (from > level && price <= level)
            }),
        };
        if !hit {
            return None;
        }
        self.fired = Some(now);
        let note = match self.note.is_empty() {
            true => String::new(),
            false => format!(" · {}", self.note),
        };
        Some(Fired {
            id: self.id,
            title: format!(
                "{} {} {:.decimals$}",
                self.spell_symbol(),
                self.condition.verb(from, level),
                level
            ),
            body: format!("now {price:.decimals$}{note}"),
        })
    }

    /// Ready to fire again, measuring from the next price rather than the
    /// one that fired it.
    pub fn rearm(&mut self) {
        self.fired = None;
        self.last = None;
    }

    /// One line: `#3 AAPL cross 195.00 (line #12) · armed · "breakout"`.
    pub fn describe(&self, decimals: usize) -> String {
        let line = self.drawing.map(|id| format!(" (line #{id})")).unwrap_or_default();
        let state = match self.fired {
            None => "armed".to_string(),
            Some(_) => "fired".to_string(),
        };
        let note = match self.note.is_empty() {
            true => String::new(),
            false => format!(" · {:?}", self.note),
        };
        format!(
            "#{} {} {} {:.decimals$}{line} · {state}{note}",
            self.id,
            self.spell_symbol(),
            self.condition.key(),
            self.level
        )
    }
}

/// Read the stored list; an unreadable row is no alerts rather than a
/// crash, and is left in place for somebody to look at until the next write.
pub fn from_json(text: Option<&str>) -> Vec<Alert> {
    text.and_then(|t| serde_json::from_str(t).ok()).unwrap_or_default()
}

pub fn to_json(alerts: &[Alert]) -> String {
    serde_json::to_string(alerts).unwrap_or_else(|_| "[]".into())
}

/// The id the next alert gets.
pub fn next_id(alerts: &[Alert]) -> i64 {
    alerts.iter().map(|a| a.id).max().unwrap_or(0) + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alert(condition: Condition, level: f64) -> Alert {
        Alert::new("AAPL", None, condition, level, 0)
    }

    #[test]
    fn above_and_below_fire_on_where_the_price_is() {
        let mut up = alert(Condition::Above, 100.0);
        assert!(up.check(99.0, 1, 2).is_none());
        let fired = up.check(100.5, 2, 2).expect("fires at the level");
        assert_eq!(fired.title, "AAPL rose to 100.00");
        assert_eq!(fired.body, "now 100.50");
        assert_eq!(up.fired, Some(2));
        let mut down = alert(Condition::Below, 100.0);
        assert!(down.check(99.0, 1, 2).is_some(), "already below fires straight away");
    }

    #[test]
    fn a_fired_alert_is_quiet_until_armed_again() {
        let mut up = alert(Condition::Above, 100.0);
        assert!(up.check(101.0, 1, 2).is_some());
        assert!(up.check(102.0, 2, 2).is_none());
        up.rearm();
        assert!(up.check(103.0, 3, 2).is_some());
    }

    #[test]
    fn a_crossing_needs_a_price_on_either_side() {
        let mut cross = alert(Condition::Cross, 100.0);
        assert!(cross.check(101.0, 1, 2).is_none(), "the first price is only remembered");
        assert!(cross.check(100.5, 2, 2).is_none());
        let fired = cross.check(99.9, 3, 2).expect("down through it");
        assert_eq!(fired.title, "AAPL fell through 100.00");
        let mut up = alert(Condition::Cross, 100.0);
        up.check(99.0, 1, 2);
        assert_eq!(up.check(100.0, 2, 2).map(|f| f.title), Some("AAPL rose through 100.00".into()));
    }

    #[test]
    fn a_note_and_a_suffix_reach_the_notification() {
        let mut a = Alert::new("NESN", Some("SW"), Condition::Above, 80.0, 0);
        a.note = "trim".into();
        let fired = a.check(81.0, 1, 2).unwrap();
        assert_eq!(fired.title, "NESN.SW rose to 80.00");
        assert_eq!(fired.body, "now 81.00 · trim");
        assert!(a.is_for("NESN", Some("SW")) && !a.is_for("NESN", None));
    }

    #[test]
    fn the_list_survives_the_store() {
        let mut a = alert(Condition::Cross, 100.0);
        a.id = 3;
        a.drawing = Some(12);
        let list = vec![a];
        assert_eq!(from_json(Some(&to_json(&list))), list);
        assert_eq!(from_json(Some("not json")), Vec::<Alert>::new());
        assert_eq!(next_id(&list), 4);
        assert_eq!(next_id(&[]), 1);
    }
}
