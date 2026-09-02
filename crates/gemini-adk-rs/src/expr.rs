//! `Expr` — a closed, serializable expression vocabulary over session state.
//!
//! Where [`Guard`](crate::flow::Guard) is the closed vocabulary for *boolean*
//! questions about state and marking, `Expr` is the closed vocabulary for
//! *values* computed from state: the language of computed (derived) state
//! variables authored as data. Every atom is a named, parameterized operation;
//! there are no closures, so an `Expr` round-trips through JSON, can be edited
//! in a UI, validated at load time ([`Expr::keys_read`] feeds the state-key
//! diff), and evaluated identically in the live runtime, the offline
//! simulator, and generated code.
//!
//! ```
//! use gemini_adk_rs::expr::Expr;
//! use gemini_adk_rs::state::State;
//!
//! // risk = 0.6 * overdue_ratio + 0.4 * missed_payments / 10
//! let expr: Expr = serde_json::from_value(serde_json::json!({
//!     "add": [
//!         {"mul": [{"const": 0.6}, {"key": "overdue_ratio"}]},
//!         {"mul": [{"const": 0.04}, {"key": "missed_payments"}]}
//!     ]
//! })).unwrap();
//!
//! let state = State::new();
//! state.set("overdue_ratio", 0.5).unwrap();
//! state.set("missed_payments", 3).unwrap();
//! assert_eq!(expr.eval(&state), Some(serde_json::json!(0.42)));
//! ```
//!
//! ## Evaluation semantics
//!
//! - Reads go through [`State::get`], so the `derived:` fallback applies —
//!   one computed variable can read another by its bare key.
//! - Arithmetic and comparison atoms are *strict*: a missing key or
//!   non-numeric operand makes the whole expression `None` (no write).
//! - Logic atoms (`all`/`any`/`not`) are *total*: a missing or non-boolean
//!   operand counts as `false`, mirroring `Guard::is_true` on an unset key.
//! - `coalesce` returns the first operand that evaluates to a value; `if`
//!   selects a branch on the truthiness of its condition.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::state::State;

/// A serializable expression over session state. See the module docs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Expr {
    /// A literal value.
    Const(Value),
    /// Read a state key (with the `derived:` fallback of [`State::get`]).
    Key(String),
    /// Numeric sum of all operands.
    Add(Vec<Expr>),
    /// Numeric product of all operands.
    Mul(Vec<Expr>),
    /// `a - b`.
    Sub(Box<Expr>, Box<Expr>),
    /// `a / b` (`None` when `b` is zero).
    Div(Box<Expr>, Box<Expr>),
    /// Numeric minimum of all operands.
    Min(Vec<Expr>),
    /// Numeric maximum of all operands.
    Max(Vec<Expr>),
    /// Structural equality of two values.
    Eq(Box<Expr>, Box<Expr>),
    /// `a > b` (numeric).
    Gt(Box<Expr>, Box<Expr>),
    /// `a >= b` (numeric).
    Gte(Box<Expr>, Box<Expr>),
    /// `a < b` (numeric).
    Lt(Box<Expr>, Box<Expr>),
    /// `a <= b` (numeric).
    Lte(Box<Expr>, Box<Expr>),
    /// `true` when every operand is `true` (missing counts as `false`).
    All(Vec<Expr>),
    /// `true` when any operand is `true` (missing counts as `false`).
    Any(Vec<Expr>),
    /// Boolean negation (missing counts as `false`, so `not` of it is `true`).
    Not(Box<Expr>),
    /// Branch on a condition's truthiness.
    If {
        /// Condition (truthy = boolean `true`).
        when: Box<Expr>,
        /// Value when the condition holds.
        then: Box<Expr>,
        /// Value otherwise.
        #[serde(rename = "else")]
        otherwise: Box<Expr>,
    },
    /// The first operand that evaluates to a value.
    Coalesce(Vec<Expr>),
    /// String concatenation of all operands (numbers/booleans stringified;
    /// missing operands contribute nothing).
    Concat(Vec<Expr>),
    /// How many of the named state keys are `true`.
    CountTrue(Vec<String>),
}

impl Expr {
    /// Evaluate against state. `None` means "no value" — a computed variable
    /// skips its write for this cycle.
    pub fn eval(&self, state: &State) -> Option<Value> {
        match self {
            Expr::Const(v) => Some(v.clone()),
            Expr::Key(k) => state.get::<Value>(k),
            Expr::Add(items) => nary(items, state, |acc, n| acc + n, 0.0),
            Expr::Mul(items) => nary(items, state, |acc, n| acc * n, 1.0),
            Expr::Sub(a, b) => Some(number(num(a, state)? - num(b, state)?)),
            Expr::Div(a, b) => {
                let d = num(b, state)?;
                if d == 0.0 {
                    None
                } else {
                    Some(number(num(a, state)? / d))
                }
            }
            Expr::Min(items) => fold_nums(items, state, f64::min),
            Expr::Max(items) => fold_nums(items, state, f64::max),
            Expr::Eq(a, b) => Some(Value::Bool(a.eval(state)? == b.eval(state)?)),
            Expr::Gt(a, b) => Some(Value::Bool(num(a, state)? > num(b, state)?)),
            Expr::Gte(a, b) => Some(Value::Bool(num(a, state)? >= num(b, state)?)),
            Expr::Lt(a, b) => Some(Value::Bool(num(a, state)? < num(b, state)?)),
            Expr::Lte(a, b) => Some(Value::Bool(num(a, state)? <= num(b, state)?)),
            Expr::All(items) => Some(Value::Bool(items.iter().all(|e| truthy(e, state)))),
            Expr::Any(items) => Some(Value::Bool(items.iter().any(|e| truthy(e, state)))),
            Expr::Not(e) => Some(Value::Bool(!truthy(e, state))),
            Expr::If {
                when,
                then,
                otherwise,
            } => {
                if truthy(when, state) {
                    then.eval(state)
                } else {
                    otherwise.eval(state)
                }
            }
            Expr::Coalesce(items) => items.iter().find_map(|e| e.eval(state)),
            Expr::Concat(items) => {
                let mut out = String::new();
                for item in items {
                    match item.eval(state) {
                        Some(Value::String(s)) => out.push_str(&s),
                        Some(Value::Null) | None => {}
                        Some(v) => out.push_str(&v.to_string()),
                    }
                }
                Some(Value::String(out))
            }
            Expr::CountTrue(keys) => Some(json!(
                keys.iter()
                    .filter(|k| state.get::<bool>(k).unwrap_or(false))
                    .count()
            )),
        }
    }

    /// Every state key this expression reads, recursively — the dependency
    /// set of a computed variable, and the load-time read universe for
    /// validation.
    pub fn keys_read(&self) -> BTreeSet<String> {
        let mut keys = BTreeSet::new();
        self.collect_keys(&mut keys);
        keys
    }

    fn collect_keys(&self, keys: &mut BTreeSet<String>) {
        match self {
            Expr::Const(_) => {}
            Expr::Key(k) => {
                keys.insert(k.clone());
            }
            Expr::Add(items)
            | Expr::Mul(items)
            | Expr::Min(items)
            | Expr::Max(items)
            | Expr::All(items)
            | Expr::Any(items)
            | Expr::Coalesce(items)
            | Expr::Concat(items) => {
                for item in items {
                    item.collect_keys(keys);
                }
            }
            Expr::Sub(a, b)
            | Expr::Div(a, b)
            | Expr::Eq(a, b)
            | Expr::Gt(a, b)
            | Expr::Gte(a, b)
            | Expr::Lt(a, b)
            | Expr::Lte(a, b) => {
                a.collect_keys(keys);
                b.collect_keys(keys);
            }
            Expr::Not(e) => e.collect_keys(keys),
            Expr::If {
                when,
                then,
                otherwise,
            } => {
                when.collect_keys(keys);
                then.collect_keys(keys);
                otherwise.collect_keys(keys);
            }
            Expr::CountTrue(ks) => keys.extend(ks.iter().cloned()),
        }
    }

    /// A compact human-readable rendering, for diagnostics and UI labels.
    pub fn describe(&self) -> String {
        match self {
            Expr::Const(v) => v.to_string(),
            Expr::Key(k) => k.clone(),
            Expr::Add(items) => infix(items, " + "),
            Expr::Mul(items) => infix(items, " * "),
            Expr::Sub(a, b) => format!("({} - {})", a.describe(), b.describe()),
            Expr::Div(a, b) => format!("({} / {})", a.describe(), b.describe()),
            Expr::Min(items) => format!("min({})", infix_bare(items)),
            Expr::Max(items) => format!("max({})", infix_bare(items)),
            Expr::Eq(a, b) => format!("({} == {})", a.describe(), b.describe()),
            Expr::Gt(a, b) => format!("({} > {})", a.describe(), b.describe()),
            Expr::Gte(a, b) => format!("({} >= {})", a.describe(), b.describe()),
            Expr::Lt(a, b) => format!("({} < {})", a.describe(), b.describe()),
            Expr::Lte(a, b) => format!("({} <= {})", a.describe(), b.describe()),
            Expr::All(items) => format!("all({})", infix_bare(items)),
            Expr::Any(items) => format!("any({})", infix_bare(items)),
            Expr::Not(e) => format!("!{}", e.describe()),
            Expr::If {
                when,
                then,
                otherwise,
            } => format!(
                "if {} then {} else {}",
                when.describe(),
                then.describe(),
                otherwise.describe()
            ),
            Expr::Coalesce(items) => format!("coalesce({})", infix_bare(items)),
            Expr::Concat(items) => format!("concat({})", infix_bare(items)),
            Expr::CountTrue(keys) => format!("count_true({})", keys.join(", ")),
        }
    }
}

fn num(e: &Expr, state: &State) -> Option<f64> {
    match e.eval(state)? {
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

fn truthy(e: &Expr, state: &State) -> bool {
    matches!(e.eval(state), Some(Value::Bool(true)))
}

/// Render an f64 as a JSON number, preferring integer form when exact.
fn number(n: f64) -> Value {
    if n.fract() == 0.0 && n.abs() < (i64::MAX as f64) {
        json!(n as i64)
    } else {
        json!(n)
    }
}

fn nary(items: &[Expr], state: &State, op: impl Fn(f64, f64) -> f64, init: f64) -> Option<Value> {
    let mut acc = init;
    for item in items {
        acc = op(acc, num(item, state)?);
    }
    Some(number(acc))
}

fn fold_nums(items: &[Expr], state: &State, op: impl Fn(f64, f64) -> f64) -> Option<Value> {
    let mut iter = items.iter();
    let mut acc = num(iter.next()?, state)?;
    for item in iter {
        acc = op(acc, num(item, state)?);
    }
    Some(number(acc))
}

fn infix(items: &[Expr], sep: &str) -> String {
    format!("({})", infix_sep(items, sep))
}

fn infix_bare(items: &[Expr]) -> String {
    infix_sep(items, ", ")
}

fn infix_sep(items: &[Expr], sep: &str) -> String {
    items
        .iter()
        .map(Expr::describe)
        .collect::<Vec<_>>()
        .join(sep)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with(pairs: &[(&str, Value)]) -> State {
        let state = State::new();
        for (k, v) in pairs {
            let _ = state.set(*k, v.clone());
        }
        state
    }

    #[test]
    fn arithmetic_is_strict_on_missing_keys() {
        let expr: Expr = serde_json::from_value(json!({
            "add": [{"key": "a"}, {"const": 1}]
        }))
        .unwrap();
        assert_eq!(expr.eval(&State::new()), None);
        assert_eq!(
            expr.eval(&state_with(&[("a", json!(2))])),
            Some(json!(3)),
            "integers stay integers"
        );
    }

    #[test]
    fn logic_is_total_on_missing_keys() {
        let expr: Expr = serde_json::from_value(json!({
            "any": [{"key": "missing"}, {"key": "set"}]
        }))
        .unwrap();
        assert_eq!(
            expr.eval(&state_with(&[("set", json!(true))])),
            Some(json!(true))
        );
        assert_eq!(expr.eval(&State::new()), Some(json!(false)));

        let not: Expr = serde_json::from_value(json!({"not": {"key": "missing"}})).unwrap();
        assert_eq!(not.eval(&State::new()), Some(json!(true)));
    }

    #[test]
    fn if_coalesce_concat_count() {
        let state = state_with(&[
            ("severity", json!("severe")),
            ("a", json!(true)),
            ("b", json!(false)),
        ]);
        let level: Expr = serde_json::from_value(json!({
            "if": {
                "when": {"eq": [{"key": "severity"}, {"const": "severe"}]},
                "then": {"const": "high"},
                "else": {"const": "normal"}
            }
        }))
        .unwrap();
        assert_eq!(level.eval(&state), Some(json!("high")));

        let fallback: Expr = serde_json::from_value(json!({
            "coalesce": [{"key": "nickname"}, {"key": "severity"}]
        }))
        .unwrap();
        assert_eq!(fallback.eval(&state), Some(json!("severe")));

        let line: Expr = serde_json::from_value(json!({
            "concat": [{"const": "severity="}, {"key": "severity"}]
        }))
        .unwrap();
        assert_eq!(line.eval(&state), Some(json!("severity=severe")));

        let count: Expr = serde_json::from_value(json!({"count_true": ["a", "b", "c"]})).unwrap();
        assert_eq!(count.eval(&state), Some(json!(1)));
    }

    #[test]
    fn division_by_zero_is_none() {
        let expr: Expr =
            serde_json::from_value(json!({"div": [{"const": 1}, {"const": 0}]})).unwrap();
        assert_eq!(expr.eval(&State::new()), None);
    }

    #[test]
    fn comparisons_and_bounds() {
        let state = state_with(&[("score", json!(0.8))]);
        let gt: Expr =
            serde_json::from_value(json!({"gt": [{"key": "score"}, {"const": 0.5}]})).unwrap();
        assert_eq!(gt.eval(&state), Some(json!(true)));
        let clamp: Expr = serde_json::from_value(json!({
            "min": [{"key": "score"}, {"const": 0.6}]
        }))
        .unwrap();
        assert_eq!(clamp.eval(&state), Some(json!(0.6)));
    }

    #[test]
    fn keys_read_is_the_full_dependency_set() {
        let expr: Expr = serde_json::from_value(json!({
            "if": {
                "when": {"any": [{"key": "a"}, {"not": {"key": "b"}}]},
                "then": {"add": [{"key": "c"}, {"const": 1}]},
                "else": {"count_true": ["d", "e"]}
            }
        }))
        .unwrap();
        let keys: Vec<String> = expr.keys_read().into_iter().collect();
        assert_eq!(keys, ["a", "b", "c", "d", "e"]);
    }

    #[test]
    fn reads_fall_back_to_derived_keys() {
        let state = state_with(&[("derived:risk", json!(0.9))]);
        let expr: Expr = serde_json::from_value(json!({"key": "risk"})).unwrap();
        assert_eq!(expr.eval(&state), Some(json!(0.9)));
    }

    #[test]
    fn serde_round_trips_and_describes() {
        let doc = json!({
            "add": [
                {"mul": [{"const": 0.6}, {"key": "overdue"}]},
                {"key": "penalty"}
            ]
        });
        let expr: Expr = serde_json::from_value(doc.clone()).unwrap();
        assert_eq!(serde_json::to_value(&expr).unwrap(), doc);
        assert_eq!(expr.describe(), "((0.6 * overdue) + penalty)");
    }
}
