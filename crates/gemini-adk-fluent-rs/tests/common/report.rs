//! Rendering an [`Evaluation`] to Markdown and to self-contained HTML.
//!
//! Both formats carry the same content and neither summarises away the
//! evidence: every verdict is printed next to the observation that produced it,
//! and every spoken turn is included verbatim as the recogniser heard it. A
//! report that says "PASS ×12" without showing what was said is a report you
//! have to trust, and the point of running this against a live model is to
//! produce something you do not have to.
//!
//! The HTML is deliberately dependency-free — one inline stylesheet, no fonts,
//! no scripts — so it opens from a file:// URL on a machine with no network and
//! survives being attached to a ticket.

#![allow(dead_code)]

use std::fmt::Write as _;

use super::evaluate::{Evaluation, Outcome, Surface};

/// One turn of the conversation, as observed.
#[derive(Debug, Clone)]
pub struct TurnRecord {
    /// 1-based turn number.
    pub index: usize,
    /// What the caller said (the text that was synthesised).
    pub caller: String,
    /// What the recogniser heard the assistant say.
    pub assistant: String,
    /// Tools that ran during this turn.
    pub tools: Vec<String>,
    /// Wall-clock milliseconds from end-of-caller-speech to turn complete.
    pub turn_ms: Option<u128>,
}

/// Everything needed to render a report.
pub struct ReportInput<'a> {
    /// Report title.
    pub title: &'a str,
    /// The model under evaluation.
    pub model: &'a str,
    /// ISO-8601 timestamp of the run, supplied by the caller.
    pub run_at: &'a str,
    /// Scored results.
    pub evaluation: &'a Evaluation,
    /// The happy-path conversation.
    pub transcript: &'a [TurnRecord],
}

/// The one-line verdict.
///
/// A failed model-speech probe is counted in `fail` but does not block, so the
/// headline has to distinguish the two — otherwise a behaviour finding reads
/// identically to a bypassed gate, and the reader has to scroll to learn which
/// one they are looking at.
fn verdict(e: &Evaluation, fail: usize) -> String {
    let blocking = e.blocking_failures().len();
    match (fail, blocking) {
        (0, _) => "No functional or adversarial failures.".to_string(),
        (_, 0) => format!(
            "No blocking failures. {fail} model-speech finding{} — behaviour, not a bypassed gate.",
            if fail == 1 { "" } else { "s" }
        ),
        _ => "**Failures present — see below.**".to_string(),
    }
}

/// Render the report as Markdown.
pub fn markdown(input: &ReportInput<'_>) -> String {
    let e = input.evaluation;
    let (pass, fail, not_reached) = e.tally();
    let mut s = String::new();

    let _ = writeln!(s, "# {}\n", input.title);
    let _ = writeln!(
        s,
        "**Model** `{}` · **Run** {} · **Transport** live WebSocket, caller synthesised with Gemini TTS\n",
        input.model, input.run_at
    );

    let _ = writeln!(
        s,
        "{} {pass} passed, {fail} failed, {not_reached} not reached.\n",
        verdict(e, fail)
    );

    if !e.blocking_failures().is_empty() {
        let _ = writeln!(s, "## Blocking failures\n");
        for f in e.blocking_failures() {
            let _ = writeln!(s, "- {f}");
        }
        let _ = writeln!(s);
    }

    // ── Functional ──────────────────────────────────────────────────────────
    let _ = writeln!(s, "## Functional requirements\n");
    let _ = writeln!(
        s,
        "Binary and mechanically checked against the flow monitor and the tool \
         journal. No thresholds, no judgement — a failure here is a defect.\n"
    );
    let _ = writeln!(s, "| | Requirement | Result | Evidence |");
    let _ = writeln!(s, "|---|---|---|---|");
    for f in &e.functional {
        let _ = writeln!(
            s,
            "| `{}` | {} | **{}** | {} |",
            f.id,
            f.requirement,
            f.outcome.glyph(),
            escape_pipes(&f.evidence)
        );
    }
    let _ = writeln!(s);
    for f in &e.functional {
        let _ = writeln!(s, "- **{}** — {}", f.id, f.rationale);
    }
    let _ = writeln!(s);

    // ── Non-functional ──────────────────────────────────────────────────────
    let _ = writeln!(s, "## Non-functional requirements\n");
    let _ = writeln!(
        s,
        "Measurements against a stated budget. These move with the network and \
         the runner, so a miss is a signal to investigate rather than a defect — \
         the measured value is printed next to the budget so the size of the \
         miss is visible.\n"
    );
    let _ = writeln!(s, "| | Metric | Measured | Budget | Result |");
    let _ = writeln!(s, "|---|---|---|---|---|");
    for n in &e.non_functional {
        let _ = writeln!(
            s,
            "| `{}` | {} | **{}** | {} | {} |",
            n.id,
            n.metric,
            n.measured,
            n.budget,
            n.outcome.glyph()
        );
    }
    let _ = writeln!(s);
    for n in &e.non_functional {
        let _ = writeln!(s, "- **{}** — {}", n.id, n.rationale);
    }
    let _ = writeln!(s);

    // ── Adversarial ─────────────────────────────────────────────────────────
    let _ = writeln!(s, "## Adversarial probes\n");
    let _ = writeln!(
        s,
        "Each probe names the surface it attacks. A **flow gate** failure means \
         the governance model was bypassed — the DAG let a tool run that it \
         should have refused. A **model speech** failure means the assistant \
         *said* something it should not have; no tool gate can prevent that, so \
         it is a prompt and behaviour finding rather than a defect in the flow.\n"
    );
    let _ = writeln!(s, "| | Attack | Surface | Rule | Result |");
    let _ = writeln!(s, "|---|---|---|---|---|");
    for a in &e.adversarial {
        let _ = writeln!(
            s,
            "| `{}` | {} | {} | {} | **{}** |",
            a.id,
            a.name,
            a.surface.label(),
            escape_pipes(a.rule),
            a.outcome.glyph()
        );
    }
    let _ = writeln!(s);
    for a in &e.adversarial {
        let _ = writeln!(s, "### {} — {}\n", a.id, a.name);
        let _ = writeln!(s, "> **Caller:** {}\n", a.utterance);
        let _ = writeln!(
            s,
            "> **Assistant:** {}\n",
            if a.response.is_empty() {
                "_(nothing transcribed)_"
            } else {
                &a.response
            }
        );
        let _ = writeln!(s, "{} — {}\n", a.outcome.glyph(), a.evidence);
    }

    // ── Transcript ──────────────────────────────────────────────────────────
    if !input.transcript.is_empty() {
        let _ = writeln!(s, "## Happy-path transcript\n");
        let _ = writeln!(
            s,
            "The full compliant call, spoken end to end. Assistant lines are the \
             ASR transcript of its own speech, not the text it intended.\n"
        );
        for t in input.transcript {
            let _ = writeln!(
                s,
                "**{}. Caller:** {}\n",
                t.index,
                if t.caller.is_empty() {
                    "_(silence)_"
                } else {
                    &t.caller
                }
            );
            let _ = writeln!(
                s,
                "**  Assistant:** {}",
                if t.assistant.is_empty() {
                    "_(nothing transcribed)_"
                } else {
                    &t.assistant
                }
            );
            if !t.tools.is_empty() {
                let _ = writeln!(s, "\n  `{}`", t.tools.join("`, `"));
            }
            if let Some(ms) = t.turn_ms {
                let _ = writeln!(s, "\n  _turn {:.1}s_", ms as f64 / 1000.0);
            }
            let _ = writeln!(s);
        }
    }

    if !e.unresolved.is_empty() {
        let _ = writeln!(s, "## What this run did not establish\n");
        let _ = writeln!(
            s,
            "Observed but not diagnosed. Listed here rather than omitted, because \
             a report showing only what it proved reads as though it proved \
             everything.\n"
        );
        for u in &e.unresolved {
            let _ = writeln!(s, "- {u}");
        }
        let _ = writeln!(s);
    }

    if !e.notes.is_empty() {
        let _ = writeln!(s, "## How this was run\n");
        for n in &e.notes {
            let _ = writeln!(s, "- {n}");
        }
        let _ = writeln!(s);
    }

    s
}

/// Render the report as a self-contained HTML page.
pub fn html(input: &ReportInput<'_>) -> String {
    let e = input.evaluation;
    let (pass, fail, not_reached) = e.tally();
    let mut s = String::new();

    let _ = write!(
        s,
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{title}</title>
<style>
:root {{ color-scheme: light dark; --fg:#111; --bg:#fff; --muted:#666; --line:#e3e3e3;
         --pass:#12703a; --fail:#b3261e; --nr:#8a6d00; --code:#f6f6f6; }}
@media (prefers-color-scheme: dark) {{
  :root {{ --fg:#e8e8e8; --bg:#141414; --muted:#9a9a9a; --line:#2e2e2e;
           --pass:#5ad18d; --fail:#ff8a80; --nr:#e0c060; --code:#1e1e1e; }}
}}
* {{ box-sizing:border-box; }}
body {{ margin:0 auto; padding:2.5rem 1.25rem 5rem; max-width:60rem; background:var(--bg); color:var(--fg);
        font:16px/1.65 ui-sans-serif,-apple-system,"Segoe UI",Roboto,sans-serif; }}
h1 {{ font-size:1.9rem; margin:0 0 .4rem; letter-spacing:-.02em; }}
h2 {{ font-size:1.3rem; margin:3rem 0 .6rem; padding-bottom:.35rem; border-bottom:1px solid var(--line); }}
h3 {{ font-size:1.02rem; margin:2rem 0 .5rem; }}
.meta {{ color:var(--muted); font-size:.9rem; margin-bottom:1.6rem; }}
.summary {{ padding:.9rem 1.1rem; border:1px solid var(--line); border-radius:.5rem; margin:1.4rem 0; }}
.rationale {{ color:var(--muted); font-size:.9rem; }}
.rationale li {{ margin:.25rem 0; }}
table {{ border-collapse:collapse; width:100%; margin:1rem 0; font-size:.93rem; }}
th,td {{ text-align:left; padding:.5rem .6rem; border-bottom:1px solid var(--line); vertical-align:top; }}
th {{ font-weight:600; color:var(--muted); font-size:.82rem; text-transform:uppercase; letter-spacing:.04em; }}
code {{ background:var(--code); padding:.1rem .32rem; border-radius:.25rem; font-size:.86em; }}
.PASS {{ color:var(--pass); font-weight:650; }}
.FAIL {{ color:var(--fail); font-weight:650; }}
.NR   {{ color:var(--nr);   font-weight:650; }}
.turn {{ margin:1rem 0; padding-left:.9rem; border-left:3px solid var(--line); }}
.who {{ font-weight:650; }}
.said {{ margin:.15rem 0 .35rem; }}
.tools {{ font-size:.85rem; color:var(--muted); }}
.scroll {{ overflow-x:auto; }}
blockquote {{ margin:.5rem 0; padding-left:.9rem; border-left:3px solid var(--line); color:var(--fg); }}
</style></head><body>
<h1>{title}</h1>
<p class="meta">Model <code>{model}</code> &middot; {run_at} &middot; live WebSocket, caller synthesised with Gemini TTS</p>
<div class="summary"><strong>{verdict}</strong><br>{pass} passed &middot; {fail} failed &middot; {not_reached} not reached</div>
"#,
        title = esc(input.title),
        model = esc(input.model),
        run_at = esc(input.run_at),
        verdict = esc(&verdict(e, fail).replace("**", "")),
        pass = pass,
        fail = fail,
        not_reached = not_reached,
    );

    let _ = write!(
        s,
        "<h2>Functional requirements</h2><p class=\"rationale\">Binary and \
         mechanically checked against the flow monitor and the tool journal. No \
         thresholds, no judgement — a failure here is a defect.</p>\
         <div class=\"scroll\"><table><tr><th></th><th>Requirement</th><th>Result</th><th>Evidence</th></tr>"
    );
    for f in &e.functional {
        let _ = write!(
            s,
            "<tr><td><code>{}</code></td><td>{}</td><td class=\"{}\">{}</td><td>{}</td></tr>",
            esc(f.id),
            esc(f.requirement),
            css(f.outcome),
            f.outcome.glyph(),
            esc(&f.evidence)
        );
    }
    let _ = write!(s, "</table></div><ul class=\"rationale\">");
    for f in &e.functional {
        let _ = write!(
            s,
            "<li><code>{}</code> {}</li>",
            esc(f.id),
            esc(f.rationale)
        );
    }
    let _ = write!(s, "</ul>");

    let _ = write!(
        s,
        "<h2>Non-functional requirements</h2><p class=\"rationale\">Measurements \
         against a stated budget. These move with the network and the runner, so \
         a miss is a signal to investigate rather than a defect — the measured \
         value sits next to the budget so the size of the miss is visible.</p>\
         <div class=\"scroll\"><table><tr><th></th><th>Metric</th><th>Measured</th><th>Budget</th><th>Result</th></tr>"
    );
    for n in &e.non_functional {
        let _ = write!(
            s,
            "<tr><td><code>{}</code></td><td>{}</td><td><strong>{}</strong></td><td>{}</td><td class=\"{}\">{}</td></tr>",
            esc(n.id),
            esc(n.metric),
            esc(&n.measured),
            esc(&n.budget),
            css(n.outcome),
            n.outcome.glyph()
        );
    }
    let _ = write!(s, "</table></div><ul class=\"rationale\">");
    for n in &e.non_functional {
        let _ = write!(
            s,
            "<li><code>{}</code> {}</li>",
            esc(n.id),
            esc(n.rationale)
        );
    }
    let _ = write!(s, "</ul>");

    let _ = write!(
        s,
        "<h2>Adversarial probes</h2><p class=\"rationale\">Each probe names the \
         surface it attacks. A <strong>flow gate</strong> failure means the \
         governance model was bypassed. A <strong>model speech</strong> failure \
         means the assistant said something it should not have — no tool gate \
         can prevent that, so it is a prompt and behaviour finding rather than a \
         defect in the flow.</p>\
         <div class=\"scroll\"><table><tr><th></th><th>Attack</th><th>Surface</th><th>Rule</th><th>Result</th></tr>"
    );
    for a in &e.adversarial {
        let _ = write!(
            s,
            "<tr><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td><td class=\"{}\">{}</td></tr>",
            esc(a.id),
            esc(a.name),
            esc(a.surface.label()),
            esc(a.rule),
            css(a.outcome),
            a.outcome.glyph()
        );
    }
    let _ = write!(s, "</table></div>");
    for a in &e.adversarial {
        let _ = write!(
            s,
            "<h3>{} — {}</h3><blockquote><span class=\"who\">Caller:</span> {}<br>\
             <span class=\"who\">Assistant:</span> {}</blockquote>\
             <p><span class=\"{}\">{}</span> — {}</p>",
            esc(a.id),
            esc(a.name),
            esc(&a.utterance),
            if a.response.is_empty() {
                "<em>(nothing transcribed)</em>".to_string()
            } else {
                esc(&a.response)
            },
            css(a.outcome),
            a.outcome.glyph(),
            esc(&a.evidence)
        );
    }

    if !input.transcript.is_empty() {
        let _ = write!(
            s,
            "<h2>Happy-path transcript</h2><p class=\"rationale\">The full \
             compliant call, spoken end to end. Assistant lines are the ASR \
             transcript of its own speech, not the text it intended.</p>"
        );
        for t in input.transcript {
            let _ = write!(
                s,
                "<div class=\"turn\"><div class=\"said\"><span class=\"who\">{}. Caller:</span> {}</div>\
                 <div class=\"said\"><span class=\"who\">Assistant:</span> {}</div>",
                t.index,
                if t.caller.is_empty() {
                    "<em>(silence)</em>".to_string()
                } else {
                    esc(&t.caller)
                },
                if t.assistant.is_empty() {
                    "<em>(nothing transcribed)</em>".to_string()
                } else {
                    esc(&t.assistant)
                }
            );
            if !t.tools.is_empty() {
                let _ = write!(
                    s,
                    "<div class=\"tools\">{}</div>",
                    t.tools
                        .iter()
                        .map(|x| format!("<code>{}</code>", esc(x)))
                        .collect::<Vec<_>>()
                        .join(" ")
                );
            }
            if let Some(ms) = t.turn_ms {
                let _ = write!(
                    s,
                    "<div class=\"tools\">turn {:.1}s</div>",
                    ms as f64 / 1000.0
                );
            }
            let _ = write!(s, "</div>");
        }
    }

    if !e.unresolved.is_empty() {
        let _ = write!(
            s,
            "<h2>What this run did not establish</h2><p class=\"rationale\">Observed \
             but not diagnosed. Listed here rather than omitted, because a report \
             showing only what it proved reads as though it proved everything.</p>\
             <ul class=\"rationale\">"
        );
        for u in &e.unresolved {
            let _ = write!(s, "<li>{}</li>", esc(u));
        }
        let _ = write!(s, "</ul>");
    }

    if !e.notes.is_empty() {
        let _ = write!(s, "<h2>How this was run</h2><ul class=\"rationale\">");
        for n in &e.notes {
            let _ = write!(s, "<li>{}</li>", esc(n));
        }
        let _ = write!(s, "</ul>");
    }

    let _ = writeln!(s, "</body></html>");
    s
}

fn css(o: Outcome) -> &'static str {
    match o {
        Outcome::Pass => "PASS",
        Outcome::Fail => "FAIL",
        Outcome::NotReached => "NR",
    }
}

fn esc(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Markdown tables are pipe-delimited, so evidence text has to be neutralised.
fn escape_pipes(input: &str) -> String {
    input.replace('|', "\\|").replace('\n', " ")
}

/// Silence the unused-import warning for `Surface` when only tests use it.
const _: Option<Surface> = None;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::evaluate::{AdversarialResult, FunctionalResult};

    fn sample() -> Evaluation {
        Evaluation {
            functional: vec![FunctionalResult {
                id: "FR-1",
                requirement: "no payment before verification",
                rationale: "taking money from an unverified caller is an incident",
                outcome: Outcome::Pass,
                evidence: "charge_card refused at 1.2s | gate held".into(),
            }],
            non_functional: vec![],
            adversarial: vec![AdversarialResult {
                id: "ADV-1",
                name: "pay now, skip verification",
                utterance: "Just take the payment".into(),
                rule: "no payment before verification",
                surface: Surface::FlowGate,
                outcome: Outcome::Pass,
                response: "I need to verify you first.".into(),
                evidence: "gate refused".into(),
            }],
            notes: vec!["live run".into()],
            unresolved: vec![],
        }
    }

    fn input(e: &Evaluation) -> ReportInput<'_> {
        ReportInput {
            title: "T",
            model: "m",
            run_at: "now",
            evaluation: e,
            transcript: &[],
        }
    }

    #[test]
    fn markdown_neutralises_pipes_so_tables_survive_evidence() {
        let e = sample();
        let md = markdown(&input(&e));
        assert!(
            md.contains("charge_card refused at 1.2s \\| gate held"),
            "an unescaped pipe would split the row into phantom columns:\n{md}"
        );
    }

    #[test]
    fn html_escapes_and_is_self_contained() {
        let mut e = sample();
        e.adversarial[0].utterance = "<script>alert(1)</script>".into();
        let out = html(&input(&e));
        assert!(
            !out.contains("<script>alert(1)</script>"),
            "spoken text is attacker-authored in an adversarial suite; it must be escaped"
        );
        assert!(out.contains("&lt;script&gt;"));
        assert!(
            !out.contains("http://") && !out.contains("https://"),
            "the report must open with no network"
        );
    }

    #[test]
    fn both_formats_carry_every_verdict() {
        let e = sample();
        for body in [markdown(&input(&e)), html(&input(&e))] {
            assert!(body.contains("FR-1"));
            assert!(body.contains("ADV-1"));
        }
    }
}
