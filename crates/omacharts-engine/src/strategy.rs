//! Saved strategies: a backtest's rules kept under a name, to run again on
//! another symbol, another resolution or another day — and, later, to watch
//! live.
//!
//! Kept as the text they were written in, the way an alert keeps its rule:
//! a strategy this build cannot read is still listed, as it was, rather
//! than lost. [`Saved::strategy`] reads it when it is run.
//!
//! Stored as one JSON list in the settings, like the alerts. Every field
//! added later must default, so a list written now still reads.

use serde::{Deserialize, Serialize};

use crate::backtest::{Stop, Strategy, Target};
use crate::bars::Timeframe;
use crate::condition::Rule;

/// The settings key the strategies are stored under, as one JSON list.
pub const SETTING: &str = "strategies";

/// The longest name a strategy may have: a label in a list, not a note.
pub const NAME_LIMIT: usize = 60;

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Saved {
    pub id: i64,
    pub name: String,
    /// The rule to buy on.
    pub entry: String,
    /// The rule to sell on; empty for none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub exit: String,
    /// `2*atr(14)` or `3%`; empty for none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stop: String,
    /// `2R` or `8%`; empty for none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub target: String,
    /// The resolution it was written for. A run on a chart uses the chart's;
    /// this is what a command uses when none is named.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeframe: Option<Timeframe>,
    /// What it is for, in the owner's words.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// When it was saved, in seconds.
    pub created: i64,
}

impl Saved {
    /// Read the rules into a strategy the backtest can run, or say what is
    /// wrong with them.
    pub fn strategy(&self) -> Result<Strategy, String> {
        parts(&self.entry, &self.exit, &self.stop, &self.target)
    }

    /// The resolution it runs on when nothing else says: its own, or daily.
    pub fn resolution(&self) -> Timeframe {
        self.timeframe.unwrap_or(Timeframe::days(1))
    }

    /// One line: `#2 Trend · buy when … · sell when … · stop 2*atr(14) · 1D`.
    pub fn describe(&self) -> String {
        let mut words = vec![format!("#{} {}", self.id, self.name), format!("buy when {}", self.entry)];
        if !self.exit.is_empty() {
            words.push(format!("sell when {}", self.exit));
        }
        if !self.stop.is_empty() {
            words.push(format!("stop {}", self.stop));
        }
        if !self.target.is_empty() {
            words.push(format!("target {}", self.target));
        }
        words.push(self.resolution().key());
        if !self.note.is_empty() {
            words.push(format!("{:?}", self.note));
        }
        words.join(" · ")
    }

    /// The same rules, each spelled the way the rule reader spells it, so
    /// two ways of writing one strategy are saved as one.
    pub fn normalised(mut self) -> Result<Saved, String> {
        let strategy = self.strategy()?;
        self.entry = strategy.entry.to_string();
        self.exit = strategy.exit.map(|r| r.to_string()).unwrap_or_default();
        self.stop = strategy.stop.map(Stop::spell).unwrap_or_default();
        self.target = strategy.target.map(Target::spell).unwrap_or_default();
        self.name = check_name(&self.name)?;
        Ok(self)
    }
}

/// A strategy from its four parts as text: an entry rule, and at least one
/// way out. Empty parts are left out.
pub fn parts(entry: &str, exit: &str, stop: &str, target: &str) -> Result<Strategy, String> {
    let entry = match entry.trim() {
        "" => return Err("a strategy needs an entry rule: when to buy".into()),
        text => Rule::parse(text).map_err(|e| format!("entry: {e}"))?,
    };
    let exit = match exit.trim() {
        "" => None,
        text => Some(Rule::parse(text).map_err(|e| format!("exit: {e}"))?),
    };
    let stop = match stop.trim() {
        "" => None,
        text => Some(Stop::parse(text)?),
    };
    let target = match target.trim() {
        "" => None,
        text => Some(Target::parse(text)?),
    };
    if exit.is_none() && stop.is_none() && target.is_none() {
        return Err("a trade needs a way out: an exit rule, a stop or a target".into());
    }
    if matches!(target, Some(Target::R(_))) && stop.is_none() {
        return Err("a target in R is a multiple of the risk, which needs a stop".into());
    }
    Ok(Strategy { entry, exit, stop, target })
}

/// A name as it is kept: trimmed, not empty, not a number (which would
/// read as an id), and not too long.
pub fn check_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("a strategy needs a name".into());
    }
    if name.trim_start_matches('#').parse::<i64>().is_ok() {
        return Err(format!("{name:?} would read as a number; give the strategy a name with a letter in it"));
    }
    if name.chars().count() > NAME_LIMIT {
        return Err(format!("a name is at most {NAME_LIMIT} characters"));
    }
    Ok(name.to_string())
}

/// The strategy `text` names: `#3`, `3`, or its name in any case.
pub fn find<'a>(list: &'a [Saved], text: &str) -> Option<&'a Saved> {
    let text = text.trim();
    if let Ok(id) = text.trim_start_matches('#').parse::<i64>() {
        return list.iter().find(|s| s.id == id);
    }
    list.iter().find(|s| s.name.eq_ignore_ascii_case(text))
}

/// Whether `name` is taken by a strategy other than `except`.
pub fn taken(list: &[Saved], name: &str, except: Option<i64>) -> bool {
    list.iter().any(|s| Some(s.id) != except && s.name.eq_ignore_ascii_case(name.trim()))
}

/// Read the stored list; an unreadable row is no strategies rather than a
/// crash, left in place until the next write.
pub fn from_json(text: Option<&str>) -> Vec<Saved> {
    text.and_then(|t| serde_json::from_str(t).ok()).unwrap_or_default()
}

pub fn to_json(list: &[Saved]) -> String {
    serde_json::to_string(list).unwrap_or_else(|_| "[]".into())
}

pub fn next_id(list: &[Saved]) -> i64 {
    list.iter().map(|s| s.id).max().unwrap_or(0) + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(name: &str, entry: &str, exit: &str, stop: &str, target: &str) -> Saved {
        Saved {
            id: 0,
            name: name.into(),
            entry: entry.into(),
            exit: exit.into(),
            stop: stop.into(),
            target: target.into(),
            timeframe: None,
            note: String::new(),
            created: 0,
        }
    }

    #[test]
    fn a_strategy_reads_its_rules_or_says_what_is_wrong() {
        let ok = saved("Trend", "sma(20) crosses above sma(50)", "SMA(20) crosses below sma(50)", "2 atr", "");
        let s = ok.strategy().unwrap();
        assert_eq!(s.entry.to_string(), "sma(20) crosses_above sma(50)");
        assert_eq!(s.stop, Some(Stop::Atr { multiple: 2.0, period: 14 }));
        let n = ok.normalised().unwrap();
        assert_eq!((n.exit.as_str(), n.stop.as_str()), ("sma(20) crosses_below sma(50)", "2*atr(14)"));
        assert!(saved("x", "", "close < 1", "", "").strategy().unwrap_err().contains("entry rule"));
        assert!(saved("x", "close >", "", "3%", "").strategy().unwrap_err().starts_with("entry:"));
        assert!(saved("x", "close > 1", "", "", "").strategy().unwrap_err().contains("way out"));
        assert!(saved("x", "close > 1", "", "", "2R").strategy().unwrap_err().contains("needs a stop"));
        assert!(saved("x", "close > 1", "", "", "8%").strategy().is_ok());
    }

    #[test]
    fn names_are_kept_clean_and_found_in_any_case() {
        assert_eq!(check_name("  Trend SPY "), Ok("Trend SPY".into()));
        assert!(check_name(" ").is_err());
        assert!(check_name("#12").is_err() && check_name("7").is_err());
        assert!(check_name(&"x".repeat(NAME_LIMIT + 1)).is_err());
        let mut a = saved("Trend", "close > 1", "close < 1", "", "");
        a.id = 2;
        let mut b = saved("Dip buyer", "rsi(2) < 10", "close > sma(5)", "", "");
        b.id = 5;
        let list = vec![a, b];
        assert_eq!(find(&list, "dip BUYER").map(|s| s.id), Some(5));
        assert_eq!(find(&list, "#2").map(|s| s.id), Some(2));
        assert_eq!(find(&list, "5").map(|s| s.id), Some(5));
        assert!(find(&list, "nothing").is_none());
        assert!(taken(&list, "trend", None) && !taken(&list, "trend", Some(2)));
        assert_eq!(next_id(&list), 6);
    }

    #[test]
    fn the_list_survives_the_store() {
        let mut a = saved("Trend", "close > sma(200)", "close < sma(200)", "3%", "2R");
        a.id = 1;
        a.timeframe = Some(Timeframe::hours(4));
        a.note = "only in bull markets".into();
        let list = vec![a];
        assert_eq!(from_json(Some(&to_json(&list))), list);
        assert_eq!(from_json(Some("garbage")), Vec::<Saved>::new());
        assert_eq!(list[0].describe(), "#1 Trend · buy when close > sma(200) · sell when close < sma(200) · stop 3% · target 2R · 4h · \"only in bull markets\"");
    }
}
