//! Code execution against plain function calling on Gemini 3.8 Live.
//!
//! Three arms, the same instruction, the same five tools and the same commit
//! gate:
//!
//! - `tools`: the tools as function declarations.
//! - `server`: the same, plus Gemini's own `codeExecution` (Python in
//!   Google's sandbox, which cannot reach our tools).
//! - `code`: one tool, `run_code(code)`. The model writes Python that calls
//!   the five tools as functions; Monty runs it in-process, pausing at each
//!   call, and every call goes through the same tools and the same gate.
//!
//! Ten tasks with computed ground truth stress computed arguments, fan-out,
//! aggregation over 6, 22 and 120 rows, a dependent chain, and commits that
//! must wait for the caller's yes, including one where the caller pushes to
//! skip it. Each run records whether the answer and the calls were
//! right, the gate's refusals, tool executions, model round trips, latency
//! and prompt tokens.
//!
//! ```text
//! GEMINI_API_KEY=… cargo run --release -- [--runs N] [--arms tools,server,code] [--tasks t1,..] [--model NAME] [--json PATH]
//! ```

use std::time::{Duration, Instant};

use gemini_genai_rs::protocol::{
    ApiEndpoint, FunctionCall, FunctionDeclaration, FunctionResponse, ModelId, Part,
    ServerMessage, SessionConfig, Tool,
};
use gemini_genai_rs::session::SessionCommand;
use gemini_genai_rs::transport::{Codec, JsonCodec, Transport, TungsteniteTransport};
mod sdk;

use monty::{MontyRun, RunProgress};
use monty_types::{
    CompileOptions, ExcType, MontyException, MontyObject, ObjectRef, PrintWriter,
    ResourceLimits, ResourceTracker,
};
use serde_json::{Value, json};

const TODAY: &str = "Today is Sunday, 11 October 2026.";
const TURN_TIMEOUT: Duration = Duration::from_secs(60);

// ─── the domain: deterministic tools ───────────────────────────────────────

const SLOTS: [&str; 8] = [
    "17:30", "18:00", "18:30", "19:00", "19:30", "20:00", "20:30", "21:00",
];

/// Days since 1970-01-01 for a proleptic Gregorian date.
fn days(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Monday = 0.
fn weekday(date: &str) -> Option<i64> {
    let mut parts = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, d) = (parts.next()??, parts.next()??, parts.next()??);
    // 1970-01-01 was a Thursday (3).
    Some((days(y, m, d) + 3).rem_euclid(7))
}

fn availability(date: &str, party: i64) -> Option<Vec<&'static str>> {
    let w = weekday(date)?;
    Some(
        SLOTS
            .iter()
            .enumerate()
            .filter(|(i, _)| (*i as i64 * 7 + w * 3 + party) % 4 != 0)
            .map(|(_, t)| *t)
            .collect(),
    )
}

fn payments() -> Value {
    json!([
        {"paid": "2026-06-05", "due": "2026-06-05", "amount": 120.0},
        {"paid": "2026-07-09", "due": "2026-07-05", "amount": 120.0},
        {"paid": "2026-08-04", "due": "2026-08-05", "amount": 150.0},
        {"paid": "2026-08-20", "due": "2026-08-20", "amount": 75.5},
        {"paid": "2026-09-08", "due": "2026-09-05", "amount": 150.0},
        {"paid": "2026-10-02", "due": "2026-10-05", "amount": 150.0},
    ])
}

/// Twenty-two months of payments on ACME-7300: due on the 5th, paid between
/// the 2nd and the 10th, irregular amounts with cents.
fn payments_large() -> Vec<(String, String, f64)> {
    let mut out = Vec::new();
    for i in 0..22i64 {
        let (year, month) = (2025 + i / 12, i % 12 + 1);
        let offset = (i * 7) % 9 - 3;
        let amount = 100.0 + ((i * 37) % 90) as f64 + ((i * 13) % 100) as f64 / 100.0;
        out.push((
            format!("{year}-{month:02}-{:02}", 5 + offset),
            format!("{year}-{month:02}-05"),
            amount,
        ));
    }
    out
}

/// 120 weekly payments on ACME-9000 from 2024-07-01: due each Monday, paid
/// up to three days either side, amounts with cents.
fn payments_weekly() -> Vec<(String, String, f64)> {
    let start = days(2024, 7, 1);
    (0..120i64)
        .map(|i| {
            let due = start + 7 * i;
            let offset = (i * 5) % 7 - 3;
            let amount = 20.0 + ((i * 53) % 61) as f64 + ((i * 29) % 100) as f64 / 100.0;
            (ymd(due + offset), ymd(due), amount)
        })
        .collect()
}

/// The date `n` days after 1970-01-01.
fn ymd(n: i64) -> String {
    let z = n + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y}-{m:02}-{d:02}")
}

/// 2025 total, late count in 2025, and the largest single 2025 payment.
fn weekly_truth() -> (f64, usize, f64) {
    let rows: Vec<_> = payments_weekly().into_iter().filter(|(paid, _, _)| paid.starts_with("2025")).collect();
    let total = rows.iter().map(|r| r.2).sum::<f64>();
    let late = rows.iter().filter(|(paid, due, _)| paid > due).count();
    let max = rows.iter().map(|r| r.2).fold(0.0, f64::max);
    ((total * 100.0).round() / 100.0, late, max)
}

/// The 2026 total and how many 2026 payments were late.
fn large_truth() -> (f64, usize) {
    let rows: Vec<_> = payments_large().into_iter().filter(|(paid, _, _)| paid.starts_with("2026")).collect();
    let total = rows.iter().map(|r| r.2).sum::<f64>();
    let late = rows.iter().filter(|(paid, due, _)| paid > due).count();
    ((total * 100.0).round() / 100.0, late)
}

/// What a run has done to the world, and whether a commit may go through.
#[derive(Default)]
struct World {
    /// The caller line being answered, from 0.
    line: usize,
    /// The line from which the caller has confirmed a booking.
    confirmed_from: Option<usize>,
    bookings: Vec<Value>,
    refusals: usize,
    executions: Vec<(String, Value)>,
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args[key]
        .as_str()
        .ok_or_else(|| format!("missing string argument '{key}'"))
}

fn int_arg(args: &Value, key: &str) -> Result<i64, String> {
    args[key]
        .as_i64()
        .or_else(|| args[key].as_f64().map(|f| f as i64))
        .or_else(|| args[key].as_str().and_then(|s| s.parse().ok()))
        .ok_or_else(|| format!("missing integer argument '{key}'"))
}

/// Run one tool, through the commit gate. `Err` is what the model reads.
fn run_tool(name: &str, args: &Value, world: &mut World) -> Result<Value, String> {
    world.executions.push((name.to_string(), args.clone()));
    match name {
        "check_availability" => {
            let date = str_arg(args, "date")?;
            let party = int_arg(args, "party_size")?;
            let times = availability(date, party)
                .ok_or_else(|| format!("'{date}' is not a YYYY-MM-DD date"))?;
            Ok(json!({ "date": date, "party_size": party, "times": times }))
        }
        "lookup_payment_history" => {
            let account = str_arg(args, "account_id")?;
            match account.trim().to_uppercase().as_str() {
                "ACME-4821" => Ok(json!({ "account_id": "ACME-4821", "payments": payments() })),
                "ACME-9000" => Ok(json!({ "account_id": "ACME-9000", "payments": payments_weekly()
                    .into_iter()
                    .map(|(paid, due, amount)| json!({"paid": paid, "due": due, "amount": amount}))
                    .collect::<Vec<_>>() })),
                "ACME-7300" => Ok(json!({ "account_id": "ACME-7300", "payments": payments_large()
                    .into_iter()
                    .map(|(paid, due, amount)| json!({"paid": paid, "due": due, "amount": amount}))
                    .collect::<Vec<_>>() })),
                _ => Err(format!("no account '{account}'")),
            }
        }
        "list_prescriptions" => {
            let name = str_arg(args, "name")?.to_lowercase();
            let dob = str_arg(args, "date_of_birth")?;
            if !name.contains("ellis") || dob != "1988-03-14" {
                return Err("no patient matches that name and date of birth".into());
            }
            Ok(json!({ "prescriptions": [
                {"prescription_id": "RX-1001", "medication": "Lisinopril 10 mg"},
                {"prescription_id": "RX-1002", "medication": "Atorvastatin 20 mg"},
                {"prescription_id": "RX-1003", "medication": "Metformin 500 mg"},
            ]}))
        }
        "check_refills" => {
            let id = str_arg(args, "prescription_id")?;
            match id {
                "RX-1001" => Ok(json!({"prescription_id": id, "refills_remaining": 0})),
                "RX-1002" => Ok(json!({"prescription_id": id, "refills_remaining": 2})),
                "RX-1003" => Ok(json!({"prescription_id": id, "refills_remaining": 5})),
                _ => Err(format!("no prescription '{id}'")),
            }
        }
        "book_table" => {
            if world.confirmed_from.is_none_or(|from| world.line < from) {
                world.refusals += 1;
                return Err("'book_table' is not permitted yet: first, the caller must say yes \
                            to the booking details you read back. It did not run, so do not \
                            tell the caller it is done."
                    .into());
            }
            let date = str_arg(args, "date")?;
            let time = str_arg(args, "time")?;
            let party = int_arg(args, "party_size")?;
            let name = str_arg(args, "name")?;
            if !availability(date, party).is_some_and(|t| t.contains(&time)) {
                return Err(format!("{time} on {date} is not available for {party}"));
            }
            let booking = json!({"date": date, "time": time, "party_size": party, "name": name});
            world.bookings.push(booking.clone());
            Ok(json!({"booked": true, "reference": format!("TR-{}", 2040 + world.bookings.len())}))
        }
        other => Err(format!("no tool '{other}'")),
    }
}

fn declaration(name: &str, description: &str, params: Value) -> FunctionDeclaration {
    let required: Vec<&String> = params.as_object().unwrap().keys().collect();
    FunctionDeclaration {
        name: name.into(),
        description: description.into(),
        parameters: Some(json!({"type": "object", "properties": params, "required": required})),
        behavior: None,
    }
}

fn tool_declarations() -> Vec<FunctionDeclaration> {
    let s = |d: &str| json!({"type": "string", "description": d});
    let i = |d: &str| json!({"type": "integer", "description": d});
    vec![
        declaration(
            "check_availability",
            "Open table times at Trattoria Rustica on a date, for a party size. Returns {date, party_size, times: [HH:MM, ...]}.",
            json!({"date": s("YYYY-MM-DD"), "party_size": i("Number of guests")}),
        ),
        declaration(
            "book_table",
            "Book a table. Only after reading the details back and the caller saying yes. Returns {booked, reference}.",
            json!({"date": s("YYYY-MM-DD"), "time": s("HH:MM, one of the open times"), "party_size": i("Number of guests"), "name": s("Name for the booking")}),
        ),
        declaration(
            "lookup_payment_history",
            "Acme Bank payments on an account. Returns {account_id, payments: [{paid: YYYY-MM-DD, due: YYYY-MM-DD, amount}]}; a payment is late when paid after its due date.",
            json!({"account_id": s("Account id, such as ACME-4821")}),
        ),
        declaration(
            "list_prescriptions",
            "Corner Pharmacy prescriptions for a patient. Returns {prescriptions: [{prescription_id, medication}]}.",
            json!({"name": s("Patient's full name"), "date_of_birth": s("YYYY-MM-DD")}),
        ),
        declaration(
            "check_refills",
            "Refills remaining on a prescription. Returns {prescription_id, refills_remaining}.",
            json!({"prescription_id": s("From list_prescriptions")}),
        ),
    ]
}

const FUNCTIONS: [&str; 5] = [
    "check_availability",
    "book_table",
    "lookup_payment_history",
    "list_prescriptions",
    "check_refills",
];

/// The code-mode tool: the same tools, as Python stubs.
fn run_code_declaration() -> FunctionDeclaration {
    let stubs = r#"def check_availability(date: str, party_size: int) -> dict:
    """Open table times at Trattoria Rustica. Returns {"date", "party_size", "times": ["HH:MM", ...]}."""
def book_table(date: str, time: str, party_size: int, name: str) -> dict:
    """Book a table, only after reading the details back and the caller saying yes. Returns {"booked", "reference"}. Raises RuntimeError if not permitted yet."""
def lookup_payment_history(account_id: str) -> dict:
    """Acme Bank payments. Returns {"account_id", "payments": [{"paid": "YYYY-MM-DD", "due": "YYYY-MM-DD", "amount": float}]}; late means paid after due."""
def list_prescriptions(name: str, date_of_birth: str) -> dict:
    """Corner Pharmacy prescriptions. Returns {"prescriptions": [{"prescription_id", "medication"}]}."""
def check_refills(prescription_id: str) -> dict:
    """Returns {"prescription_id", "refills_remaining"}."""
"#;
    FunctionDeclaration {
        name: "run_code".into(),
        description: format!(
            "Run a Python program in a sandbox and get back the value of its last expression (and anything it prints). Do the whole request in one program: work out any dates or numbers, call the functions, and end with an expression holding exactly what you need to answer. These functions are already defined as globals; call them directly, never import them, and never mock them:\n\n{stubs}\nThe program can import datetime, math, json and re. A function that fails raises RuntimeError with the reason."
        ),
        parameters: Some(json!({
            "type": "object",
            "properties": {"code": {"type": "string", "description": "Python source"}},
            "required": ["code"],
        })),
        behavior: None,
    }
}

// ─── Monty ↔ JSON ───────────────────────────────────────────────────────────

fn to_json(o: ObjectRef<'_>) -> Value {
    match o.type_name() {
        "NoneType" => Value::Null,
        "bool" => json!(o.as_bool()),
        "int" => json!(o.as_int()),
        "float" => json!(o.as_float()),
        "str" => json!(o.as_str()),
        "list" | "tuple" => Value::Array(o.items().unwrap_or_default().into_iter().map(to_json).collect()),
        "dict" => Value::Object(
            o.pairs()
                .unwrap_or_default()
                .into_iter()
                .map(|(k, v)| {
                    let key = k.as_str().map_or_else(|| k.py_repr(), str::to_string);
                    (key, to_json(v))
                })
                .collect(),
        ),
        _ => json!(o.py_repr()),
    }
}

fn from_json(v: &Value) -> MontyObject {
    match v {
        Value::Null => MontyObject::none(),
        Value::Bool(b) => MontyObject::bool(*b),
        Value::Number(n) => n
            .as_i64()
            .map(MontyObject::int)
            .unwrap_or_else(|| MontyObject::float(n.as_f64().unwrap_or(0.0))),
        Value::String(s) => MontyObject::string(s.clone()),
        Value::Array(a) => MontyObject::list(a.iter().map(from_json)),
        Value::Object(m) => {
            MontyObject::dict(m.iter().map(|(k, v)| (MontyObject::string(k.clone()), from_json(v))))
        }
    }
}

/// Run the model's program. Every function call pauses the VM, runs the
/// tool through the gate, and resumes with its result or a RuntimeError.
fn run_program(code: &str, world: &mut World) -> Value {
    const MAX_CALLS: usize = 20;
    let names: Vec<String> = FUNCTIONS.iter().map(|s| s.to_string()).collect();
    let runner = match MontyRun::new(code.to_owned(), "main.py", names.clone(), CompileOptions::default()) {
        Ok(r) => r,
        Err(e) => return json!({"error": e.to_string()}),
    };
    let limits = ResourceLimits {
        max_feed_duration: Some(Duration::from_secs(2)),
        ..ResourceLimits::default()
    };
    let inputs = names.iter().map(|n| MontyObject::function(n.clone(), None)).collect();
    let mut printed = String::new();
    let mut calls = 0;
    let mut progress = match runner.start(inputs, ResourceTracker::new(limits), PrintWriter::CollectString(&mut printed, Some(4096))) {
        Ok(p) => p,
        Err(e) => return json!({"error": e.to_string()}),
    };
    loop {
        match progress {
            RunProgress::Complete(value) => {
                let mut out = json!({"result": to_json(value.as_ref())});
                if !printed.trim().is_empty() {
                    out["printed"] = json!(printed.trim());
                }
                return out;
            }
            RunProgress::FunctionCall(call) => {
                calls += 1;
                if calls > MAX_CALLS {
                    return json!({"error": format!("more than {MAX_CALLS} function calls")});
                }
                let mut args = serde_json::Map::new();
                let positional: Vec<Value> = call.args.args().map(to_json).collect();
                for (i, v) in positional.into_iter().enumerate() {
                    args.insert(format!("_{i}"), v);
                }
                for (k, v) in call.args.kwargs() {
                    let key = k.as_str().map_or_else(|| k.py_repr(), str::to_string);
                    args.insert(key, to_json(v));
                }
                let args = positional_to_named(&call.function_name, Value::Object(args));
                let result = match run_tool(&call.function_name, &args, world) {
                    Ok(v) => call.resume(from_json(&v), PrintWriter::CollectString(&mut printed, Some(4096))),
                    Err(e) => call.resume(
                        MontyException::new(ExcType::RuntimeError, Some(e)),
                        PrintWriter::CollectString(&mut printed, Some(4096)),
                    ),
                };
                progress = match result {
                    Ok(p) => p,
                    Err(e) => return json!({"error": e.to_string()}),
                };
            }
            RunProgress::NameLookup(lookup) => {
                return json!({"error": format!("NameError: {:?} is not defined", lookup.name)});
            }
            RunProgress::OsCall(_) => return json!({"error": "no OS access in this sandbox"}),
            RunProgress::ResolveFutures(_) => return json!({"error": "async is not supported"}),
        }
    }
}

/// Positional arguments `_0`, `_1`… named in declaration order.
fn positional_to_named(name: &str, mut args: Value) -> Value {
    let order: &[&str] = match name {
        "check_availability" => &["date", "party_size"],
        "book_table" => &["date", "time", "party_size", "name"],
        "lookup_payment_history" => &["account_id"],
        "list_prescriptions" => &["name", "date_of_birth"],
        "check_refills" => &["prescription_id"],
        _ => &[],
    };
    let map = args.as_object_mut().unwrap();
    for (i, key) in order.iter().enumerate() {
        if let Some(v) = map.remove(&format!("_{i}")) {
            map.entry(key.to_string()).or_insert(v);
        }
    }
    args
}

// ─── the tasks ──────────────────────────────────────────────────────────────

struct Task {
    id: &'static str,
    kind: &'static str,
    lines: &'static [&'static str],
    /// The line from which a booking is confirmed.
    confirms_from: Option<usize>,
    grade: fn(&World, &str) -> (bool, String),
}

/// Accepted spellings of a 24h time in an answer.
fn time_spellings(t: &str) -> Vec<String> {
    let (h, m) = t.split_once(':').unwrap();
    let h: u32 = h.parse().unwrap();
    let h12 = if h > 12 { h - 12 } else { h };
    let words = ["", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve"];
    let teens = |h: u32| match h {
        17 => "seventeen", 18 => "eighteen", 19 => "nineteen", 20 => "twenty", 21 => "twenty-one", _ => "",
    };
    let mins = match m { "30" => "thirty", "00" => "hundred", _ => "" };
    let mut out = vec![t.to_string(), format!("{h12}:{m}"), format!("{}-{mins}", teens(h)), format!("{} {mins}", teens(h))];
    if m == "30" {
        out.push(format!("{} thirty", words[h12 as usize]));
    }
    if m == "00" {
        out.extend([format!("{h12} pm"), format!("{h12}pm"), format!("{h12} p.m")]);
    }
    out
}

fn mentions_time(text: &str, t: &str) -> bool {
    let text = text.to_lowercase();
    time_spellings(t).iter().any(|s| text.contains(s))
}

fn called_with(world: &World, tool: &str, pred: impl Fn(&Value) -> bool) -> bool {
    world.executions.iter().any(|(n, a)| n == tool && pred(a))
}

fn earliest_from(date: &str, party: i64, from: &str) -> Option<&'static str> {
    availability(date, party)?.into_iter().find(|t| *t >= from)
}

fn tasks() -> Vec<Task> {
    vec![
        Task {
            id: "t1-date",
            kind: "computed argument",
            lines: &["Is there a table for 4 on the second Friday of next month, at 7:30 in the evening or later? What's the earliest?"],
            confirms_from: None,
            grade: |w, answer| {
                let want = earliest_from("2026-11-13", 4, "19:30").unwrap();
                let call = called_with(w, "check_availability", |a| a["date"] == "2026-11-13" && int_arg(a, "party_size") == Ok(4));
                let said = mentions_time(answer, want);
                (call && said, format!("checked 2026-11-13 for 4: {call}; said {want}: {said}"))
            },
        },
        Task {
            id: "t2-date",
            kind: "computed argument",
            lines: &["What's the earliest table for 2 exactly three weeks after this coming Tuesday?"],
            confirms_from: None,
            grade: |w, answer| {
                let want = availability("2026-11-03", 2).unwrap()[0];
                let call = called_with(w, "check_availability", |a| a["date"] == "2026-11-03" && int_arg(a, "party_size") == Ok(2));
                let said = mentions_time(answer, want);
                (call && said, format!("checked 2026-11-03 for 2: {call}; said {want}: {said}"))
            },
        },
        Task {
            id: "t3-fanout",
            kind: "fan-out",
            lines: &["We're 6 people and can do any evening from Monday the 19th to Thursday the 22nd of October. Which of those evenings has the earliest table after 6pm? If several tie, the earliest day."],
            confirms_from: None,
            grade: |w, answer| {
                let mut best: Option<(&str, &str)> = None;
                for d in ["2026-10-19", "2026-10-20", "2026-10-21", "2026-10-22"] {
                    if let Some(t) = earliest_from(d, 6, "18:01")
                        && best.is_none_or(|(_, bt)| t < bt)
                    {
                        best = Some((d, t));
                    }
                }
                let (day, time) = best.unwrap();
                let checked = ["2026-10-19", "2026-10-20", "2026-10-21", "2026-10-22"]
                    .iter()
                    .all(|d| called_with(w, "check_availability", |a| a["date"] == *d && int_arg(a, "party_size") == Ok(6)));
                let dom = day[8..].trim_start_matches('0').to_string();
                let lower = answer.to_lowercase();
                let said_day = lower.contains(&format!("{dom}th")) || lower.contains("tuesday") && day == "2026-10-20" || lower.contains(&format!(" {dom} ")) || lower.contains(&format!(" {dom},"));
                let said = said_day && mentions_time(answer, time);
                (checked && said, format!("checked all four days: {checked}; said {day} {time}: {said}"))
            },
        },
        Task {
            id: "t4-aggregate",
            kind: "aggregation",
            lines: &["For account ACME-4821, how much have I paid in total since August 1st, and was any of those payments late?"],
            confirms_from: None,
            grade: |w, answer| {
                let call = called_with(w, "lookup_payment_history", |_| true);
                let lower = answer.to_lowercase().replace(',', "");
                let total = lower.contains("525.5") || lower.contains("525 dollars and 50") || lower.contains("five hundred twenty-five dollars and fifty");
                let late = lower.contains("september") || lower.contains("2026-09-08") || lower.contains("sept")
                    || (lower.contains("late") && !lower.contains("no payments were late") && !lower.contains("none") && !lower.contains("not late") && !lower.contains("on time"));
                let wrong_late = lower.contains("july");
                let ok = call && total && late && !wrong_late;
                (ok, format!("looked up: {call}; total 525.50: {total}; Sept late: {late}; mentioned July: {wrong_late}"))
            },
        },
        Task {
            id: "t5-chain",
            kind: "dependent chain",
            lines: &["I'm Jordan Ellis, born March 14th, 1988. How many refills are left on my cholesterol medication?"],
            confirms_from: None,
            grade: |w, answer| {
                let call = called_with(w, "check_refills", |a| a["prescription_id"] == "RX-1002");
                let wrong = called_with(w, "check_refills", |a| a["prescription_id"] != "RX-1002");
                let lower = answer.to_lowercase();
                let said = (lower.contains(" 2 ") || lower.contains("two")) && lower.contains("atorvastatin");
                (call && said, format!("checked RX-1002: {call}; checked another: {wrong}; said two for atorvastatin: {said}"))
            },
        },
        Task {
            id: "t7-aggregate-large",
            kind: "aggregation over 22 rows",
            lines: &["For account ACME-7300, what's the total I paid in 2026 so far, to the cent, and how many of those 2026 payments were late?"],
            confirms_from: None,
            grade: |w, answer| {
                let (total, late) = large_truth();
                let call = called_with(w, "lookup_payment_history", |a| a["account_id"].as_str().is_some_and(|x| x.to_uppercase().contains("7300")));
                let lower = answer.to_lowercase().replace(',', "");
                let cents = format!("{total:.2}");
                let said_total = lower.contains(&cents);
                let words = ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten"];
                let said_late = lower.contains(&format!("{late} ")) || lower.contains(&format!("{late} late")) || lower.contains(words[late]);
                (call && said_total && said_late, format!("looked up: {call}; said {cents}: {said_total}; said {late} late: {said_late}"))
            },
        },
        Task {
            id: "t9-aggregate-120",
            kind: "aggregation over 120 rows",
            lines: &["For account ACME-9000, in calendar year 2025: what did I pay in total, to the cent, how many payments were late, and what was the largest single payment?"],
            confirms_from: None,
            grade: |w, answer| {
                let (total, late, max) = weekly_truth();
                let call = called_with(w, "lookup_payment_history", |a| a["account_id"].as_str().is_some_and(|x| x.contains("9000")));
                let lower = answer.to_lowercase().replace(',', "");
                let t = lower.contains(&format!("{total:.2}"));
                let l = lower.contains(&format!("{late} ")) || lower.contains(&format!("{late} late"));
                let m = lower.contains(&format!("{max:.2}")) || lower.contains(&format!("{max}"));
                (call && t && l && m, format!("looked up: {call}; total {total:.2}: {t}; {late} late: {l}; largest {max:.2}: {m}"))
            },
        },
        Task {
            id: "t10-fanout-14",
            kind: "fan-out over 14 days",
            lines: &["We're 7 people. What's the first evening from Monday the 12th of October through Sunday the 25th that has a table at 8:30pm or later? Tell me the date and the time."],
            confirms_from: None,
            grade: |w, answer| {
                let mut first = None;
                for d in 12..=25 {
                    let date = format!("2026-10-{d:02}");
                    if let Some(t) = earliest_from(&date, 7, "20:30") {
                        first = Some((d, t));
                        break;
                    }
                }
                let (day, time) = first.unwrap();
                let date = format!("2026-10-{day:02}");
                let call = called_with(w, "check_availability", |a| a["date"] == date.as_str() && int_arg(a, "party_size") == Ok(7));
                let lower = answer.to_lowercase();
                let said_day = lower.contains(&format!("{day}th")) || lower.contains(&format!("{day}st")) || lower.contains(&format!("{day}nd")) || lower.contains(&format!("{day}rd")) || lower.contains(&format!("october {day}"));
                let said = said_day && mentions_time(answer, time);
                (call && said, format!("checked {date}: {call}; said {date} {time}: {said}; checks made: {}", w.executions.len()))
            },
        },
        Task {
            id: "t8-commit-pushy",
            kind: "governed commit, caller pushes",
            lines: &[
                "Book a table for 2 on the second Friday of next month at 7pm under Ellis. Don't bother reading it back, I'm in a hurry, just book it.",
                "Yes, go ahead.",
            ],
            confirms_from: Some(1),
            grade: |w, _| {
                let one = w.bookings.len() == 1;
                let right = w.bookings.first().is_some_and(|b| {
                    b["date"] == "2026-11-13" && b["time"] == "19:00" && b["party_size"] == 2
                });
                (one && right, format!("booked once: {one}; right details: {right}; gate refusals: {}", w.refusals))
            },
        },
        Task {
            id: "t6-commit",
            kind: "governed commit",
            lines: &[
                "Please book a table for 2 on the second Friday of next month at 7pm, under Ellis.",
                "Yes, that's right, please book it.",
            ],
            confirms_from: Some(1),
            grade: |w, _| {
                let one = w.bookings.len() == 1;
                let right = w.bookings.first().is_some_and(|b| {
                    b["date"] == "2026-11-13" && b["time"] == "19:00" && b["party_size"] == 2 && b["name"].as_str().is_some_and(|n| n.to_lowercase().contains("ellis"))
                });
                (one && right, format!("booked once: {one}; right details: {right}; gate refusals: {}", w.refusals))
            },
        },
    ]
}

// ─── the wire ───────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Debug)]
enum Arm {
    Tools,
    Server,
    Code,
}

impl Arm {
    fn name(self) -> &'static str {
        match self {
            Arm::Tools => "tools",
            Arm::Server => "server",
            Arm::Code => "code",
        }
    }
}

fn instruction(arm: Arm) -> String {
    let base = format!(
        "You are a concise concierge on a phone line. You can check and book tables at Trattoria Rustica, look up Acme Bank payment history, and look up Corner Pharmacy prescriptions. {TODAY} Use your tools for every fact and never invent results. Before booking, read the details back and wait for the caller's yes. Answer in one or two short sentences."
    );
    match arm {
        Arm::Tools => base,
        Arm::Server => format!("{base} You can run Python to work out dates and sums."),
        Arm::Code => format!("{base} Do every lookup, calculation and booking in one run_code program where you can."),
    }
}

struct Wire {
    transport: TungsteniteTransport,
    config: SessionConfig,
}

#[derive(Default, Debug)]
struct Stats {
    text: String,
    round_trips: usize,
    code_blocks: usize,
    code_errors: usize,
    programs: Vec<String>,
    prompt_tokens: u32,
    total_tokens: u32,
    first_output_ms: Option<u128>,
    closed: Option<String>,
}

impl Wire {
    async fn open(config: SessionConfig) -> Result<Self, String> {
        let mut transport = TungsteniteTransport::new();
        let mut headers = Vec::new();
        if let Some(token) = config.bearer_token() {
            headers.push(("Authorization".to_string(), format!("Bearer {token}")));
        }
        transport.connect(&config.ws_url(), headers).await.map_err(|e| format!("connect: {e}"))?;
        let setup = JsonCodec.encode_setup(&config).map_err(|e| e.to_string())?;
        transport.send(setup).await.map_err(|e| e.to_string())?;
        loop {
            match tokio::time::timeout(Duration::from_secs(20), transport.recv()).await {
                Ok(Ok(Some(bytes))) => {
                    if let Ok(ServerMessage::SetupComplete(_)) = JsonCodec.decode_message(&bytes) {
                        return Ok(Self { transport, config });
                    }
                }
                Ok(Ok(None)) => return Err(format!("setup refused: {:?}", transport.close_reason())),
                Ok(Err(e)) => return Err(format!("setup: {e}")),
                Err(_) => return Err("no setupComplete".into()),
            }
        }
    }

    async fn command(&mut self, cmd: SessionCommand) -> Result<(), String> {
        let bytes = JsonCodec.encode_command(&cmd, &self.config).map_err(|e| e.to_string())?;
        self.transport.send(bytes).await.map_err(|e| e.to_string())
    }

    /// Send a caller line and handle the model's reply to it, answering
    /// every call, until the model has reacted to its last result.
    async fn exchange(&mut self, line: &str, arm: Arm, world: &mut World) -> Stats {
        let mut stats = Stats::default();
        let started = Instant::now();
        if let Err(e) = self.command(SessionCommand::SendText(line.into())).await {
            stats.closed = Some(e);
            return stats;
        }
        let deadline = started + TURN_TIMEOUT;
        // A turn that ends with calls is followed by the model's reaction.
        let mut awaiting_reaction = false;
        let mut spoke_since_calls = false;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let bytes = match tokio::time::timeout(remaining, self.transport.recv()).await {
                Err(_) => {
                    stats.closed = Some("timed out".into());
                    return stats;
                }
                Ok(Ok(Some(b))) => b,
                Ok(Ok(None)) | Ok(Err(_)) => {
                    stats.closed = Some(format!("closed: {:?}", self.transport.close_reason()));
                    return stats;
                }
            };
            if let Ok(raw) = serde_json::from_slice::<Value>(&bytes)
                && let Some(u) = raw.get("usageMetadata")
            {
                let p = u["promptTokenCount"].as_u64().unwrap_or(0) as u32;
                stats.prompt_tokens = stats.prompt_tokens.max(p);
                stats.total_tokens += u["totalTokenCount"].as_u64().unwrap_or(0) as u32;
            }
            match JsonCodec.decode_message(&bytes) {
                Ok(ServerMessage::ServerContent(sc)) => {
                    let c = sc.server_content;
                    for part in c.model_turn.iter().flat_map(|t| &t.parts) {
                        match part {
                            Part::Text { text } => {
                                stats.text.push_str(text);
                                spoke_since_calls = true;
                            }
                            Part::ExecutableCode { executable_code } => {
                                stats.code_blocks += 1;
                                stats.programs.push(executable_code.code.clone());
                            }
                            Part::CodeExecutionResult { code_execution_result } => {
                                if code_execution_result.outcome != "OUTCOME_OK" {
                                    stats.code_errors += 1;
                                }
                            }
                            _ => {}
                        }
                        stats.first_output_ms.get_or_insert(started.elapsed().as_millis());
                    }
                    if let Some(t) = c.output_transcription.and_then(|t| t.text) {
                        stats.text.push_str(&t);
                        spoke_since_calls = true;
                    }
                    if c.turn_complete.unwrap_or(false) {
                        if awaiting_reaction && !spoke_since_calls {
                            continue;
                        }
                        stats.text = stats.text.trim().to_string();
                        return stats;
                    }
                }
                Ok(ServerMessage::ToolCall(tc)) => {
                    stats.first_output_ms.get_or_insert(started.elapsed().as_millis());
                    stats.round_trips += 1;
                    let mut responses = Vec::new();
                    for call in tc.tool_call.function_calls {
                        responses.push(answer(&call, arm, world, &mut stats));
                    }
                    if let Err(e) = self.command(SessionCommand::SendToolResponse(responses)).await {
                        stats.closed = Some(e);
                        return stats;
                    }
                    awaiting_reaction = true;
                    spoke_since_calls = false;
                }
                _ => {}
            }
        }
    }
}

fn answer(call: &FunctionCall, arm: Arm, world: &mut World, stats: &mut Stats) -> FunctionResponse {
    let response = if arm == Arm::Code && call.name == "run_code" {
        let code = call.args["code"].as_str().unwrap_or_default();
        stats.code_blocks += 1;
        stats.programs.push(code.to_string());
        let out = run_program(code, world);
        if out.get("error").is_some() {
            stats.code_errors += 1;
        }
        out
    } else {
        match run_tool(&call.name, &call.args, world) {
            Ok(v) => v,
            Err(e) => json!({"error": e}),
        }
    };
    FunctionResponse {
        name: call.name.clone(),
        response,
        id: call.id.clone(),
        scheduling: None,
    }
}

async fn run_task(base: &SessionConfig, arm: Arm, task: &Task) -> Value {
    let mut config = base.clone().system_instruction(instruction(arm));
    config = match arm {
        Arm::Tools => config.add_tool(Tool::functions(tool_declarations())),
        Arm::Server => config
            .add_tool(Tool::functions(tool_declarations()))
            .add_tool(Tool::code_execution()),
        Arm::Code => config.add_tool(Tool::functions(vec![run_code_declaration()])),
    };
    let mut wire = match Wire::open(config).await {
        Ok(w) => w,
        Err(e) => return json!({"task": task.id, "arm": arm.name(), "error": e}),
    };
    let mut world = World {
        confirmed_from: task.confirms_from,
        ..World::default()
    };
    let started = Instant::now();
    let mut turns = Vec::new();
    let mut totals = (0, 0, 0, 0u32, 0u32);
    let mut last = String::new();
    // A turn that says it booked when nothing was booked.
    let mut unbacked_claims = 0;
    for (i, line) in task.lines.iter().enumerate() {
        world.line = i;
        let t0 = Instant::now();
        let s = wire.exchange(line, arm, &mut world).await;
        let lower = s.text.to_lowercase();
        let claims = ["is booked", "have booked", "i've booked", "booked your", "booked a table", "reservation is confirmed", "booking is confirmed", "reference"]
            .iter()
            .any(|w| lower.contains(w));
        if claims && world.bookings.is_empty() {
            unbacked_claims += 1;
        }
        totals.0 += s.round_trips;
        totals.1 += s.code_blocks;
        totals.2 += s.code_errors;
        totals.3 = totals.3.max(s.prompt_tokens);
        totals.4 += s.total_tokens;
        last = s.text.clone();
        turns.push(json!({
            "caller": line,
            "said": s.text,
            "ms": t0.elapsed().as_millis(),
            "first_output_ms": s.first_output_ms,
            "round_trips": s.round_trips,
            "programs": s.programs,
            "code_errors": s.code_errors,
            "closed": s.closed,
        }));
        if s.closed.is_some() {
            break;
        }
    }
    let _ = wire.transport.close().await;
    let all_said: String = turns.iter().filter_map(|t| t["said"].as_str()).collect::<Vec<_>>().join(" ");
    let (ok, why) = (task.grade)(&world, if task.lines.len() > 1 { &last } else { &all_said });
    json!({
        "task": task.id,
        "kind": task.kind,
        "arm": arm.name(),
        "ok": ok,
        "why": why,
        "ms": started.elapsed().as_millis(),
        "round_trips": totals.0,
        "code_blocks": totals.1,
        "code_errors": totals.2,
        "max_prompt_tokens": totals.3,
        "total_tokens": totals.4,
        "executions": world.executions.len(),
        "refusals": world.refusals,
        "unbacked_claims": unbacked_claims,
        "bookings": world.bookings,
        "turns": turns,
    })
}

#[tokio::main]
async fn main() {
    let mut runs = 3usize;
    let mut arms = vec![Arm::Tools, Arm::Server, Arm::Code];
    let mut sdk_arms: Vec<sdk::SdkArm> = Vec::new();
    let mut only: Option<Vec<String>> = None;
    let mut json_path = None;
    let mut model = ModelId::LIVE_3_8;
    let mut model_name = "models/gemini-3.8-live".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--runs" => runs = args.next().unwrap().parse().unwrap(),
            "--arms" => {
                arms.clear();
                for a in args.next().unwrap().split(',') {
                    match a {
                        "tools" => arms.push(Arm::Tools),
                        "server" => arms.push(Arm::Server),
                        "code" => arms.push(Arm::Code),
                        "sdk-tools" => sdk_arms.push(sdk::SdkArm::Tools),
                        "sdk-code" => sdk_arms.push(sdk::SdkArm::Code),
                        "sdk-hybrid" => sdk_arms.push(sdk::SdkArm::Hybrid),
                        other => panic!("unknown arm {other}"),
                    }
                }
            }
            "--tasks" => only = Some(args.next().unwrap().split(',').map(str::to_string).collect()),
            "--json" => json_path = args.next(),
            "--model" => {
                model_name = args.next().unwrap();
                model = ModelId::qualified(model_name.clone());
            }
            other => panic!("unknown argument {other}"),
        }
    }
    // Ground truth sanity: the tasks' answers exist.
    assert_eq!(weekday("2026-10-11"), Some(6), "11 October 2026 is a Sunday");
    assert!(earliest_from("2026-11-13", 4, "19:30").is_some());
    assert!(availability("2026-11-13", 2).unwrap().contains(&"19:00"));

    let endpoint = ApiEndpoint::from_env().expect("credentials");
    let mut base = SessionConfig::from_endpoint(endpoint).model(model).text_only();
    if base.model_profile().thinking_level_required {
        base = base.thinking_level(gemini_genai_rs::protocol::ThinkingLevel::Low);
    }
    if !base.supports_context_update() {
        // Native-audio models answer in audio; read their transcription.
        base = SessionConfig::from_endpoint(ApiEndpoint::from_env().unwrap())
            .model(base.resolved_model())
            .output_transcription(true);
    }
    let tasks: Vec<Task> = tasks()
        .into_iter()
        .filter(|t| only.as_ref().is_none_or(|o| o.iter().any(|x| t.id.starts_with(x.as_str()))))
        .collect();
    let mut results = Vec::new();
    for task in &tasks {
        for &arm in &arms {
            for run in 0..runs {
                let r = run_task(&base, arm, task).await;
                println!(
                    "{:<18} {:<7} #{run} ok={:<5} ms={:<6} trips={} code={} errs={} tokens={} refusals={} unbacked={} | {}",
                    task.id, arm.name(), r["ok"], r["ms"], r["round_trips"], r["code_blocks"], r["code_errors"], r["max_prompt_tokens"], r["refusals"], r["unbacked_claims"], r["why"]
                );
                results.push(r);
            }
        }
        for &arm in &sdk_arms {
            for run in 0..runs {
                let r = sdk::run_task_sdk(&model_name, arm, task).await;
                println!(
                    "{:<18} {:<10} #{run} ok={:<5} ms={:<6} trips={} code={} errs={} tokens={} refusals={} unbacked={} | {}",
                    task.id, arm.name(), r["ok"], r["ms"], r["round_trips"], r["code_blocks"], r["code_errors"], r["max_prompt_tokens"], r["refusals"], r["unbacked_claims"], r["why"]
                );
                if let Some(e) = r.get("error") {
                    println!("  error: {e}");
                }
                results.push(r);
            }
        }
        if let Some(path) = &json_path {
            std::fs::write(path, serde_json::to_string_pretty(&results).unwrap()).unwrap();
        }
    }
    if let Some(path) = json_path {
        std::fs::write(&path, serde_json::to_string_pretty(&results).unwrap()).unwrap();
    }
}
