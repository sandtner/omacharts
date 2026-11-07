//! A backtest as the window runs it: the form the dock edits, kept between
//! sessions, and what the chart is given to draw from the result.
//!
//! The simulation is the engine's ([`omacharts_engine::backtest`]). This
//! is the part between a form somebody is typing into and that function:
//! reading the fields, picking the bars the period covers, and turning the
//! trades into marks a chart can lay over its own bars.

use omacharts_engine::backtest::{self, Reason, Report, Settings, Stop, Strategy, Target};
use omacharts_engine::condition::{Inputs, Rule};
use omacharts_engine::position::{Risk, Stake};
use omacharts_engine::Bar;
use serde::{Deserialize, Serialize};

/// The settings key the dock's form is kept under.
pub const SETTING: &str = "backtest_request";

/// The periods the dock offers, as `--last` reads them; `all` and `custom`
/// are the two that are not a length.
pub const PERIODS: [&str; 8] = ["1mo", "3mo", "6mo", "1y", "2y", "5y", "all", "custom"];

/// What the dock's form says, as typed. Text rather than parsed values, so
/// a half-typed rule is kept as it was left rather than lost.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Request {
    pub entry: String,
    pub exit: String,
    pub stop: String,
    pub target: String,
    /// One of [`PERIODS`].
    pub period: String,
    /// `custom`'s ends, as days: `2026-01-01`.
    pub from: String,
    pub to: String,
    /// Empty is `config risk`'s.
    pub account: String,
    pub risk: String,
    pub commission: String,
    /// In percent.
    pub slippage: String,
}

impl Default for Request {
    /// The example everybody starts from: a trend follower with a stop.
    fn default() -> Request {
        Request {
            entry: "sma(20) crosses_above sma(50)".into(),
            exit: "sma(20) crosses_below sma(50)".into(),
            stop: "2*atr(14)".into(),
            target: String::new(),
            period: "all".into(),
            from: String::new(),
            to: String::new(),
            account: String::new(),
            risk: String::new(),
            commission: "0".into(),
            slippage: "0.05".into(),
        }
    }
}

impl Request {
    pub fn from_json(text: Option<&str>) -> Request {
        text.and_then(|t| serde_json::from_str(t).ok()).unwrap_or_default()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Which field a problem is in, so the form can mark it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Field {
    Entry,
    Exit,
    Stop,
    Target,
    Period,
    Account,
    Risk,
    Costs,
}

/// A form read into what the engine runs.
pub struct Plan {
    pub strategy: Strategy,
    pub settings: Settings,
    /// The account, with its currency, for saying amounts back.
    pub risk: Risk,
}

/// Read the form. `stored` is `config risk`, which empty money fields
/// fall back to.
pub fn plan(request: &Request, stored: &Risk) -> Result<Plan, (Field, String)> {
    let rule = |text: &str, field: Field| -> Result<Option<Rule>, (Field, String)> {
        match text.trim().is_empty() {
            true => Ok(None),
            false => Rule::parse(text).map(Some).map_err(|e| (field, e)),
        }
    };
    let entry = rule(&request.entry, Field::Entry)?.ok_or((Field::Entry, "an entry rule says when to buy".to_string()))?;
    let exit = rule(&request.exit, Field::Exit)?;
    let stop = match request.stop.trim() {
        "" => None,
        text => Some(Stop::parse(text).map_err(|e| (Field::Stop, e))?),
    };
    let target = match request.target.trim() {
        "" => None,
        text => Some(Target::parse(text).map_err(|e| (Field::Target, e))?),
    };
    if exit.is_none() && stop.is_none() && target.is_none() {
        return Err((Field::Exit, "a trade needs a way out: an exit rule, a stop or a target".into()));
    }
    if matches!(target, Some(Target::R(_))) && stop.is_none() {
        return Err((Field::Target, "a target in R is a multiple of the risk, which needs a stop".into()));
    }
    let mut risk = stored.clone();
    if !request.account.trim().is_empty() {
        risk.account = money(&request.account)
            .filter(|a| *a > 0.0)
            .ok_or((Field::Account, "the account is an amount more than zero".to_string()))?;
    }
    if !request.risk.trim().is_empty() {
        risk.stake = Stake::parse(&request.risk)
            .ok_or((Field::Risk, "the risk is a percentage such as 1% or an amount such as 250".to_string()))?;
    }
    let figure = |text: &str| match text.trim() {
        "" => Some(0.0),
        t => money(t).filter(|v| *v >= 0.0),
    };
    let commission = figure(&request.commission)
        .ok_or((Field::Costs, "commission and slippage are numbers, zero or more".to_string()))?;
    let slippage = figure(&request.slippage)
        .filter(|s| *s < 50.0)
        .ok_or((Field::Costs, "commission and slippage are numbers, zero or more".to_string()))?;
    Ok(Plan {
        strategy: Strategy { entry, exit, stop, target },
        settings: Settings { account: risk.account, stake: risk.stake, commission, slippage: slippage / 100.0 },
        risk,
    })
}

/// `12,500`, `12'500`, `12500.5`.
fn money(text: &str) -> Option<f64> {
    text.trim().replace([',', '\'', '_', ' '], "").parse::<f64>().ok().filter(|v| v.is_finite())
}

/// When a period that runs up to the last bar starts: `1mo`, `6w`, `90d`,
/// `2y` back from `last`, by the calendar for months and years. `None` for
/// something that is not a length.
pub fn period_start(text: &str, last: i64) -> Option<i64> {
    use chrono::{Local, Months, TimeZone};
    let t = text.trim().to_lowercase();
    let split = t.find(|c: char| !c.is_ascii_digit())?;
    let (count, unit) = t.split_at(split);
    let count: u32 = count.parse().ok().filter(|n| *n > 0)?;
    let end = Local.timestamp_opt(last, 0).single()?;
    let start = match unit {
        "d" | "day" | "days" => end - chrono::Duration::days(count as i64),
        "w" | "wk" | "week" | "weeks" => end - chrono::Duration::weeks(count as i64),
        "mo" | "m" | "month" | "months" => end.checked_sub_months(Months::new(count))?,
        "y" | "yr" | "year" | "years" => end.checked_sub_months(Months::new(count * 12))?,
        _ => return None,
    };
    Some(start.timestamp())
}

/// A day, `2026-01-01`, as the moment it starts here.
pub fn day(text: &str) -> Option<i64> {
    use chrono::TimeZone;
    chrono::NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d")
        .ok()?
        .and_hms_opt(0, 0, 0)
        .and_then(|t| chrono::Local.from_local_datetime(&t).earliest())
        .map(|t| t.timestamp())
}

/// The bars of `bars` the form's period covers, as indices.
pub fn window(request: &Request, bars: &[Bar]) -> Result<std::ops::Range<usize>, (Field, String)> {
    let Some(last) = bars.last() else { return Ok(0..0) };
    let at = |ts: i64| bars.partition_point(|b| b.ts < ts);
    let range = match request.period.as_str() {
        "all" | "" => 0..bars.len(),
        "custom" => {
            let bad = || (Field::Period, "the period's ends are days such as 2026-01-01".to_string());
            let from = match request.from.trim() {
                "" => 0,
                t => at(day(t).ok_or_else(bad)?),
            };
            let to = match request.to.trim() {
                "" => bars.len(),
                t => at(day(t).ok_or_else(bad)? + 86_400),
            };
            from..to
        }
        length => {
            let start = period_start(length, last.ts)
                .ok_or((Field::Period, format!("{length:?} is not a period such as 3mo or 1y")))?;
            at(start)..bars.len()
        }
    };
    match range.start < range.end {
        true => Ok(range),
        false => Err((Field::Period, "the period holds no bars".into())),
    }
}

/// Run a form over a chart's bars. The bars are closed ones: the caller
/// leaves out the one still forming.
pub fn run(plan: &Plan, request: &Request, bars: &[Bar], inputs: &Inputs) -> Result<Report, (Field, String)> {
    let window = window(request, bars)?;
    let report = backtest::run(&plan.strategy, bars, inputs, &plan.settings, window.clone());
    if report.from >= report.to {
        return Err((
            Field::Period,
            format!(
                "{} bars are not enough: the entry rule needs {} before it can answer",
                window.len(),
                plan.strategy.entry.warm_up()
            ),
        ));
    }
    Ok(report)
}

/// One trade as a chart draws it.
#[derive(Clone, PartialEq, Debug)]
pub struct TradeMark {
    pub entry_ts: i64,
    pub exit_ts: i64,
    pub entry: f64,
    pub exit: f64,
    pub stop: Option<f64>,
    pub target: Option<f64>,
    pub win: bool,
    pub open: bool,
    /// What the exit says: `+2.1R`, or `+3.4%` with no stop.
    pub label: String,
}

/// What a chart lays over its bars after a backtest.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Marks {
    /// The bars the backtest traded on, first and last.
    pub from_ts: i64,
    pub to_ts: i64,
    pub trades: Vec<TradeMark>,
    /// The trade picked in the list, drawn stronger.
    pub selected: Option<usize>,
}

pub fn marks(report: &Report, bars: &[Bar]) -> Marks {
    let trades = report
        .trades
        .iter()
        .map(|t| TradeMark {
            entry_ts: t.entry_ts,
            exit_ts: t.exit_ts,
            entry: t.entry,
            exit: t.exit,
            stop: t.stop,
            target: t.target,
            win: t.pnl > 0.0,
            open: t.reason == Reason::Open,
            label: label(t),
        })
        .collect();
    Marks {
        from_ts: bars.get(report.from).map_or(0, |b| b.ts),
        to_ts: report.to.checked_sub(1).and_then(|l| bars.get(l)).map_or(0, |b| b.ts),
        trades,
        selected: None,
    }
}

/// `+2.1R`, or the move in percent for a trade with no stop.
pub fn label(t: &backtest::Trade) -> String {
    match t.r {
        Some(r) => format!("{r:+.1}R"),
        None => format!("{:+.1}%", (t.exit / t.entry - 1.0) * 100.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bars(n: usize) -> Vec<Bar> {
        let start = 1_700_000_000 / 86_400 * 86_400;
        (0..n)
            .map(|i| {
                let c = 100.0 + (i as f64 * 0.3).sin() * 10.0;
                Bar { ts: start + i as i64 * 86_400, open: c, high: c + 1.0, low: c - 1.0, close: c, volume: 1.0 }
            })
            .collect()
    }

    #[test]
    fn the_form_reads_into_a_strategy_or_names_its_bad_field() {
        let stored = Risk::default();
        let ok = plan(&Request::default(), &stored).unwrap();
        assert_eq!(ok.strategy.entry.to_string(), "sma(20) crosses_above sma(50)");
        assert_eq!(ok.settings.account, 10_000.0);
        assert!((ok.settings.slippage - 0.0005).abs() < 1e-12);
        let wrong = |change: fn(&mut Request)| {
            let mut r = Request::default();
            change(&mut r);
            plan(&r, &stored).err().map(|(field, _)| field)
        };
        assert_eq!(wrong(|r| r.entry = "sma(".into()), Some(Field::Entry));
        assert_eq!(wrong(|r| r.entry.clear()), Some(Field::Entry));
        assert_eq!(wrong(|r| r.stop = "lots".into()), Some(Field::Stop));
        assert_eq!(wrong(|r| {
            r.exit.clear();
            r.stop.clear();
        }), Some(Field::Exit));
        assert_eq!(wrong(|r| {
            r.stop.clear();
            r.target = "2R".into();
        }), Some(Field::Target));
        assert_eq!(wrong(|r| r.account = "-5".into()), Some(Field::Account));
        assert_eq!(wrong(|r| r.risk = "much".into()), Some(Field::Risk));
        assert_eq!(wrong(|r| r.slippage = "x".into()), Some(Field::Costs));
        let own = Request { account: "25'000".into(), risk: "0.5%".into(), ..Request::default() };
        let p = plan(&own, &stored).unwrap();
        assert_eq!((p.settings.account, p.settings.stake), (25_000.0, Stake::Percent(0.5)));
    }

    #[test]
    fn a_period_picks_the_bars_it_covers() {
        let b = bars(400);
        let mut r = Request::default();
        assert_eq!(window(&r, &b).unwrap(), 0..400);
        r.period = "3mo".into();
        let w = window(&r, &b).unwrap();
        assert!((88..=94).contains(&w.len()), "{w:?}");
        r.period = "6w".into();
        assert_eq!(window(&r, &b).unwrap().len(), 43, "six weeks back, both ends included");
        r.period = "custom".into();
        r.from = "never".into();
        assert_eq!(window(&r, &b).map_err(|e| e.0), Err(Field::Period));
        r.from.clear();
        assert_eq!(window(&r, &b).unwrap(), 0..400);
        r.period = "fortnight".into();
        assert!(window(&r, &b).is_err());
        assert_eq!(period_start("2y", 1_700_000_000).map(|s| (1_700_000_000 - s) / 86_400), Some(730));
    }

    #[test]
    fn a_report_becomes_marks_the_chart_can_draw() {
        let b = bars(300);
        let mut r = Request {
            entry: "close crosses_above sma(10)".into(),
            exit: "close crosses_below sma(10)".into(),
            stop: "5%".into(),
            ..Request::default()
        };
        let p = plan(&r, &Risk::default()).unwrap();
        let inputs = Inputs { session_origin: 0, timeframe: omacharts_engine::Timeframe::days(1) };
        let report = run(&p, &r, &b, &inputs).unwrap();
        assert!(!report.trades.is_empty());
        let m = marks(&report, &b);
        assert_eq!(m.trades.len(), report.trades.len());
        assert_eq!(m.from_ts, b[report.from].ts);
        assert!(m.trades.iter().all(|t| t.label.ends_with('R') && t.exit_ts >= t.entry_ts));
        r.period = "custom".into();
        r.from = "2023-11-15".into();
        r.to = "2023-11-16".into();
        assert_eq!(run(&p, &r, &b, &inputs).map(|_| ()).map_err(|e| e.0), Err(Field::Period), "two bars cannot warm an sma(10)");
    }
}
