//! What the app lays over the bars that is neither a drawing nor a price
//! mark: where a rule alert fired, and the trades of a backtest. Plain data,
//! filled by the app and drawn by the chart.

/// Where a rule fired, for a chart to mark: the last moment of the bar it
/// fired on — so a chart of any resolution puts it on the bar that holds
/// that moment — and what to call it.
#[derive(Clone, PartialEq, Debug)]
pub struct Marker {
    pub ts: i64,
    pub label: String,
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
