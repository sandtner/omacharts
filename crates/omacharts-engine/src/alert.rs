//! Price alerts: a level (or a channel) on a symbol, and the rule that says
//! when a price has reached it.
//!
//! Nothing here fetches or notifies. The app asks [`Alert::check`] with the
//! freshest price it has — a quote the bar widget's poll brought in, a bar a
//! loader reply landed, a tick off a live stream — and acts on the answer.
//! Keeping the rule in one pure function is what lets the window and a
//! process with no window agree on exactly when an alert fires.
//!
//! Every field added after the first release has a default, so a list
//! written by an older build reads as alerts that fire once, never expire
//! and are not paused — which is what they were.

use serde::{Deserialize, Serialize};

use crate::bars::{Bar, Timeframe};
use crate::condition::{Inputs, Rule};

/// The settings key the alerts are stored under, as one JSON list.
pub const SETTING: &str = "alerts";

/// How many firings an alert remembers, newest kept.
pub const LOG_LIMIT: usize = 50;

/// What a price has to do to fire an alert.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Condition {
    /// Through the level, either way, since the last price seen. The one to
    /// put on a line on the chart: a line moved to the other side of the
    /// price keeps meaning "tell me when it gets here".
    Cross,
    /// Through the level from below.
    CrossUp,
    /// Through the level from above.
    CrossDown,
    /// At or above the level, however it got there.
    Above,
    /// At or below the level.
    Below,
    /// Into the channel between the level and the upper bound, from outside.
    Enter,
    /// Out of the channel, either side, from inside.
    Exit,
}

impl Condition {
    pub const ALL: [Condition; 7] = [
        Condition::Cross,
        Condition::CrossUp,
        Condition::CrossDown,
        Condition::Above,
        Condition::Below,
        Condition::Enter,
        Condition::Exit,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Condition::Cross => "cross",
            Condition::CrossUp => "cross-up",
            Condition::CrossDown => "cross-down",
            Condition::Above => "above",
            Condition::Below => "below",
            Condition::Enter => "enter",
            Condition::Exit => "exit",
        }
    }

    /// How a person reads it in a list: `Crossing up`.
    pub fn label(self) -> &'static str {
        match self {
            Condition::Cross => "Crossing",
            Condition::CrossUp => "Crossing up",
            Condition::CrossDown => "Crossing down",
            Condition::Above => "At or above",
            Condition::Below => "At or below",
            Condition::Enter => "Entering channel",
            Condition::Exit => "Exiting channel",
        }
    }

    pub fn from_key(key: &str) -> Option<Condition> {
        let key = key.replace('_', "-");
        Condition::ALL.into_iter().find(|c| c.key().eq_ignore_ascii_case(&key))
    }

    /// A channel has two bounds; everything else one level.
    pub fn is_channel(self) -> bool {
        matches!(self, Condition::Enter | Condition::Exit)
    }

    /// Whether it needs a price before this one to mean anything: a crossing
    /// or an entry is a move, not a place.
    pub fn is_move(self) -> bool {
        !matches!(self, Condition::Above | Condition::Below)
    }

    /// How a fired alert says what happened: `rose to`, `crossed`.
    fn verb(self, from: Option<f64>, level: f64) -> &'static str {
        match self {
            Condition::Above => "rose to",
            Condition::Below => "fell to",
            Condition::CrossUp => "rose through",
            Condition::CrossDown => "fell through",
            Condition::Cross => match from {
                Some(from) if from > level => "fell through",
                _ => "rose through",
            },
            Condition::Enter => "entered",
            Condition::Exit => "left",
        }
    }
}

/// How often an alert may fire.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Frequency {
    /// Fire, then wait to be armed again.
    #[default]
    Once,
    /// Fire each time the condition becomes true, and stay armed. A level
    /// condition fires on getting there, not on every price while there.
    Every,
}

impl Frequency {
    pub const ALL: [Frequency; 2] = [Frequency::Once, Frequency::Every];

    pub fn key(self) -> &'static str {
        match self {
            Frequency::Once => "once",
            Frequency::Every => "every",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Frequency::Once => "Only once",
            Frequency::Every => "Every time",
        }
    }

    pub fn from_key(key: &str) -> Option<Frequency> {
        Frequency::ALL.into_iter().find(|f| f.key().eq_ignore_ascii_case(key))
    }
}

/// One firing, for the log.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Hit {
    /// When, in seconds.
    pub at: i64,
    /// The price that fired it.
    pub price: f64,
}

/// Where an alert stands, for a list to show and filter on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Armed,
    Paused,
    Fired,
    Expired,
}

impl State {
    pub fn key(self) -> &'static str {
        match self {
            State::Armed => "armed",
            State::Paused => "paused",
            State::Fired => "fired",
            State::Expired => "expired",
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
    /// The price, or the channel's first bound. For an alert on a drawing
    /// this is where the line was when last looked at; the line is what
    /// counts, see [`Alert::drawing`].
    pub level: f64,
    /// The channel's other bound, for [`Condition::is_channel`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upper: Option<f64>,
    /// A horizontal line this alert follows: move the line and the alert
    /// moves with it. The drawing's id in the store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drawing: Option<i64>,
    /// What the alert is called in the list, and at the front of the
    /// notification.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Words to put in the notification. `{{ticker}}`, `{{price}}` and
    /// `{{level}}` are put in as it fires.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    #[serde(default, skip_serializing_if = "is_once")]
    pub frequency: Frequency,
    /// When it stops, in seconds. An expired alert stays in the list until
    /// somebody removes it or gives it a new date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<i64>,
    /// Kept, and not checked, until resumed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub paused: bool,
    /// When it was set, in seconds.
    pub created: i64,
    /// The last price it was checked against, which is what a crossing is
    /// measured from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<f64>,
    /// When it fired, for an alert that fires once. A fired alert is done
    /// until it is armed again: a level that fires every two minutes while
    /// the price sits on it is noise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fired: Option<i64>,
    /// Every firing, newest last, at most [`LOG_LIMIT`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub log: Vec<Hit>,
    /// A rule over the bars, such as `sma(20) crosses_above sma(50)`, in
    /// place of a price level: see [`crate::condition`]. With one, the
    /// condition and level above mean nothing. Kept as the text it was
    /// written in, so a rule this build cannot read is still shown as it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    /// The bars a rule is read on. Unset is daily.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeframe: Option<Timeframe>,
    /// Ask a rule only about bars that have closed, rather than about the
    /// one forming, whose indicators move with every tick.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub on_close: bool,
    /// The bar a rule last fired on, so that it fires once a bar at most.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bar: Option<i64>,
}

fn is_once(f: &Frequency) -> bool {
    *f == Frequency::Once
}

/// What a check found.
#[derive(Clone, PartialEq, Debug)]
pub struct Fired {
    pub id: i64,
    /// `AAPL rose through 195.00`, after the alert's name when it has one.
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
            upper: None,
            drawing: None,
            name: String::new(),
            note: String::new(),
            frequency: Frequency::Once,
            expires: None,
            paused: false,
            created,
            last: None,
            fired: None,
            log: Vec::new(),
            rule: None,
            timeframe: None,
            on_close: false,
            bar: None,
        }
    }

    /// An alert on a rule over the bars rather than on a price.
    pub fn on_rule(symbol: &str, suffix: Option<&str>, rule: &Rule, timeframe: Timeframe, created: i64) -> Alert {
        let mut alert = Alert::new(symbol, suffix, Condition::Cross, 0.0, created);
        alert.rule = Some(rule.to_string());
        alert.timeframe = Some(timeframe);
        alert
    }

    pub fn is_rule(&self) -> bool {
        self.rule.is_some()
    }

    /// The rule, read; `None` for a price alert, `Some(Err)` for a rule this
    /// build cannot read.
    pub fn parsed_rule(&self) -> Option<Result<Rule, String>> {
        self.rule.as_deref().map(Rule::parse)
    }

    /// A price the alert's figures are written to the scale of: its level,
    /// or for a rule, which has none, the price it last fired at.
    pub fn scale(&self) -> f64 {
        match self.is_rule() {
            true => self.log.last().map_or(0.0, |h| h.price),
            false => self.level,
        }
    }

    /// The bars a rule is read on.
    pub fn rule_timeframe(&self) -> Timeframe {
        self.timeframe.unwrap_or(Timeframe::days(1))
    }

    /// What it fires on, for a list: `Crossing up`, or `Rule on 1h, bar close`.
    pub fn spell_condition(&self) -> String {
        match self.is_rule() {
            true => format!(
                "Rule on {}{}",
                self.rule_timeframe().key(),
                if self.on_close { ", bar close" } else { "" }
            ),
            false => self.condition.label().to_string(),
        }
    }

    /// Whether it would fire now: not fired, not paused, not past its date.
    pub fn is_armed(&self) -> bool {
        self.is_armed_at(chrono::Utc::now().timestamp())
    }

    pub fn is_armed_at(&self, now: i64) -> bool {
        self.state(now) == State::Armed
    }

    pub fn state(&self, now: i64) -> State {
        if self.fired.is_some() {
            State::Fired
        } else if self.expires.is_some_and(|at| at <= now) {
            State::Expired
        } else if self.paused {
            State::Paused
        } else {
            State::Armed
        }
    }

    /// The channel's bounds, low first. A single level is a channel of none.
    pub fn bounds(&self) -> (f64, f64) {
        let other = self.upper.unwrap_or(self.level);
        (self.level.min(other), self.level.max(other))
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

    /// The level, or the channel, as figures: `195.00`, `190.00–200.00`.
    pub fn spell_level(&self, decimals: usize) -> String {
        if let Some(rule) = &self.rule {
            return rule.clone();
        }
        match self.condition.is_channel() {
            true => {
                let (lo, hi) = self.bounds();
                format!("{lo:.decimals$}–{hi:.decimals$}")
            }
            false => format!("{:.decimals$}", self.level),
        }
    }

    /// Whether `price` satisfies the condition, given the one before it.
    fn hits(&self, from: Option<f64>, price: f64) -> bool {
        let level = self.level;
        let (lo, hi) = self.bounds();
        let inside = |p: f64| (lo..=hi).contains(&p);
        match self.condition {
            Condition::Above => price >= level,
            Condition::Below => price <= level,
            Condition::Cross => from.is_some_and(|from| {
                (from < level && price >= level) || (from > level && price <= level)
            }),
            Condition::CrossUp => from.is_some_and(|from| from < level && price >= level),
            Condition::CrossDown => from.is_some_and(|from| from > level && price <= level),
            Condition::Enter => from.is_some_and(|from| !inside(from) && inside(price)),
            Condition::Exit => from.is_some_and(|from| inside(from) && !inside(price)),
        }
    }

    /// Put a price to the alert, at `now`, and say whether that fired it.
    ///
    /// Remembers the price either way, so the next check measures a crossing
    /// from here. A crossing alert seeing its first price only remembers
    /// it: with nothing before, there is nothing to have crossed from.
    pub fn check(&mut self, price: f64, now: i64, decimals: usize) -> Option<Fired> {
        if !price.is_finite() || !self.is_armed_at(now) {
            return None;
        }
        let from = self.last.replace(price);
        let mut hit = self.hits(from, price);
        // Every time, for a place rather than a move, means every time it
        // gets there: not every price while it stays.
        if hit && self.frequency == Frequency::Every && !self.condition.is_move() {
            hit = !from.is_some_and(|from| self.hits(None, from));
        }
        if !hit {
            return None;
        }
        if self.frequency == Frequency::Once {
            self.fired = Some(now);
        }
        self.log.push(Hit { at: now, price });
        if self.log.len() > LOG_LIMIT {
            self.log.drain(..self.log.len() - LOG_LIMIT);
        }
        let note = self.message(price, decimals);
        let note = match note.is_empty() {
            true => String::new(),
            false => format!(" · {note}"),
        };
        let what = format!(
            "{} {} {}",
            self.spell_symbol(),
            self.condition.verb(from, self.level),
            self.spell_level(decimals)
        );
        Some(Fired {
            id: self.id,
            title: match self.name.is_empty() {
                true => what,
                false => format!("{}: {what}", self.name),
            },
            body: format!("now {price:.decimals$}{note}"),
        })
    }

    /// The note with its placeholders filled in.
    pub fn message(&self, price: f64, decimals: usize) -> String {
        self.note
            .replace("{{ticker}}", &self.spell_symbol())
            .replace("{{price}}", &format!("{price:.decimals$}"))
            .replace("{{level}}", &self.spell_level(decimals))
    }

    /// Ready to fire again, measuring from the next price rather than the
    /// one that fired it.
    pub fn rearm(&mut self) {
        self.fired = None;
        self.last = None;
    }

    /// Put the bars to a rule alert, at `now`, and say whether that fired it.
    ///
    /// `bars` are the alert's timeframe, oldest first, the last one perhaps
    /// still forming — which it can only be while the market is `trading`:
    /// Friday's daily bar is closed on Saturday morning, a day after it
    /// opened or not. Asked about the newest bar, or with
    /// [`Alert::on_close`] the newest that has closed. Fires at most once a
    /// bar. "Every time" on a rule with a place in it — `rsi(14) > 70` —
    /// means each time it starts to hold, not every bar it stays true.
    pub fn check_rule(
        &mut self,
        bars: &[Bar],
        inputs: &Inputs,
        now: i64,
        trading: bool,
        decimals: usize,
    ) -> Option<Fired> {
        if !self.is_armed_at(now) {
            return None;
        }
        let rule = self.parsed_rule()?.ok()?;
        let last = bars.last()?;
        let forming = trading && last.ts + self.rule_timeframe().seconds() > now;
        let at = match self.on_close && forming {
            true => bars.len().checked_sub(2)?,
            false => bars.len() - 1,
        };
        let bar = bars[at].ts;
        if self.bar == Some(bar) {
            return None;
        }
        let computed = rule.compute(bars, inputs);
        let hit = match self.frequency {
            Frequency::Once => rule.holds(&computed, at),
            Frequency::Every => rule.starts(&computed, at),
        };
        if !hit {
            return None;
        }
        let price = last.close;
        self.bar = Some(bar);
        if self.frequency == Frequency::Once {
            self.fired = Some(now);
        }
        self.log.push(Hit { at: now, price });
        if self.log.len() > LOG_LIMIT {
            self.log.drain(..self.log.len() - LOG_LIMIT);
        }
        let values: Vec<String> = computed
            .values_at(at)
            .into_iter()
            .filter(|(s, _)| *s != crate::condition::Source::Close)
            .filter_map(|(s, v)| v.map(|v| format!("{s} {}", spell_value(v, decimals))))
            .collect();
        let value = values.first().map(|v| v.rsplit(' ').next().unwrap_or("").to_string()).unwrap_or_default();
        let note = self.message(price, decimals).replace("{{value}}", &value);
        let mut body = format!("now {price:.decimals$}");
        if !values.is_empty() {
            body.push_str(&format!(" · {}", values.join(" · ")));
        }
        if !note.is_empty() {
            body.push_str(&format!(" · {note}"));
        }
        let what = format!("{} {} ({})", self.spell_symbol(), rule, self.rule_timeframe().key());
        Some(Fired {
            id: self.id,
            title: match self.name.is_empty() {
                true => what,
                false => format!("{}: {what}", self.name),
            },
            body,
        })
    }

    /// Stop checking it, keeping everything else.
    pub fn pause(&mut self) {
        self.paused = true;
    }

    /// Check it again. The price it last saw may be hours old, and a
    /// crossing measured from it would fire on a move nobody watched.
    pub fn resume(&mut self) {
        self.paused = false;
        self.last = None;
    }

    /// Back to being checked from the next bar, as an edit leaves it.
    pub fn restart(&mut self) {
        self.fired = None;
        self.bar = None;
    }

    /// The same alert, ready to be given its own id: armed, with no log.
    pub fn duplicate(&self, created: i64) -> Alert {
        Alert { id: 0, created, fired: None, paused: false, log: Vec::new(), bar: None, ..self.clone() }
    }

    /// The last time it fired, from the log or from before there was one.
    pub fn last_fired(&self) -> Option<i64> {
        self.log.last().map(|h| h.at).or(self.fired)
    }

    /// One line: `#3 [Breakout] AAPL cross 195.00 (line #12) · every time · armed · "go"`.
    pub fn describe(&self, decimals: usize, now: i64) -> String {
        let name = match self.name.is_empty() {
            true => String::new(),
            false => format!(" [{}]", self.name),
        };
        let line = self.drawing.map(|id| format!(" (line #{id})")).unwrap_or_default();
        let frequency = match self.frequency {
            Frequency::Once => "",
            Frequency::Every => " · every time",
        };
        let expires = match (self.expires, self.state(now)) {
            (Some(at), State::Armed | State::Paused) => chrono::DateTime::from_timestamp(at, 0)
                .map(|t| format!(" · until {}", t.format("%Y-%m-%d %H:%M UTC")))
                .unwrap_or_default(),
            _ => String::new(),
        };
        let note = match self.note.is_empty() {
            true => String::new(),
            false => format!(" · {:?}", self.note),
        };
        let what = match &self.rule {
            Some(rule) => format!(
                "when {rule} on {}{}",
                self.rule_timeframe().key(),
                if self.on_close { " at bar close" } else { "" }
            ),
            None => format!("{} {}", self.condition.key(), self.spell_level(decimals)),
        };
        format!(
            "#{}{name} {} {what}{line}{frequency} · {}{expires}{note}",
            self.id,
            self.spell_symbol(),
            self.state(now).key(),
        )
    }
}

/// An indicator's value, to as many places as it reads: a price to the
/// instrument's, an oscillator to two.
fn spell_value(value: f64, decimals: usize) -> String {
    let places = if value.abs() >= 1000.0 { decimals.min(2) } else { decimals.max(2) };
    format!("{value:.places$}")
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
        assert_eq!(up.log, vec![Hit { at: 2, price: 100.5 }]);
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
    fn a_crossing_one_way_ignores_the_other() {
        let mut up = alert(Condition::CrossUp, 100.0);
        up.check(101.0, 1, 2);
        assert!(up.check(99.0, 2, 2).is_none(), "down is not up");
        assert!(up.check(100.5, 3, 2).is_some());
        let mut down = alert(Condition::CrossDown, 100.0);
        down.check(99.0, 1, 2);
        assert!(down.check(101.0, 2, 2).is_none());
        assert_eq!(down.check(98.0, 3, 2).map(|f| f.title), Some("AAPL fell through 100.00".into()));
    }

    #[test]
    fn a_channel_fires_on_the_way_in_or_out() {
        let mut enter = alert(Condition::Enter, 200.0);
        enter.upper = Some(190.0);
        assert_eq!(enter.bounds(), (190.0, 200.0));
        enter.check(205.0, 1, 2);
        let fired = enter.check(195.0, 2, 2).expect("into it");
        assert_eq!(fired.title, "AAPL entered 190.00–200.00");
        let mut exit = alert(Condition::Exit, 190.0);
        exit.upper = Some(200.0);
        exit.check(195.0, 1, 2);
        assert!(exit.check(199.0, 2, 2).is_none());
        assert!(exit.check(189.0, 3, 2).is_some());
    }

    #[test]
    fn every_time_stays_armed_and_fires_on_each_arrival() {
        let mut every = alert(Condition::Above, 100.0);
        every.frequency = Frequency::Every;
        assert!(every.check(101.0, 1, 2).is_some(), "first price already there");
        assert!(every.check(102.0, 2, 2).is_none(), "still there is not again");
        assert!(every.check(99.0, 3, 2).is_none());
        assert!(every.check(100.0, 4, 2).is_some(), "back again");
        assert!(every.fired.is_none() && every.is_armed_at(5));
        assert_eq!(every.log.len(), 2);
        let mut cross = alert(Condition::Cross, 100.0);
        cross.frequency = Frequency::Every;
        cross.check(99.0, 1, 2);
        assert!(cross.check(101.0, 2, 2).is_some());
        assert!(cross.check(99.0, 3, 2).is_some());
    }

    #[test]
    fn paused_and_expired_alerts_stay_quiet() {
        let mut a = alert(Condition::Above, 100.0);
        a.pause();
        assert_eq!(a.state(0), State::Paused);
        assert!(a.check(101.0, 1, 2).is_none());
        a.resume();
        a.expires = Some(10);
        assert_eq!(a.state(9), State::Armed);
        assert_eq!(a.state(10), State::Expired);
        assert!(a.check(101.0, 11, 2).is_none());
        assert!(a.check(101.0, 9, 2).is_some());
    }

    #[test]
    fn a_name_a_note_and_a_suffix_reach_the_notification() {
        let mut a = Alert::new("NESN", Some("SW"), Condition::Above, 80.0, 0);
        a.note = "trim {{ticker}} at {{price}} (set {{level}})".into();
        a.name = "Swiss".into();
        let fired = a.check(81.0, 1, 2).unwrap();
        assert_eq!(fired.title, "Swiss: NESN.SW rose to 80.00");
        assert_eq!(fired.body, "now 81.00 · trim NESN.SW at 81.00 (set 80.00)");
        assert!(a.is_for("NESN", Some("SW")) && !a.is_for("NESN", None));
    }

    #[test]
    fn the_log_keeps_the_newest() {
        let mut a = alert(Condition::Cross, 100.0);
        a.frequency = Frequency::Every;
        for n in 0..(LOG_LIMIT as i64 * 2 + 2) {
            a.check(if n % 2 == 0 { 99.0 } else { 101.0 }, n, 2);
        }
        assert_eq!(a.log.len(), LOG_LIMIT);
        assert_eq!(a.log.last().unwrap().at, LOG_LIMIT as i64 * 2 + 1);
    }

    #[test]
    fn the_list_survives_the_store_and_reads_the_old_one() {
        let mut a = alert(Condition::CrossUp, 100.0);
        a.id = 3;
        a.drawing = Some(12);
        a.frequency = Frequency::Every;
        a.expires = Some(99);
        a.log.push(Hit { at: 1, price: 2.0 });
        let list = vec![a];
        assert_eq!(from_json(Some(&to_json(&list))), list);
        assert_eq!(from_json(Some("not json")), Vec::<Alert>::new());
        let old = r#"[{"id":1,"symbol":"AAPL","condition":"above","level":250.0,"created":5,"fired":7}]"#;
        let read = from_json(Some(old));
        assert_eq!(read[0].frequency, Frequency::Once);
        assert_eq!(read[0].state(10), State::Fired);
        assert_eq!(next_id(&list), 4);
        assert_eq!(next_id(&[]), 1);
    }

    #[test]
    fn a_duplicate_starts_fresh() {
        let mut a = alert(Condition::Above, 100.0);
        a.id = 4;
        a.check(101.0, 1, 2);
        a.pause();
        let b = a.duplicate(9);
        assert_eq!((b.id, b.created, b.fired, b.paused, b.log.len()), (0, 9, None, false, 0));
        assert_eq!(b.level, 100.0);
    }

    fn day_bars(closes: &[f64]) -> Vec<Bar> {
        closes
            .iter()
            .enumerate()
            .map(|(i, &c)| Bar { ts: i as i64 * 86_400, open: c, high: c, low: c, close: c, volume: 1.0 })
            .collect()
    }

    fn rule_alert(text: &str) -> Alert {
        let rule = Rule::parse(text).unwrap();
        Alert::on_rule("SPY", None, &rule, Timeframe::days(1), 0)
    }

    const INPUTS: Inputs = Inputs { session_origin: 0, timeframe: Timeframe::days(1) };

    #[test]
    fn a_rule_fires_on_the_bar_its_cross_happens() {
        let mut a = rule_alert("sma(2) crosses_above sma(3)");
        a.name = "Trend".into();
        a.note = "go long, sma(2) at {{value}}".into();
        let b = day_bars(&[10.0, 10.0, 10.0, 11.0]);
        // Bar 2 newest: flat, nothing yet.
        assert!(a.check_rule(&b[..3], &INPUTS, 3 * 86_400, true, 2).is_none());
        let fired = a.check_rule(&b, &INPUTS, 3 * 86_400 + 60, true, 2).expect("crossed on bar 3");
        assert_eq!(fired.title, "Trend: SPY sma(2) crosses_above sma(3) (1D)");
        assert_eq!(fired.body, "now 11.00 · sma(2) 10.50 · sma(3) 10.33 · go long, sma(2) at 10.50");
        assert_eq!((a.fired, a.bar), (Some(3 * 86_400 + 60), Some(3 * 86_400)));
        a.rearm();
        assert!(a.check_rule(&b, &INPUTS, 3 * 86_400 + 120, true, 2).is_none(), "once a bar");
    }

    #[test]
    fn on_close_waits_for_the_bar_to_finish() {
        let mut a = rule_alert("close > 10.5");
        a.on_close = true;
        let b = day_bars(&[10.0, 10.0, 11.0]);
        assert!(a.check_rule(&b, &INPUTS, 2 * 86_400 + 3600, true, 2).is_none(), "bar 2 still forming");
        assert!(a.check_rule(&b, &INPUTS, 3 * 86_400, true, 2).is_some(), "bar 2 closed");
        let mut weekend = rule_alert("close > 10.5");
        weekend.on_close = true;
        assert!(weekend.check_rule(&b, &INPUTS, 2 * 86_400 + 3600, false, 2).is_some(), "no market, no forming bar");
        let mut live = rule_alert("close > 10.5");
        assert!(live.check_rule(&b, &INPUTS, 2 * 86_400 + 3600, true, 2).is_some(), "the forming bar counts");
    }

    #[test]
    fn every_time_on_a_rule_fires_as_it_starts_to_hold() {
        let mut a = rule_alert("close > 10.5");
        a.frequency = Frequency::Every;
        let b = day_bars(&[10.0, 11.0, 12.0, 10.0, 11.0]);
        let mut fired = Vec::new();
        for n in 1..=b.len() {
            if a.check_rule(&b[..n], &INPUTS, (n as i64 - 1) * 86_400 + 60, true, 2).is_some() {
                fired.push(n - 1);
            }
        }
        assert_eq!(fired, vec![1, 4]);
        assert!(a.is_armed_at(10 * 86_400));
    }

    #[test]
    fn a_rule_alert_survives_the_store_and_says_what_it_is() {
        let mut a = rule_alert("rsi(14) crosses_below 30");
        a.id = 2;
        a.timeframe = Some(Timeframe::hours(1));
        a.on_close = true;
        let list = vec![a.clone()];
        assert_eq!(from_json(Some(&to_json(&list))), list);
        assert_eq!(a.describe(2, 0), "#2 SPY when rsi(14) crosses_below 30 on 1h at bar close · armed");
        assert_eq!(a.spell_condition(), "Rule on 1h, bar close");
        assert_eq!(a.spell_level(2), "rsi(14) crosses_below 30");
        let mut broken = a.clone();
        broken.rule = Some("rsi(14) ~ 3".into());
        assert!(broken.check_rule(&day_bars(&[1.0, 2.0]), &INPUTS, 0, true, 2).is_none());
    }
}
