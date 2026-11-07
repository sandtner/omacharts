//! Conditions over bars: `sma(20) crosses_above sma(50)`, `rsi(14) < 30`,
//! `close > vwap and rsi(14) > 50`.
//!
//! The building block an indicator alert fires on today and a strategy's
//! entry and exit rules will be written in later. A rule is read from the
//! text a person types, computed over a chart's bars with the same indicator
//! functions the chart draws with, and asked about one bar at a time — so a
//! rule that holds on the chart holds here, on the same bar.
//!
//! Nothing here fetches or stores. The caller hands over bars (closed ones,
//! plus the one forming if it wants live answers) and decides which bar to
//! ask about.
//!
//! # Grammar
//!
//! ```text
//! rule    := all ("or" all)*
//! all     := clause ("and" clause)*
//! clause  := operand op operand
//! operand := number | source
//! source  := name ["(" number ("," number)* ")"] ["." part]
//! op      := crosses_above | crosses_below | crosses | > | < | >= | <=
//! ```
//!
//! `and` binds tighter than `or`, and there are no brackets: a rule that
//! needs them is a strategy, and a strategy has room for several rules.

use std::fmt;

use crate::bars::{Bar, Timeframe};
use crate::indicators::{self, bollinger, oscillators, vwap, Reset};

/// A value per bar that a rule can compare.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Source {
    Open,
    High,
    Low,
    Close,
    Volume,
    Sma(usize),
    Ema(usize),
    Rsi(usize),
    Atr(usize),
    Macd { fast: usize, slow: usize, signal: usize, part: MacdPart },
    Stoch { period: usize, k_smooth: usize, d_period: usize, part: StochPart },
    Bollinger { period: usize, deviations: f64, part: BandPart },
    /// Reset each session, as a chart's VWAP is by default. `deviations`
    /// matters only to the bands.
    Vwap { deviations: f64, part: BandPart },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MacdPart {
    Line,
    Signal,
    Histogram,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StochPart {
    K,
    D,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BandPart {
    Basis,
    Upper,
    Lower,
}

/// One side of a comparison.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Operand {
    Value(f64),
    Series(Source),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    /// Above now, at or below on the bar before: TradingView's `ta.crossover`.
    CrossesAbove,
    /// Below now, at or above on the bar before.
    CrossesBelow,
    /// Either of the two.
    Crosses,
    Greater,
    Less,
    GreaterEq,
    LessEq,
}

impl Op {
    pub fn key(self) -> &'static str {
        match self {
            Op::CrossesAbove => "crosses_above",
            Op::CrossesBelow => "crosses_below",
            Op::Crosses => "crosses",
            Op::Greater => ">",
            Op::Less => "<",
            Op::GreaterEq => ">=",
            Op::LessEq => "<=",
        }
    }

    /// A move between two bars, rather than a place on one.
    pub fn is_cross(self) -> bool {
        matches!(self, Op::CrossesAbove | Op::CrossesBelow | Op::Crosses)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Clause {
    pub left: Operand,
    pub op: Op,
    pub right: Operand,
}

/// Any of several groups, each of which holds when all its clauses do.
#[derive(Clone, PartialEq, Debug)]
pub struct Rule {
    pub any: Vec<Vec<Clause>>,
}

/// What the indicators that take a session need to know about the chart.
#[derive(Clone, Copy, Debug)]
pub struct Inputs {
    /// The instrument's session open, as [`indicators::Context`] has it.
    pub session_origin: i64,
    pub timeframe: Timeframe,
}

/// Every source a rule reads, computed over the same bars.
pub struct Computed {
    sources: Vec<Source>,
    values: Vec<Vec<Option<f64>>>,
    len: usize,
}

impl Source {
    fn name(self) -> &'static str {
        match self {
            Source::Open => "open",
            Source::High => "high",
            Source::Low => "low",
            Source::Close => "close",
            Source::Volume => "volume",
            Source::Sma(_) => "sma",
            Source::Ema(_) => "ema",
            Source::Rsi(_) => "rsi",
            Source::Atr(_) => "atr",
            Source::Macd { .. } => "macd",
            Source::Stoch { .. } => "stoch",
            Source::Bollinger { .. } => "bb",
            Source::Vwap { .. } => "vwap",
        }
    }

    /// Bars of history the source needs before it has a first value. A
    /// rough figure, for saying why a rule has no answer yet.
    pub fn warm_up(self) -> usize {
        match self {
            Source::Open | Source::High | Source::Low | Source::Close | Source::Volume | Source::Vwap { .. } => 1,
            Source::Sma(n) | Source::Ema(n) | Source::Atr(n) => n,
            Source::Rsi(n) => n + 1,
            Source::Macd { slow, signal, .. } => slow + signal,
            Source::Stoch { period, k_smooth, d_period, .. } => period + k_smooth + d_period,
            Source::Bollinger { period, .. } => period,
        }
    }

    /// The value at every bar.
    pub fn compute(self, bars: &[Bar], inputs: &Inputs) -> Vec<Option<f64>> {
        let each = |f: fn(&Bar) -> f64| bars.iter().map(|b| Some(f(b)).filter(|v| v.is_finite())).collect();
        match self {
            Source::Open => each(|b| b.open),
            Source::High => each(|b| b.high),
            Source::Low => each(|b| b.low),
            Source::Close => each(|b| b.close),
            Source::Volume => each(|b| b.volume),
            Source::Sma(n) => indicators::sma(bars, n),
            Source::Ema(n) => indicators::ema(bars, n),
            Source::Rsi(n) => oscillators::rsi(bars, n),
            Source::Atr(n) => oscillators::atr(bars, n),
            Source::Macd { fast, slow, signal, part } => {
                let (line, signal, histogram) = oscillators::macd(bars, fast, slow, signal);
                match part {
                    MacdPart::Line => line,
                    MacdPart::Signal => signal,
                    MacdPart::Histogram => histogram,
                }
            }
            Source::Stoch { period, k_smooth, d_period, part } => {
                let (k, d) = oscillators::stochastic(bars, period, k_smooth, d_period);
                match part {
                    StochPart::K => k,
                    StochPart::D => d,
                }
            }
            Source::Bollinger { period, deviations, part } => {
                let mut bands = bollinger::default_bands();
                bands[0].deviations = deviations;
                pick(bollinger::compute(bars, period, &bands), part)
            }
            Source::Vwap { deviations, part } => {
                let mut bands = vwap::default_bands();
                bands.truncate(1);
                bands[0].deviations = deviations;
                bands[0].enabled = true;
                let reset = Reset::Session.effective_for(inputs.timeframe);
                pick(vwap::compute(bars, reset, inputs.session_origin, &bands), part)
            }
        }
    }
}

fn pick(bands: indicators::Bands, part: BandPart) -> Vec<Option<f64>> {
    match part {
        BandPart::Basis => bands.center,
        BandPart::Upper => bands.bands.into_iter().next().map(|b| b.upper).unwrap_or_default(),
        BandPart::Lower => bands.bands.into_iter().next().map(|b| b.lower).unwrap_or_default(),
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = self.name();
        match *self {
            Source::Open | Source::High | Source::Low | Source::Close | Source::Volume => write!(f, "{name}"),
            Source::Sma(n) | Source::Ema(n) | Source::Rsi(n) | Source::Atr(n) => write!(f, "{name}({n})"),
            Source::Macd { fast, slow, signal, part } => {
                write!(f, "{name}({fast},{slow},{signal})")?;
                match part {
                    MacdPart::Line => Ok(()),
                    MacdPart::Signal => write!(f, ".signal"),
                    MacdPart::Histogram => write!(f, ".hist"),
                }
            }
            Source::Stoch { period, k_smooth, d_period, part } => {
                write!(f, "{name}({period},{k_smooth},{d_period})")?;
                match part {
                    StochPart::K => Ok(()),
                    StochPart::D => write!(f, ".d"),
                }
            }
            Source::Bollinger { period, deviations, part } => {
                write!(f, "{name}({period},{})", number(deviations))?;
                write_band(f, part)
            }
            Source::Vwap { deviations, part } => match part {
                BandPart::Basis => write!(f, "{name}"),
                _ => {
                    write!(f, "{name}({})", number(deviations))?;
                    write_band(f, part)
                }
            },
        }
    }
}

fn write_band(f: &mut fmt::Formatter<'_>, part: BandPart) -> fmt::Result {
    match part {
        BandPart::Basis => Ok(()),
        BandPart::Upper => write!(f, ".upper"),
        BandPart::Lower => write!(f, ".lower"),
    }
}

/// A figure as short as it reads: `2`, `1.5`, `0.25`.
fn number(value: f64) -> String {
    format!("{value}")
}

impl fmt::Display for Operand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Operand::Value(v) => write!(f, "{}", number(*v)),
            Operand::Series(s) => write!(f, "{s}"),
        }
    }
}

impl fmt::Display for Clause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} {}", self.left, self.op.key(), self.right)
    }
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let groups: Vec<String> = self
            .any
            .iter()
            .map(|all| all.iter().map(ToString::to_string).collect::<Vec<_>>().join(" and "))
            .collect();
        write!(f, "{}", groups.join(" or "))
    }
}

/// A word, a figure or a mark, as the rule is read.
#[derive(Clone, PartialEq, Debug)]
enum Token {
    Word(String),
    Number(f64),
    Open,
    Close,
    Comma,
    Dot,
    Mark(String),
}

fn tokens(text: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_alphabetic() || c == '_' || c == '%' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '_' | '-' | '%')) {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect::<String>().to_lowercase().replace('-', "_");
            out.push(Token::Word(word));
        } else if c.is_ascii_digit()
            || (c == '.' && chars.get(i + 1).is_some_and(char::is_ascii_digit) && !matches!(out.last(), Some(Token::Close | Token::Word(_))))
            || (c == '-' && chars.get(i + 1).is_some_and(|n| n.is_ascii_digit() || *n == '.'))
        {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            let figure: String = chars[start..i].iter().collect();
            let value: f64 = figure.parse().map_err(|_| format!("{figure:?} is not a number"))?;
            out.push(Token::Number(value));
        } else {
            i += 1;
            out.push(match c {
                '(' => Token::Open,
                ')' => Token::Close,
                ',' => Token::Comma,
                '.' => Token::Dot,
                '>' | '<' | '=' | '!' => {
                    let mut mark = c.to_string();
                    if chars.get(i) == Some(&'=') {
                        mark.push('=');
                        i += 1;
                    }
                    Token::Mark(mark)
                }
                _ => return Err(format!("{c:?} has no meaning in a rule")),
            });
        }
    }
    Ok(out)
}

struct Reader {
    tokens: Vec<Token>,
    at: usize,
}

impl Reader {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn next(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.at).cloned();
        self.at += 1;
        token
    }

    fn word_is(&self, word: &str) -> bool {
        matches!(self.peek(), Some(Token::Word(w)) if w == word)
    }

    fn rule(&mut self) -> Result<Rule, String> {
        let mut any = vec![self.all()?];
        while self.word_is("or") {
            self.at += 1;
            any.push(self.all()?);
        }
        if let Some(token) = self.peek() {
            return Err(format!("did not expect {} there; join clauses with `and` or `or`", spell(token)));
        }
        Ok(Rule { any })
    }

    fn all(&mut self) -> Result<Vec<Clause>, String> {
        let mut all = vec![self.clause()?];
        while self.word_is("and") {
            self.at += 1;
            all.push(self.clause()?);
        }
        Ok(all)
    }

    fn clause(&mut self) -> Result<Clause, String> {
        let left = self.operand()?;
        let op = self.op()?;
        let right = self.operand()?;
        if let (Operand::Value(_), Operand::Value(_)) = (left, right) {
            return Err("a clause compares a series with something; two numbers never change".into());
        }
        Ok(Clause { left, op, right })
    }

    fn op(&mut self) -> Result<Op, String> {
        let token = self.next().ok_or("the rule ends where a comparison was expected")?;
        let op = match &token {
            Token::Mark(m) => match m.as_str() {
                ">" => Op::Greater,
                "<" => Op::Less,
                ">=" => Op::GreaterEq,
                "<=" => Op::LessEq,
                _ => return Err(format!("{m:?} is not a comparison; use >, <, >=, <= or crosses_above")),
            },
            Token::Word(w) => match w.as_str() {
                "crosses_above" | "crossover" | "cross_up" | "crosses_up" => Op::CrossesAbove,
                "crosses_below" | "crossunder" | "cross_down" | "crosses_down" => Op::CrossesBelow,
                "crosses" | "cross" => match self.peek() {
                    Some(Token::Word(w)) if w == "above" || w == "up" || w == "over" => {
                        self.at += 1;
                        Op::CrossesAbove
                    }
                    Some(Token::Word(w)) if w == "below" || w == "down" || w == "under" => {
                        self.at += 1;
                        Op::CrossesBelow
                    }
                    _ => Op::Crosses,
                },
                "above" => Op::Greater,
                "below" => Op::Less,
                _ => return Err(format!("{w:?} is not a comparison; use >, <, >=, <=, crosses, crosses_above or crosses_below")),
            },
            other => return Err(format!("expected a comparison, not {}", spell(other))),
        };
        Ok(op)
    }

    fn operand(&mut self) -> Result<Operand, String> {
        match self.next() {
            Some(Token::Number(v)) => Ok(Operand::Value(v)),
            Some(Token::Word(name)) => Ok(Operand::Series(self.source(&name)?)),
            Some(other) => Err(format!("expected a number or a series such as sma(20), not {}", spell(&other))),
            None => Err("the rule ends where a number or a series was expected".into()),
        }
    }

    fn source(&mut self, name: &str) -> Result<Source, String> {
        let mut args = Vec::new();
        if self.peek() == Some(&Token::Open) {
            self.at += 1;
            loop {
                match self.next() {
                    Some(Token::Number(v)) => args.push(v),
                    Some(Token::Close) if args.is_empty() => break,
                    other => {
                        return Err(format!(
                            "{name}(…) takes numbers, not {}",
                            other.as_ref().map_or("the end of the rule".into(), spell)
                        ));
                    }
                }
                match self.next() {
                    Some(Token::Comma) => continue,
                    Some(Token::Close) => break,
                    other => {
                        return Err(format!(
                            "expected `,` or `)` in {name}(…), not {}",
                            other.as_ref().map_or("the end of the rule".into(), spell)
                        ));
                    }
                }
            }
        }
        let mut part = None;
        if self.peek() == Some(&Token::Dot) {
            self.at += 1;
            match self.next() {
                Some(Token::Word(w)) => part = Some(w),
                _ => return Err(format!("{name}. needs a part after the dot, such as .signal or .upper")),
            }
        }
        build(name, &args, part.as_deref())
    }
}

fn spell(token: &Token) -> String {
    match token {
        Token::Word(w) => format!("`{w}`"),
        Token::Number(v) => format!("`{v}`"),
        Token::Open => "`(`".into(),
        Token::Close => "`)`".into(),
        Token::Comma => "`,`".into(),
        Token::Dot => "`.`".into(),
        Token::Mark(m) => format!("`{m}`"),
    }
}

/// A whole count of bars, at least one.
fn period(name: &str, value: Option<f64>, default: Option<usize>) -> Result<usize, String> {
    match value {
        None => default.ok_or_else(|| format!("{name} needs a length, such as {name}(20)")),
        Some(v) if v >= 1.0 && v.fract() == 0.0 && v <= 5000.0 => Ok(v as usize),
        Some(v) => Err(format!("{name}'s length is a whole number of bars from 1 to 5000, not {v}")),
    }
}

fn deviations(name: &str, value: Option<f64>) -> Result<f64, String> {
    match value {
        None => Ok(2.0),
        Some(v) if v > 0.0 && v <= bollinger::MAX_DEVIATIONS => Ok(v),
        Some(v) => Err(format!("{name}'s deviations are more than 0 and at most {}, not {v}", bollinger::MAX_DEVIATIONS)),
    }
}

fn build(name: &str, args: &[f64], part: Option<&str>) -> Result<Source, String> {
    let at = |i: usize| args.get(i).copied();
    let most = |n: usize| match args.len() > n {
        true => Err(format!("{name} takes at most {n} number{}", if n == 1 { "" } else { "s" })),
        false => Ok(()),
    };
    let no_part = |source: Source| match part {
        None => Ok(source),
        Some(p) => Err(format!("{name} has no part called .{p}")),
    };
    let band = |p: Option<&str>| match p {
        None | Some("basis" | "middle" | "mid" | "center" | "centre") => Ok(BandPart::Basis),
        Some("upper" | "top" | "up") => Ok(BandPart::Upper),
        Some("lower" | "bottom" | "low" | "down") => Ok(BandPart::Lower),
        Some(p) => Err(format!("{name} has .basis, .upper and .lower, not .{p}")),
    };
    match name {
        "open" | "high" | "low" | "close" | "price" | "volume" => {
            most(0)?;
            no_part(match name {
                "open" => Source::Open,
                "high" => Source::High,
                "low" => Source::Low,
                "volume" => Source::Volume,
                _ => Source::Close,
            })
        }
        "sma" | "ma" => {
            most(1)?;
            no_part(Source::Sma(period("sma", at(0), None)?))
        }
        "ema" => {
            most(1)?;
            no_part(Source::Ema(period("ema", at(0), None)?))
        }
        "rsi" => {
            most(1)?;
            no_part(Source::Rsi(period("rsi", at(0), Some(14))?))
        }
        "atr" => {
            most(1)?;
            no_part(Source::Atr(period("atr", at(0), Some(14))?))
        }
        "macd" => {
            most(3)?;
            let part = match part {
                None | Some("line" | "macd") => MacdPart::Line,
                Some("signal" | "sig") => MacdPart::Signal,
                Some("hist" | "histogram") => MacdPart::Histogram,
                Some(p) => return Err(format!("macd has .line, .signal and .hist, not .{p}")),
            };
            let (fast, slow, signal) =
                (period("macd", at(0), Some(12))?, period("macd", at(1), Some(26))?, period("macd", at(2), Some(9))?);
            if fast >= slow {
                return Err(format!("macd's fast length {fast} has to be shorter than its slow length {slow}"));
            }
            Ok(Source::Macd { fast, slow, signal, part })
        }
        "stoch" | "stochastic" => {
            most(3)?;
            let part = match part {
                None | Some("k" | "%k") => StochPart::K,
                Some("d" | "%d") => StochPart::D,
                Some(p) => return Err(format!("stoch has .k and .d, not .{p}")),
            };
            Ok(Source::Stoch {
                period: period("stoch", at(0), Some(14))?,
                k_smooth: period("stoch", at(1), Some(3))?,
                d_period: period("stoch", at(2), Some(3))?,
                part,
            })
        }
        "bb" | "bollinger" | "boll" => {
            most(2)?;
            Ok(Source::Bollinger {
                period: period("bb", at(0), Some(20))?,
                deviations: deviations("bb", at(1))?,
                part: band(part)?,
            })
        }
        "vwap" => {
            most(1)?;
            Ok(Source::Vwap { deviations: deviations("vwap", at(0))?, part: band(part)? })
        }
        _ => Err(format!(
            "no series called {name:?}; there are close, open, high, low, volume, sma(n), ema(n), rsi(n), atr(n), macd, stoch, bb and vwap"
        )),
    }
}

impl Rule {
    pub fn parse(text: &str) -> Result<Rule, String> {
        let tokens = tokens(text)?;
        if tokens.is_empty() {
            return Err("a rule is empty; try `sma(20) crosses_above sma(50)` or `rsi(14) < 30`".into());
        }
        Reader { tokens, at: 0 }.rule()
    }

    /// Every series the rule reads, each once, in the order written.
    pub fn sources(&self) -> Vec<Source> {
        let mut out: Vec<Source> = Vec::new();
        for clause in self.any.iter().flatten() {
            for operand in [clause.left, clause.right] {
                if let Operand::Series(s) = operand
                    && !out.contains(&s)
                {
                    out.push(s);
                }
            }
        }
        out
    }

    /// Whether every way the rule can hold is a move between two bars. Such
    /// a rule is an event: it is true on the bar it happens and not after.
    /// One with a place in it — `rsi(14) > 70` — stays true while it is.
    pub fn is_event(&self) -> bool {
        self.any.iter().all(|all| all.iter().any(|c| c.op.is_cross()))
    }

    /// The bars of history needed before the rule can answer at all.
    pub fn warm_up(&self) -> usize {
        let deepest = self.sources().into_iter().map(Source::warm_up).max().unwrap_or(1);
        deepest + usize::from(self.any.iter().flatten().any(|c| c.op.is_cross()))
    }

    pub fn compute(&self, bars: &[Bar], inputs: &Inputs) -> Computed {
        let sources = self.sources();
        let values = sources.iter().map(|s| s.compute(bars, inputs)).collect();
        Computed { sources, values, len: bars.len() }
    }

    /// Whether it holds on bar `i`. A clause with a value missing — not
    /// enough history yet — does not hold.
    pub fn holds(&self, computed: &Computed, i: usize) -> bool {
        self.any.iter().any(|all| all.iter().all(|clause| computed.clause(clause, i).unwrap_or(false)))
    }

    /// Whether bar `i` is one where it starts to hold: every bar an event
    /// happens on, and the first bar of each stretch a place holds for.
    pub fn starts(&self, computed: &Computed, i: usize) -> bool {
        self.holds(computed, i) && (self.is_event() || i == 0 || !self.holds(computed, i - 1))
    }

    /// The bars it starts on, oldest first: what a backtest enters on.
    pub fn signals(&self, computed: &Computed) -> Vec<usize> {
        (0..computed.len).filter(|&i| self.starts(computed, i)).collect()
    }
}

impl Computed {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// A source's value at bar `i`.
    pub fn value(&self, source: Source, i: usize) -> Option<f64> {
        let at = self.sources.iter().position(|s| *s == source)?;
        self.values[at].get(i).copied().flatten()
    }

    /// Every source with its value at bar `i`, for saying why.
    pub fn values_at(&self, i: usize) -> Vec<(Source, Option<f64>)> {
        self.sources.iter().map(|s| (*s, self.value(*s, i))).collect()
    }

    fn operand(&self, operand: Operand, i: usize) -> Option<f64> {
        match operand {
            Operand::Value(v) => Some(v),
            Operand::Series(s) => self.value(s, i),
        }
    }

    fn clause(&self, clause: &Clause, i: usize) -> Option<bool> {
        let (a, b) = (self.operand(clause.left, i)?, self.operand(clause.right, i)?);
        let before = || -> Option<(f64, f64)> {
            let j = i.checked_sub(1)?;
            Some((self.operand(clause.left, j)?, self.operand(clause.right, j)?))
        };
        Some(match clause.op {
            Op::Greater => a > b,
            Op::Less => a < b,
            Op::GreaterEq => a >= b,
            Op::LessEq => a <= b,
            Op::CrossesAbove => {
                let (pa, pb) = before()?;
                a > b && pa <= pb
            }
            Op::CrossesBelow => {
                let (pa, pb) = before()?;
                a < b && pa >= pb
            }
            Op::Crosses => {
                let (pa, pb) = before()?;
                (a > b && pa <= pb) || (a < b && pa >= pb)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bars(closes: &[f64]) -> Vec<Bar> {
        closes
            .iter()
            .enumerate()
            .map(|(i, &c)| Bar { ts: i as i64 * 86_400, open: c, high: c + 1.0, low: c - 1.0, close: c, volume: 100.0 })
            .collect()
    }

    fn inputs() -> Inputs {
        Inputs { session_origin: 0, timeframe: Timeframe::days(1) }
    }

    #[test]
    fn rules_read_the_way_people_write_them() {
        let cases = [
            ("sma(20) crosses_above sma(50)", "sma(20) crosses_above sma(50)"),
            ("SMA(20) crosses above SMA(50)", "sma(20) crosses_above sma(50)"),
            ("rsi < 30", "rsi(14) < 30"),
            ("rsi(14) >= 70 and close > ema(200)", "rsi(14) >= 70 and close > ema(200)"),
            ("macd crosses macd.signal", "macd(12,26,9) crosses macd(12,26,9).signal"),
            ("price crosses-below bb(20, 2.5).lower", "close crosses_below bb(20,2.5).lower"),
            ("close > vwap or stoch.d < 20", "close > vwap or stoch(14,3,3).d < 20"),
            ("macd.hist crosses_above 0", "macd(12,26,9).hist crosses_above 0"),
            ("close above -1.5", "close > -1.5"),
            ("vwap(1.5).upper < high", "vwap(1.5).upper < high"),
        ];
        for (text, spelled) in cases {
            let rule = Rule::parse(text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(rule.to_string(), spelled, "{text}");
            assert_eq!(Rule::parse(spelled).unwrap(), rule, "spelled form reads back: {spelled}");
        }
    }

    #[test]
    fn a_bad_rule_says_what_is_wrong() {
        let wrong = |text: &str| Rule::parse(text).expect_err(text);
        assert!(wrong("").contains("empty"));
        assert!(wrong("sma crosses_above 5").contains("needs a length"));
        assert!(wrong("foo(3) > 1").contains("no series called"));
        assert!(wrong("rsi(14) = 30").contains("not a comparison"));
        assert!(wrong("rsi(14) > ").contains("ends where"));
        assert!(wrong("3 > 4").contains("two numbers"));
        assert!(wrong("macd(26,12) > 0").contains("shorter"));
        assert!(wrong("rsi(14) > 30 rsi(14) < 70").contains("`and` or `or`"));
        assert!(wrong("bb.side > close").contains(".basis"));
        assert!(wrong("sma(2.5) > close").contains("whole number"));
    }

    #[test]
    fn a_cross_is_true_on_the_bar_it_happens() {
        // 10, 10, 10, then a climb: the 2-bar average rises through the
        // 3-bar one on the first bar of the climb, and stays above.
        let b = bars(&[10.0, 10.0, 10.0, 11.0, 12.0, 13.0, 12.0, 10.0, 9.0]);
        let rule = Rule::parse("sma(2) crosses_above sma(3)").unwrap();
        let c = rule.compute(&b, &inputs());
        assert!(rule.is_event());
        assert_eq!(rule.signals(&c), vec![3]);
        let down = Rule::parse("sma(2) crosses_below sma(3)").unwrap();
        assert_eq!(down.signals(&down.compute(&b, &inputs())), vec![7]);
        let either = Rule::parse("sma(2) crosses sma(3)").unwrap();
        assert_eq!(either.signals(&either.compute(&b, &inputs())), vec![3, 7]);
    }

    #[test]
    fn a_place_starts_once_per_stretch() {
        let b = bars(&[5.0, 7.0, 8.0, 6.0, 9.0, 9.5]);
        let rule = Rule::parse("close > 6.5").unwrap();
        let c = rule.compute(&b, &inputs());
        assert!(!rule.is_event());
        assert!(rule.holds(&c, 2) && !rule.holds(&c, 3));
        assert_eq!(rule.signals(&c), vec![1, 4]);
    }

    #[test]
    fn and_binds_tighter_than_or() {
        let b = bars(&[5.0, 7.0, 8.0, 6.0, 9.0]);
        let rule = Rule::parse("close > 8.5 or close > 6 and close < 7.5").unwrap();
        let c = rule.compute(&b, &inputs());
        let held: Vec<usize> = (0..b.len()).filter(|&i| rule.holds(&c, i)).collect();
        assert_eq!(held, vec![1, 4]);
    }

    #[test]
    fn no_history_is_no_answer() {
        let b = bars(&[1.0, 2.0, 3.0]);
        let rule = Rule::parse("sma(50) < close").unwrap();
        let c = rule.compute(&b, &inputs());
        assert!((0..3).all(|i| !rule.holds(&c, i)));
        assert_eq!(c.value(Source::Sma(50), 2), None);
        assert_eq!(rule.warm_up(), 50);
        assert_eq!(Rule::parse("sma(2) crosses close").unwrap().warm_up(), 3);
    }

    #[test]
    fn indicators_come_from_the_chart_s_own_functions() {
        let closes: Vec<f64> = (0..60).map(|i| 100.0 + (i as f64 * 0.7).sin() * 5.0).collect();
        let b = bars(&closes);
        let rule = Rule::parse("rsi(14) > 50 and macd crosses macd.signal or bb.upper < high").unwrap();
        let c = rule.compute(&b, &inputs());
        assert_eq!(c.value(Source::Rsi(14), 59), oscillators::rsi(&b, 14)[59]);
        let (_, signal, _) = oscillators::macd(&b, 12, 26, 9);
        let s = Source::Macd { fast: 12, slow: 26, signal: 9, part: MacdPart::Signal };
        assert_eq!(c.value(s, 59), signal[59]);
        let upper = c.value(Source::Bollinger { period: 20, deviations: 2.0, part: BandPart::Upper }, 59).unwrap();
        let basis = Source::Bollinger { period: 20, deviations: 2.0, part: BandPart::Basis }.compute(&b, &inputs());
        assert!(upper > basis[59].unwrap());
        assert_eq!(c.values_at(59).len(), 5);
    }
}
