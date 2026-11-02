//! Sizing a trade from where it is entered, where it is stopped and where it
//! is taken.
//!
//! A position drawing is three prices. What makes it worth drawing rather
//! than three horizontal lines is the arithmetic between them: how far the
//! stop is from the entry is the risk on one unit, and the account's risk on
//! one trade divided by that is how many units to buy. The arithmetic lives
//! here, away from any widget, so the chart, the terminal and the tests all
//! get the same answer from the same function.

use serde::{Deserialize, Serialize};

use crate::{grouped_decimals, price_decimals, InstrumentKind};

/// The settings key the account and the risk are stored under.
pub const SETTING: &str = "position_risk";

/// How much of the account one trade may lose.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "lowercase")]
pub enum Stake {
    /// A share of the account, in percent: `1.0` is one percent.
    Percent(f64),
    /// A fixed amount in the account's currency, whatever the account holds.
    Amount(f64),
}

impl Stake {
    /// Read `1%`, `0.5 %` or `250` — a percentage or an amount.
    pub fn parse(text: &str) -> Option<Stake> {
        let text = text.trim();
        let (number, percent) = match text.strip_suffix('%') {
            Some(rest) => (rest.trim(), true),
            None => (text, false),
        };
        let value: f64 = number.replace(['_', '\''], "").parse().ok()?;
        if !value.is_finite() || value <= 0.0 || (percent && value > 100.0) {
            return None;
        }
        Some(match percent {
            true => Stake::Percent(value),
            false => Stake::Amount(value),
        })
    }

    /// The other way: `1%` or `250`.
    pub fn spell(self) -> String {
        match self {
            Stake::Percent(p) => format!("{}%", trim_float(p)),
            Stake::Amount(a) => trim_float(a),
        }
    }
}

/// The account a position is sized against.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Risk {
    /// What the account is worth, in its own currency.
    pub account: f64,
    /// How much of it a trade may lose.
    pub stake: Stake,
    /// The account's currency, for saying amounts back: `CHF`, `USD`. Empty
    /// says amounts as bare numbers.
    #[serde(default)]
    pub currency: String,
}

impl Default for Risk {
    /// Ten thousand and one percent: round numbers somebody replaces with
    /// their own, and which give a sensible picture until they do.
    fn default() -> Risk {
        Risk { account: 10_000.0, stake: Stake::Percent(1.0), currency: String::new() }
    }
}

impl Risk {
    /// What one trade may lose, in the account's currency.
    pub fn amount(&self) -> f64 {
        match self.stake {
            Stake::Percent(p) => self.account * p / 100.0,
            Stake::Amount(a) => a,
        }
    }

    /// Read back what [`Risk::to_json`] wrote; anything unreadable is the
    /// default, since a broken row should not take the tool away.
    pub fn from_json(text: Option<&str>) -> Risk {
        text.and_then(|t| serde_json::from_str(t).ok()).unwrap_or_default()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// `10,000 CHF, risking 1% (100 CHF) a trade`.
    pub fn describe(&self) -> String {
        let per = match self.stake {
            Stake::Percent(_) => format!("{} ({})", self.stake.spell(), self.money(self.amount())),
            Stake::Amount(_) => self.money(self.amount()),
        };
        format!("account {}, risking {per} a trade", self.money(self.account))
    }

    /// An amount with the currency after it, when there is one.
    pub fn money(&self, value: f64) -> String {
        // Cents only where they are a noticeable part of the amount.
        let decimals = if value.abs() >= 100.0 || value.fract() == 0.0 { 0 } else { 2 };
        let number = grouped_decimals(value, decimals);
        match self.currency.is_empty() {
            true => number,
            false => format!("{number} {}", self.currency),
        }
    }
}

/// Which way a position is.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Long,
    Short,
}

impl Side {
    pub fn key(self) -> &'static str {
        match self {
            Side::Long => "long",
            Side::Short => "short",
        }
    }
}

/// A trade worked out: its side, the distances, the ratio and the size.
#[derive(Clone, Copy, PartialEq, Debug, Serialize)]
pub struct Plan {
    pub side: Side,
    pub entry: f64,
    pub stop: f64,
    pub target: f64,
    /// What one unit loses at the stop, as a positive number.
    pub risk_per_unit: f64,
    /// What one unit makes at the target, as a positive number; negative
    /// when the target is on the stop's side of the entry.
    pub reward_per_unit: f64,
    /// Reward over risk. `None` with the stop on the entry.
    pub ratio: Option<f64>,
    /// How many units the account's risk buys: whole units for anything
    /// traded in whole units, fractional for crypto and currencies.
    pub quantity: f64,
    /// What the position loses at the stop and makes at the target, sized.
    pub loss: f64,
    pub gain: f64,
    /// What the position costs to put on: entry times quantity.
    pub notional: f64,
}

impl Plan {
    /// Work out a trade. The side is read from the target: above the entry
    /// is long, below is short — the stop is then wherever the hand put it,
    /// and a stop on the wrong side shows as a ratio below zero rather than
    /// being moved behind anybody's back.
    pub fn new(entry: f64, stop: f64, target: f64, risk: &Risk, kind: Option<InstrumentKind>) -> Plan {
        let side = match target >= entry {
            true => Side::Long,
            false => Side::Short,
        };
        let sign = match side {
            Side::Long => 1.0,
            Side::Short => -1.0,
        };
        let risk_per_unit = (entry - stop) * sign;
        let reward_per_unit = (target - entry) * sign;
        let ratio = (risk_per_unit.abs() > f64::EPSILON).then(|| reward_per_unit / risk_per_unit);
        let raw = match risk_per_unit > f64::EPSILON {
            true => risk.amount() / risk_per_unit,
            false => 0.0,
        };
        let quantity = match fractional(kind) {
            true => (raw * 10_000.0).floor() / 10_000.0,
            false => raw.floor(),
        }
        .max(0.0);
        Plan {
            side,
            entry,
            stop,
            target,
            risk_per_unit,
            reward_per_unit,
            ratio,
            quantity,
            loss: quantity * risk_per_unit,
            gain: quantity * reward_per_unit,
            notional: quantity * entry,
        }
    }

    /// The words on the target zone, the stop zone and the entry, each one
    /// line, at the instrument's own precision.
    pub fn labels(&self, risk: &Risk, kind: Option<InstrumentKind>) -> Labels {
        let decimals = price_decimals(0.0, self.entry, kind);
        let price = |p: f64| grouped_decimals(p, decimals);
        let pct = |from: f64, to: f64| match from != 0.0 {
            true => format!("{:+.2}%", (to - from) / from.abs() * 100.0),
            false => String::new(),
        };
        let ratio = match self.ratio {
            Some(r) => format!("R:R {r:.2}"),
            None => "R:R —".to_string(),
        };
        Labels {
            target: format!(
                "Target {}  {}  +{}",
                price(self.target),
                pct(self.entry, self.target),
                risk.money(self.gain)
            ),
            stop: format!(
                "Stop {}  {}  −{}",
                price(self.stop),
                pct(self.entry, self.stop),
                risk.money(self.loss)
            ),
            entry: format!(
                "{} {} @ {}  {ratio}",
                match self.side {
                    Side::Long => "Long",
                    Side::Short => "Short",
                },
                spell_quantity(self.quantity),
                price(self.entry),
            ),
        }
    }

    /// One line for a terminal.
    pub fn describe(&self, risk: &Risk, kind: Option<InstrumentKind>) -> String {
        let l = self.labels(risk, kind);
        format!("{}; {}; {}", l.entry, l.target, l.stop)
    }
}

/// What a position drawing says, line by line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Labels {
    pub target: String,
    pub stop: String,
    pub entry: String,
}

/// Whether the instrument trades in parts of a unit.
fn fractional(kind: Option<InstrumentKind>) -> bool {
    matches!(kind, Some(InstrumentKind::Crypto | InstrumentKind::Fx))
}

/// A quantity as a person writes it: `166`, `0.4123`.
pub fn spell_quantity(quantity: f64) -> String {
    match quantity.fract() == 0.0 {
        true => grouped_decimals(quantity, 0),
        false => trim_float(quantity),
    }
}

fn trim_float(value: f64) -> String {
    let text = format!("{value:.4}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    text.to_string()
}

/// Where the stop goes when nobody has put it anywhere: as far below the
/// entry as the target is above it, so a fresh position starts at one to one
/// and the hand moves the stop to where the chart says it belongs.
pub fn mirrored_stop(entry: f64, target: f64) -> f64 {
    entry - (target - entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn risk() -> Risk {
        Risk { account: 50_000.0, stake: Stake::Percent(1.0), currency: "CHF".into() }
    }

    #[test]
    fn a_long_is_sized_from_the_distance_to_the_stop() {
        let plan = Plan::new(100.0, 95.0, 115.0, &risk(), None);
        assert_eq!(plan.side, Side::Long);
        assert_eq!(plan.risk_per_unit, 5.0);
        assert_eq!(plan.quantity, 100.0);
        assert_eq!(plan.ratio, Some(3.0));
        assert_eq!(plan.loss, 500.0);
        assert_eq!(plan.gain, 1500.0);
    }

    #[test]
    fn a_short_is_read_from_a_target_below_the_entry() {
        let plan = Plan::new(100.0, 104.0, 90.0, &risk(), None);
        assert_eq!(plan.side, Side::Short);
        assert_eq!(plan.risk_per_unit, 4.0);
        assert_eq!(plan.quantity, 125.0);
        assert_eq!(plan.ratio, Some(2.5));
    }

    #[test]
    fn shares_are_whole_and_crypto_is_not() {
        let r = Risk { account: 1000.0, stake: Stake::Amount(10.0), currency: String::new() };
        assert_eq!(Plan::new(100.0, 97.0, 110.0, &r, None).quantity, 3.0);
        assert_eq!(Plan::new(100.0, 97.0, 110.0, &r, Some(InstrumentKind::Crypto)).quantity, 3.3333);
    }

    #[test]
    fn a_stop_on_the_entry_sizes_nothing_rather_than_infinity() {
        let plan = Plan::new(100.0, 100.0, 110.0, &risk(), None);
        assert_eq!(plan.quantity, 0.0);
        assert_eq!(plan.ratio, None);
    }

    #[test]
    fn a_stake_is_a_percentage_or_an_amount() {
        assert_eq!(Stake::parse("1%"), Some(Stake::Percent(1.0)));
        assert_eq!(Stake::parse(" 0.5 % "), Some(Stake::Percent(0.5)));
        assert_eq!(Stake::parse("250"), Some(Stake::Amount(250.0)));
        assert_eq!(Stake::parse("1'000"), Some(Stake::Amount(1000.0)));
        assert_eq!(Stake::parse("0"), None);
        assert_eq!(Stake::parse("150%"), None);
        assert_eq!(Stake::parse("lots"), None);
        assert_eq!(Stake::Percent(0.5).spell(), "0.5%");
        assert_eq!(Stake::Amount(250.0).spell(), "250");
    }

    #[test]
    fn risk_survives_the_store_and_a_broken_row_is_the_default() {
        let r = risk();
        assert_eq!(Risk::from_json(Some(&r.to_json())), r);
        assert_eq!(Risk::from_json(Some("{nonsense")), Risk::default());
        assert_eq!(Risk::from_json(None), Risk::default());
    }

    #[test]
    fn the_labels_say_size_ratio_and_money() {
        let plan = Plan::new(100.0, 95.0, 115.0, &risk(), None);
        let labels = plan.labels(&risk(), None);
        assert!(labels.entry.starts_with("Long 100 @ 100"), "{}", labels.entry);
        assert!(labels.entry.ends_with("R:R 3.00"), "{}", labels.entry);
        assert!(labels.target.contains("+15.00%") && labels.target.contains("1,500 CHF"), "{}", labels.target);
        assert!(labels.stop.contains("-5.00%") && labels.stop.contains("500 CHF"), "{}", labels.stop);
    }

    use crate::drawings::{Anchor, Drawing, Grip, Kind, Projected};

    fn position() -> Drawing {
        Drawing::new(Kind::Position, Anchor::new(0, 100.0), Anchor::new(10, 110.0))
    }

    #[test]
    fn a_fresh_position_mirrors_its_stop_until_one_is_put_down() {
        let mut d = position();
        assert_eq!(d.stop_price(), 90.0);
        d.move_grip(Grip::FromTo, Anchor::new(0, 120.0));
        assert_eq!(d.stop_price(), 80.0, "an unset stop follows the target");
        d.move_grip(Grip::Stop, Anchor::new(0, 97.0));
        assert_eq!(d.stop, Some(97.0));
        assert_eq!(d.to.ts, 10, "the target grip moves a price, not the far end");
    }

    #[test]
    fn moving_the_entry_leaves_a_mirrored_stop_where_it_was() {
        let mut d = position();
        d.move_grip(Grip::From, Anchor::new(2, 102.0));
        assert_eq!(d.stop, Some(90.0));
        assert_eq!(d.from, Anchor::new(2, 102.0));
    }

    #[test]
    fn the_far_end_moves_time_only_and_the_body_moves_the_stop_too() {
        let mut d = position();
        assert_eq!(d.drag_grip(Grip::Right, Anchor::new(30, 1.0)), Grip::Right);
        assert_eq!((d.to.ts, d.to.price), (30, 110.0));
        d.stop = Some(95.0);
        d.shift(5, 2.0);
        assert_eq!(d.stop, Some(97.0));
    }

    #[test]
    fn the_stop_is_a_grip_and_the_stop_zone_is_the_body() {
        // Entry at y=100, target at y=50, stop at y=130 (screen grows down).
        let p = Projected::new(Kind::Position, (10.0, 100.0), (110.0, 50.0)).with_stop(130.0);
        assert_eq!(p.hit(10.0, 130.0), Some(Grip::Stop));
        assert_eq!(p.hit(10.0, 50.0), Some(Grip::FromTo));
        assert_eq!(p.hit(110.0, 100.0), Some(Grip::Right));
        assert_eq!(p.hit(60.0, 120.0), Some(Grip::Body));
        assert_eq!(p.bounds(), (10.0, 50.0, 100.0, 80.0));
    }

    #[test]
    fn a_position_survives_the_store_with_its_stop() {
        let mut d = position();
        d.stop = Some(96.5);
        let back: Drawing = serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
        assert_eq!(back, d);
        assert_eq!(Kind::from_key("long"), Some(Kind::Position));
    }
}
