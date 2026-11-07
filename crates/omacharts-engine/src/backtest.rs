//! Backtests: what a strategy written as rules would have done over a
//! symbol's history.
//!
//! A strategy is an entry rule, and optionally an exit rule, a stop and a
//! target, all read with [`crate::condition`] over the same bars a chart
//! draws. Long only, one position at a time.
//!
//! The simulation keeps to what could have been done on the day:
//!
//! - A rule is asked about a bar only once that bar has closed, and acted on
//!   at the next bar's open. Filling at the close the signal came from would
//!   trade on a price nobody could still get.
//! - A stop and a target are watched inside each bar from the one filled on.
//!   A bar that opens past the stop fills at the open, not at the stop it
//!   jumped; a bar that reaches both is taken as stopped, since the order
//!   inside a bar is not known and assuming the kinder one flatters.
//! - Every fill pays the slippage and the commission.
//!
//! Positions are sized as the position tool sizes them: the stake of the
//! account at risk between entry and stop, the account being what the
//! trades have made of it so far. With no stop there is no risk to size by,
//! and the whole account goes in. Never more than the account: no leverage.

use crate::bars::Bar;
use crate::condition::{Inputs, Rule};
use crate::indicators::oscillators;
use crate::position::Stake;

/// Where a trade is stopped out, from the price it filled at.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Stop {
    /// So many average true ranges below, as the ATR stood on the signal bar.
    Atr { multiple: f64, period: usize },
    /// So many percent below.
    Percent(f64),
}

/// Where a trade takes its profit.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Target {
    /// So many times the distance to the stop above: `2R`.
    R(f64),
    /// So many percent above.
    Percent(f64),
}

impl Stop {
    /// `2*atr(14)`, `2 atr`, `atr(20)`, `1.5atr(10)` or `3%`.
    pub fn parse(text: &str) -> Result<Stop, String> {
        let bad = || format!("a stop is a multiple of the ATR, such as 2*atr(14), or a percentage, such as 3%; not {text:?}");
        let t = text.trim().to_lowercase().replace(' ', "");
        if let Some(p) = t.strip_suffix('%') {
            let p: f64 = p.parse().map_err(|_| bad())?;
            return match p > 0.0 && p < 100.0 {
                true => Ok(Stop::Percent(p)),
                false => Err(bad()),
            };
        }
        let at = t.find("atr").ok_or_else(bad)?;
        let multiple = match t[..at].trim_end_matches('*') {
            "" => 1.0,
            m => m.parse::<f64>().map_err(|_| bad())?,
        };
        let rest = &t[at + 3..];
        let period = match rest {
            "" => 14,
            _ => rest
                .strip_prefix('(')
                .and_then(|r| r.strip_suffix(')'))
                .and_then(|n| n.parse::<usize>().ok())
                .filter(|n| (1..=500).contains(n))
                .ok_or_else(bad)?,
        };
        match multiple > 0.0 && multiple.is_finite() {
            true => Ok(Stop::Atr { multiple, period }),
            false => Err(bad()),
        }
    }

    pub fn spell(self) -> String {
        match self {
            Stop::Atr { multiple, period } => format!("{multiple}*atr({period})"),
            Stop::Percent(p) => format!("{p}%"),
        }
    }
}

impl Target {
    /// `2R`, `1.5r` or `8%`.
    pub fn parse(text: &str) -> Result<Target, String> {
        let bad = || format!("a target is a multiple of the risk, such as 2R, or a percentage, such as 8%; not {text:?}");
        let t = text.trim().to_lowercase().replace(' ', "");
        let (number, make): (&str, fn(f64) -> Target) = match (t.strip_suffix('r'), t.strip_suffix('%')) {
            (Some(n), _) => (n, Target::R),
            (_, Some(n)) => (n, Target::Percent),
            _ => return Err(bad()),
        };
        let value: f64 = number.parse().map_err(|_| bad())?;
        match value > 0.0 && value.is_finite() {
            true => Ok(make(value)),
            false => Err(bad()),
        }
    }

    pub fn spell(self) -> String {
        match self {
            Target::R(r) => format!("{r}R"),
            Target::Percent(p) => format!("{p}%"),
        }
    }
}

/// What to trade on.
#[derive(Clone, PartialEq, Debug)]
pub struct Strategy {
    pub entry: Rule,
    pub exit: Option<Rule>,
    pub stop: Option<Stop>,
    pub target: Option<Target>,
}

/// What a trade costs, and what it is sized against.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Settings {
    /// What the account starts with.
    pub account: f64,
    /// How much of it one trade may lose, with a stop to measure it by.
    pub stake: Stake,
    /// Paid on every fill, in the account's currency.
    pub commission: f64,
    /// A fraction of the price lost on every fill: 0.0005 is 0.05%.
    pub slippage: f64,
}

/// Why a trade ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reason {
    /// The exit rule held.
    Rule,
    Stop,
    Target,
    /// Still open at the last bar, valued at its close.
    Open,
}

impl Reason {
    pub fn key(self) -> &'static str {
        match self {
            Reason::Rule => "rule",
            Reason::Stop => "stop",
            Reason::Target => "target",
            Reason::Open => "open",
        }
    }
}

/// One round trip.
#[derive(Clone, PartialEq, Debug)]
pub struct Trade {
    /// The bars it was filled on, in and out.
    pub entry_at: usize,
    pub exit_at: usize,
    pub entry_ts: i64,
    pub exit_ts: i64,
    /// Prices filled at, slippage included.
    pub entry: f64,
    pub exit: f64,
    pub stop: Option<f64>,
    pub target: Option<f64>,
    pub units: f64,
    /// What it made, after commission on both fills.
    pub pnl: f64,
    /// `pnl` in units of what it risked, when it had a stop.
    pub r: Option<f64>,
    pub reason: Reason,
}

/// The figures a backtest is judged by.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Stats {
    /// Closed trades; one still open is in the list but not in these.
    pub trades: usize,
    pub wins: usize,
    pub win_rate: f64,
    /// Average R over the trades that had a stop.
    pub average_r: Option<f64>,
    /// Average profit a trade.
    pub expectancy: f64,
    /// Gross profit over gross loss; `None` with no losing trade.
    pub profit_factor: Option<f64>,
    pub net_profit: f64,
    /// From the starting account to the final one, open trade valued, in percent.
    pub return_pct: f64,
    /// The deepest fall from a high of the account, in percent, negative.
    pub max_drawdown_pct: f64,
    /// Share of the bars a position was held, in percent.
    pub exposure_pct: f64,
    /// Holding from the first bar the rules could answer to the last, in percent.
    pub buy_hold_pct: f64,
    pub average_bars: f64,
}

/// What a backtest found.
#[derive(Clone, PartialEq, Debug)]
pub struct Report {
    pub trades: Vec<Trade>,
    /// The account at each bar's close, over the bars tested.
    pub equity: Vec<f64>,
    /// The bars tested: the first is where the rules could first answer.
    pub from: usize,
    pub to: usize,
    pub stats: Stats,
    /// An entry rule that held on the last bar: it would buy at the next open.
    pub signal_pending: bool,
}

struct Holding {
    entry_at: usize,
    entry: f64,
    units: f64,
    stop: Option<f64>,
    target: Option<f64>,
}

/// Run `strategy` over `bars`, all of them closed, trading only within
/// `window` (indices into `bars`; indicators still see the bars before it).
pub fn run(
    strategy: &Strategy,
    bars: &[Bar],
    inputs: &Inputs,
    settings: &Settings,
    window: std::ops::Range<usize>,
) -> Report {
    let n = bars.len();
    let entry = strategy.entry.compute(bars, inputs);
    let exit = strategy.exit.as_ref().map(|rule| (rule, rule.compute(bars, inputs)));
    let atr = match strategy.stop {
        Some(Stop::Atr { period, .. }) => oscillators::atr(bars, period),
        _ => Vec::new(),
    };
    let warm = strategy.entry.warm_up().saturating_sub(1);
    let from = window.start.max(warm).min(n);
    let to = window.end.min(n);

    let mut cash = settings.account;
    let mut holding: Option<Holding> = None;
    let mut trades = Vec::new();
    let mut equity = Vec::with_capacity(to.saturating_sub(from));
    let mut buy_at: Option<usize> = None;
    let mut sell_at: Option<usize> = None;
    let mut held_bars = 0usize;
    let buy = |price: f64| price * (1.0 + settings.slippage);
    let sell = |price: f64| price * (1.0 - settings.slippage);

    let close = |h: Holding, at: usize, price: f64, reason: Reason, cash: &mut f64, trades: &mut Vec<Trade>| {
        let exit = sell(price);
        *cash += h.units * exit - settings.commission;
        let pnl = (exit - h.entry) * h.units - 2.0 * settings.commission;
        let r = h.stop.map(|stop| pnl / ((h.entry - stop) * h.units)).filter(|r| r.is_finite());
        trades.push(Trade {
            entry_at: h.entry_at,
            exit_at: at,
            entry_ts: bars[h.entry_at].ts,
            exit_ts: bars[at].ts,
            entry: h.entry,
            exit,
            stop: h.stop,
            target: h.target,
            units: h.units,
            pnl,
            r,
            reason,
        });
    };

    // By index: a bar is acted on with the one before it (the ATR its stop
    // is measured from) and the one after it (where its decision fills).
    #[allow(clippy::needless_range_loop)]
    for i in from..to {
        let bar = &bars[i];
        // At the open: what yesterday's close decided.
        if sell_at == Some(i)
            && let Some(h) = holding.take()
        {
            close(h, i, bar.open, Reason::Rule, &mut cash, &mut trades);
        }
        if buy_at == Some(i) && holding.is_none() {
            let price = buy(bar.open);
            let stop = match strategy.stop {
                Some(Stop::Percent(p)) => Some(price * (1.0 - p / 100.0)),
                Some(Stop::Atr { multiple, .. }) => atr.get(i - 1).copied().flatten().map(|a| price - multiple * a),
                None => None,
            }
            .filter(|s| *s > 0.0 && *s < price);
            let target = match (strategy.target, stop) {
                (Some(Target::R(r)), Some(stop)) => Some(price + r * (price - stop)),
                (Some(Target::Percent(p)), _) => Some(price * (1.0 + p / 100.0)),
                _ => None,
            };
            let spend = (cash - settings.commission).max(0.0);
            let mut units = match stop {
                Some(stop) => {
                    let at_risk = match settings.stake {
                        Stake::Percent(p) => cash * p / 100.0,
                        Stake::Amount(a) => a,
                    };
                    (at_risk / (price - stop)).min(spend / price)
                }
                None => spend / price,
            };
            // Whole shares where a share is affordable; a fraction of a coin
            // where it is not.
            if units >= 1.0 {
                units = units.floor();
            }
            if units > 0.0 && units.is_finite() {
                cash -= units * price + settings.commission;
                holding = Some(Holding { entry_at: i, entry: price, units, stop, target });
            }
        }
        // Inside the bar: the stop first, then the target.
        if let Some(h) = &holding {
            let hit = match (h.stop, h.target) {
                (Some(stop), _) if bar.low <= stop => Some((bar.open.min(stop), Reason::Stop)),
                (_, Some(target)) if bar.high >= target => Some((bar.open.max(target), Reason::Target)),
                _ => None,
            };
            if let Some((price, reason)) = hit
                && let Some(h) = holding.take()
            {
                close(h, i, price, reason, &mut cash, &mut trades);
                sell_at = None;
            }
        }
        if holding.is_some() {
            held_bars += 1;
        }
        equity.push(cash + holding.as_ref().map_or(0.0, |h| h.units * bar.close));
        // At the close: what tomorrow's open will do.
        match &holding {
            Some(_) => {
                if let Some((rule, computed)) = &exit
                    && rule.holds(computed, i)
                {
                    sell_at = Some(i + 1);
                }
            }
            None => {
                if strategy.entry.starts(&entry, i) {
                    buy_at = Some(i + 1);
                }
            }
        }
    }
    let signal_pending = holding.is_none() && buy_at == Some(to) && to > from;
    if let Some(h) = holding.take() {
        let last = to - 1;
        // Valued at the close without paying to get out: it has not.
        let exit = bars[last].close;
        let pnl = (exit - h.entry) * h.units - settings.commission;
        let r = h.stop.map(|stop| pnl / ((h.entry - stop) * h.units)).filter(|r| r.is_finite());
        trades.push(Trade {
            entry_at: h.entry_at,
            exit_at: last,
            entry_ts: bars[h.entry_at].ts,
            exit_ts: bars[last].ts,
            entry: h.entry,
            exit,
            stop: h.stop,
            target: h.target,
            units: h.units,
            pnl,
            r,
            reason: Reason::Open,
        });
    }

    let stats = stats(&trades, &equity, settings.account, bars, from, to, held_bars);
    Report { trades, equity, from, to, stats, signal_pending }
}

fn stats(trades: &[Trade], equity: &[f64], account: f64, bars: &[Bar], from: usize, to: usize, held: usize) -> Stats {
    let closed: Vec<&Trade> = trades.iter().filter(|t| t.reason != Reason::Open).collect();
    let count = closed.len();
    let wins = closed.iter().filter(|t| t.pnl > 0.0).count();
    let gross_win: f64 = closed.iter().filter(|t| t.pnl > 0.0).map(|t| t.pnl).sum();
    let gross_loss: f64 = -closed.iter().filter(|t| t.pnl < 0.0).map(|t| t.pnl).sum::<f64>();
    let rs: Vec<f64> = closed.iter().filter_map(|t| t.r).collect();
    let mut peak = account;
    let mut deepest = 0.0f64;
    for value in equity {
        peak = peak.max(*value);
        if peak > 0.0 {
            deepest = deepest.min((value - peak) / peak * 100.0);
        }
    }
    let last = equity.last().copied().unwrap_or(account);
    let tested = to.saturating_sub(from);
    let buy_hold_pct = match (bars.get(from), to.checked_sub(1).and_then(|l| bars.get(l))) {
        (Some(first), Some(end)) if first.close > 0.0 && tested > 0 => (end.close / first.close - 1.0) * 100.0,
        _ => 0.0,
    };
    let average = |values: &[f64]| (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64);
    Stats {
        trades: count,
        wins,
        win_rate: if count == 0 { 0.0 } else { wins as f64 / count as f64 * 100.0 },
        average_r: average(&rs),
        expectancy: if count == 0 { 0.0 } else { closed.iter().map(|t| t.pnl).sum::<f64>() / count as f64 },
        profit_factor: (gross_loss > 0.0).then(|| gross_win / gross_loss),
        net_profit: last - account,
        return_pct: if account > 0.0 { (last / account - 1.0) * 100.0 } else { 0.0 },
        max_drawdown_pct: deepest,
        exposure_pct: if tested == 0 { 0.0 } else { held as f64 / tested as f64 * 100.0 },
        buy_hold_pct,
        average_bars: average(&closed.iter().map(|t| (t.exit_at - t.entry_at) as f64).collect::<Vec<_>>())
            .unwrap_or(0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bars::Timeframe;

    fn bar(i: usize, open: f64, high: f64, low: f64, close: f64) -> Bar {
        Bar { ts: i as i64 * 86_400, open, high, low, close, volume: 1.0 }
    }

    /// A flat open and close at each price, a point either side.
    fn flat(closes: &[f64]) -> Vec<Bar> {
        closes.iter().enumerate().map(|(i, &c)| bar(i, c, c + 1.0, c - 1.0, c)).collect()
    }

    const INPUTS: Inputs = Inputs { session_origin: 0, timeframe: Timeframe::days(1) };

    fn free(account: f64) -> Settings {
        Settings { account, stake: Stake::Percent(1.0), commission: 0.0, slippage: 0.0 }
    }

    fn strategy(entry: &str, exit: Option<&str>) -> Strategy {
        Strategy {
            entry: Rule::parse(entry).unwrap(),
            exit: exit.map(|e| Rule::parse(e).unwrap()),
            stop: None,
            target: None,
        }
    }

    #[test]
    fn stops_and_targets_read_the_way_people_write_them() {
        assert_eq!(Stop::parse("2*atr(14)"), Ok(Stop::Atr { multiple: 2.0, period: 14 }));
        assert_eq!(Stop::parse("1.5 ATR(10)"), Ok(Stop::Atr { multiple: 1.5, period: 10 }));
        assert_eq!(Stop::parse("atr"), Ok(Stop::Atr { multiple: 1.0, period: 14 }));
        assert_eq!(Stop::parse("3%"), Ok(Stop::Percent(3.0)));
        assert!(Stop::parse("3").is_err() && Stop::parse("0%").is_err() && Stop::parse("2*atr(0)").is_err());
        assert_eq!(Target::parse("2R"), Ok(Target::R(2.0)));
        assert_eq!(Target::parse("8 %"), Ok(Target::Percent(8.0)));
        assert!(Target::parse("2").is_err());
        assert_eq!(Stop::parse("2*atr(14)").unwrap().spell(), "2*atr(14)");
        assert_eq!(Target::R(1.5).spell(), "1.5R");
    }

    #[test]
    fn a_signal_fills_at_the_next_open_and_the_exit_rule_closes_it() {
        // Close crosses above 10.5 on bar 2, back below on bar 5.
        let mut b = flat(&[10.0, 10.0, 11.0, 12.0, 13.0, 10.0, 10.0]);
        b[3].open = 11.5;
        b[6].open = 9.5;
        let s = strategy("close crosses_above 10.5", Some("close < 10.5"));
        let report = run(&s, &b, &INPUTS, &free(1150.0), 0..b.len());
        assert_eq!(report.trades.len(), 1);
        let t = &report.trades[0];
        assert_eq!((t.entry_at, t.entry, t.exit_at, t.exit, t.reason), (3, 11.5, 6, 9.5, Reason::Rule));
        assert_eq!(t.units, 100.0, "the whole account, in whole shares, with no stop");
        assert_eq!(t.pnl, -200.0);
        assert_eq!(t.r, None);
        assert_eq!(report.stats.trades, 1);
        assert_eq!(report.stats.net_profit, -200.0);
        assert_eq!(report.equity.len(), b.len() - 1, "from the first bar the rule can answer");
    }

    #[test]
    fn a_stop_sizes_the_trade_and_a_gap_fills_at_the_open() {
        let mut b = flat(&[10.0, 10.0, 11.0, 12.0, 12.0, 12.0]);
        b[3].open = 10.0;
        b[4] = bar(4, 9.0, 9.5, 8.0, 9.0);
        let mut s = strategy("close crosses_above 10.5", None);
        s.stop = Some(Stop::Percent(5.0));
        let report = run(&s, &b, &INPUTS, &free(10_000.0), 0..b.len());
        let t = &report.trades[0];
        assert_eq!(t.stop, Some(9.5));
        // 1% of 10,000 = 100 at risk over 0.5 a share.
        assert_eq!(t.units, 200.0);
        assert_eq!((t.exit_at, t.exit, t.reason), (4, 9.0, Reason::Stop), "opened below the stop");
        assert_eq!(t.r, Some(-2.0));
    }

    #[test]
    fn a_target_in_r_and_a_bar_reaching_both_counts_as_stopped() {
        let mut b = flat(&[10.0, 10.0, 11.0, 10.0, 10.0, 10.0]);
        b[3] = bar(3, 10.0, 10.5, 9.9, 10.2);
        b[4] = bar(4, 10.2, 11.2, 10.1, 11.0);
        let mut s = strategy("close crosses_above 10.5", None);
        s.stop = Some(Stop::Percent(5.0));
        s.target = Some(Target::R(2.0));
        let report = run(&s, &b, &INPUTS, &free(10_000.0), 0..b.len());
        let t = &report.trades[0];
        assert_eq!(t.target, Some(11.0));
        assert_eq!((t.exit_at, t.exit, t.reason), (4, 11.0, Reason::Target));
        assert!((t.r.unwrap() - 2.0).abs() < 1e-9);

        let mut both = b.clone();
        both[4] = bar(4, 10.2, 11.2, 9.0, 11.0);
        let report = run(&s, &both, &INPUTS, &free(10_000.0), 0..both.len());
        assert_eq!(report.trades[0].reason, Reason::Stop);
    }

    #[test]
    fn costs_come_off_every_fill() {
        let mut b = flat(&[10.0, 10.0, 11.0, 12.0, 13.0, 10.0, 10.0]);
        b[3].open = 10.0;
        b[6].open = 10.0;
        let s = strategy("close crosses_above 10.5", Some("close < 10.5"));
        let settings = Settings { account: 1002.0, stake: Stake::Percent(1.0), commission: 1.0, slippage: 0.01 };
        let t = run(&s, &b, &INPUTS, &settings, 0..b.len()).trades[0].clone();
        assert!((t.entry - 10.1).abs() < 1e-9 && (t.exit - 9.9).abs() < 1e-9);
        assert_eq!(t.units, 99.0);
        assert!((t.pnl - (-0.2 * 99.0 - 2.0)).abs() < 1e-9);
    }

    #[test]
    fn what_is_still_open_is_valued_and_kept_apart() {
        let b = flat(&[10.0, 10.0, 11.0, 12.0, 14.0]);
        let s = strategy("close crosses_above 10.5", None);
        let report = run(&s, &b, &INPUTS, &free(1100.0), 0..b.len());
        assert_eq!(report.trades.len(), 1);
        assert_eq!(report.trades[0].reason, Reason::Open);
        assert_eq!(report.stats.trades, 0, "an open trade is not a result yet");
        assert!(report.stats.return_pct > 0.0);
        let pending = run(&s, &b[..3], &INPUTS, &free(1000.0), 0..3);
        assert!(pending.signal_pending && pending.trades.is_empty());
    }

    #[test]
    fn the_figures_add_up() {
        // Up through 10.5 and back, twice: one win, one loss.
        let mut b = flat(&[10.0, 11.0, 12.0, 10.0, 10.0, 11.0, 10.0, 9.0, 9.0]);
        for (i, open) in [(2, 11.0), (4, 12.0), (6, 11.0), (8, 9.0)] {
            b[i].open = open;
        }
        let s = strategy("close crosses_above 10.5", Some("close < 10.5"));
        let report = run(&s, &b, &INPUTS, &free(1100.0), 0..b.len());
        let st = &report.stats;
        assert_eq!((st.trades, st.wins), (2, 1));
        assert_eq!(st.win_rate, 50.0);
        assert_eq!(report.trades[0].pnl, 100.0);
        assert!(st.profit_factor.is_some());
        assert!(st.max_drawdown_pct < 0.0);
        assert!(st.exposure_pct > 0.0 && st.exposure_pct < 100.0);
        assert!((st.buy_hold_pct - (9.0 / 11.0 - 1.0) * 100.0).abs() < 1e-9, "from the first bar the rule answers");
    }

    #[test]
    fn a_window_trades_only_inside_it() {
        let b = flat(&[10.0, 11.0, 10.0, 11.0, 10.0, 11.0, 10.0]);
        let s = strategy("close crosses_above 10.5", Some("close < 10.5"));
        let all = run(&s, &b, &INPUTS, &free(1000.0), 0..b.len());
        let late = run(&s, &b, &INPUTS, &free(1000.0), 4..b.len());
        assert!(all.trades.len() > late.trades.len());
        assert!(late.trades.iter().all(|t| t.entry_at >= 4));
    }
}
