use std::collections::{BTreeMap, BTreeSet};

use gemini_adk_rs::live::RuntimeContract;
use serde_json::Value;

pub fn run(path: &str, contract_path: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let data = std::fs::read_to_string(path)?;
    let root: Value = serde_json::from_str(&data)?;
    let events = extract_events(&root)?;
    let contract = match contract_path {
        Some(path) => Some(serde_json::from_str::<RuntimeContract>(
            &std::fs::read_to_string(path)?,
        )?),
        None => None,
    };

    let report = analyze(&events, contract.as_ref());
    print_report(path, contract_path, &report);

    if report.errors.is_empty() {
        Ok(())
    } else {
        Err(format!("replay validation found {} issue(s)", report.errors.len()).into())
    }
}

fn extract_events(root: &Value) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    if let Some(events) = root.as_array() {
        return Ok(events.clone());
    }
    if let Some(events) = root.get("events").and_then(Value::as_array) {
        return Ok(events.clone());
    }
    Err("expected a JSON array or object with an `events` array".into())
}

struct ReplayReport {
    total_events: usize,
    type_counts: BTreeMap<String, usize>,
    phases: Vec<String>,
    tools: Vec<String>,
    promotions: Vec<String>,
    errors: Vec<String>,
    warnings: Vec<String>,
}

fn analyze(events: &[Value], contract: Option<&RuntimeContract>) -> ReplayReport {
    let mut report = ReplayReport {
        total_events: events.len(),
        type_counts: BTreeMap::new(),
        phases: Vec::new(),
        tools: Vec::new(),
        promotions: Vec::new(),
        errors: Vec::new(),
        warnings: Vec::new(),
    };

    let contract_phases = contract.map(|c| {
        c.phases
            .iter()
            .map(|phase| phase.name.as_str())
            .collect::<BTreeSet<_>>()
    });
    let contract_tools = contract.map(|c| {
        c.tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<BTreeSet<_>>()
    });
    let contract_promotions = contract.map(|c| {
        c.extractors
            .iter()
            .flat_map(|extractor| extractor.promotions.iter())
            .map(|promotion| promotion.state_key.as_str())
            .collect::<BTreeSet<_>>()
    });

    for (idx, event) in events.iter().enumerate() {
        let Some(event_type) = event.get("type").and_then(Value::as_str) else {
            report
                .warnings
                .push(format!("event #{idx} has no string `type`"));
            continue;
        };
        *report
            .type_counts
            .entry(event_type.to_string())
            .or_insert(0) += 1;

        match event_type {
            "phaseChange" => {
                let from = event.get("from").and_then(Value::as_str).unwrap_or("?");
                let to = event.get("to").and_then(Value::as_str).unwrap_or("?");
                report.phases.push(format!("{from} -> {to}"));
                if let Some(phases) = &contract_phases {
                    if to != "?" && !phases.contains(to) {
                        report
                            .errors
                            .push(format!("event #{idx} transitions to unknown phase `{to}`"));
                    }
                }
            }
            "toolCallEvent" => {
                let name = event.get("name").and_then(Value::as_str).unwrap_or("?");
                report.tools.push(name.to_string());
                if let Some(tools) = &contract_tools {
                    if name != "?" && !tools.contains(name) {
                        report
                            .errors
                            .push(format!("event #{idx} calls unknown tool `{name}`"));
                    }
                }
            }
            "statePromotionEvent" => {
                let key = event
                    .get("state_key")
                    .or_else(|| event.get("stateKey"))
                    .and_then(Value::as_str)
                    .unwrap_or("?");
                report.promotions.push(key.to_string());
                if let Some(promotions) = &contract_promotions {
                    if !promotions.is_empty() && key != "?" && !promotions.contains(key) {
                        report.errors.push(format!(
                            "event #{idx} promotes undeclared state key `{key}`"
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    report
}

fn print_report(path: &str, contract_path: Option<&str>, report: &ReplayReport) {
    println!("\n  ADK Replay — {}\n", path);
    if let Some(contract_path) = contract_path {
        println!("  Contract: {}", contract_path);
    } else {
        println!("  Contract: not provided");
    }
    println!("  Events:   {}", report.total_events);

    println!("\n  Event types:");
    for (event_type, count) in &report.type_counts {
        println!("    {:<24} {}", event_type, count);
    }

    if !report.phases.is_empty() {
        println!("\n  Phase transitions:");
        for transition in &report.phases {
            println!("    {}", transition);
        }
    }

    if !report.tools.is_empty() {
        println!("\n  Tool calls:");
        for tool in &report.tools {
            println!("    {}", tool);
        }
    }

    if !report.promotions.is_empty() {
        println!("\n  State promotions:");
        for key in &report.promotions {
            println!("    {}", key);
        }
    }

    if !report.warnings.is_empty() {
        println!("\n  Warnings:");
        for warning in &report.warnings {
            println!("    - {}", warning);
        }
    }

    if !report.errors.is_empty() {
        println!("\n  Errors:");
        for error in &report.errors {
            println!("    - {}", error);
        }
    } else {
        println!("\n  Validation: passed");
    }
    println!();
}
