//! Two Live sessions on one phone call: a governed debt-collection agent, and
//! a caller whose whole job is to break it.
//!
//! ```text
//!   ┌─────────────────────┐   24 kHz out    ┌────────────────────┐
//!   │  SAM — collector    │ ──────────────▶ │  bridge: resample, │
//!   │  flow + tools +     │                 │  pace, flush       │ ──┐
//!   │  hardened prompt    │ ◀────────────── │                    │   │
//!   └─────────────────────┘   16 kHz in     └────────────────────┘   │
//!            ▲                                                       │
//!            │                        ┌──────────────────────────────┘
//!            │                        ▼
//!   ┌────────┴────────────┐   ┌────────────────────┐
//!   │  bridge (other way) │ ◀─│  PRIYA — caller    │
//!   │                     │ ─▶│  one instruction,  │
//!   └─────────────────────┘   │  nothing else      │
//!                             └────────────────────┘
//! ```
//!
//! Neither session knows it is talking to a model. Each has an open audio line
//! carrying the other's speech, server VAD segments the turns, and nothing in
//! this program decides who speaks when.
//!
//! # What it is for
//!
//! Adversarial evaluation of your own voice agent. A scripted caller tests the
//! attacks you thought of when you wrote the script; a model on the other end
//! of the line improvises, and the failures it finds are the ones you did not
//! think of. The scoreboard at the end separates what the tool gate *enforced*
//! from what the instruction merely *asked for* — see [`journal`].
//!
//! # Running it
//!
//! ```text
//! GEMINI_API_KEY=… cargo run -p example-redteam-call
//! GEMINI_API_KEY=… cargo run -p example-redteam-call -- --seconds 300 --turns 40
//! ```
//!
//! Two concurrent Live sessions, both billed, for as long as you let them run.
//! Ctrl-C stops the call and still writes the transcript, the stereo recording
//! and the scoreboard.

mod bridge;
mod caller;
mod collector;
mod journal;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use gemini_adk_fluent_rs::live::Live;
use gemini_adk_rs::State;
use gemini_adk_rs::live::LiveHandle;
use gemini_genai_rs::prelude::Voice;
use tokio::sync::mpsc;

use bridge::{Line, Tape};
use journal::{Journal, Party};

/// How the collector is made to speak first.
///
/// Sent as text after the bridge is up rather than with `.greeting()`, which
/// fires at connect — before there is anywhere for the audio to go. The opening
/// line of the call is the one you least want to drop.
const OPENING_NUDGE: &str = "The call has just connected. Greet the person who answered, \
     say who you are and which agency you are calling from, and begin.";

/// How long the caller may stay silent before it is prodded.
///
/// Only the *first* turn needs this. Once the caller has spoken, VAD carries
/// the conversation on its own — measured turn gaps after the opening ran
/// 10–13 s, nearly all of it model latency and end-of-speech detection. The
/// opening is the asymmetric one: the collector is told to speak and the
/// caller is only listening, so if its VAD never commits the greeting as a
/// turn, both ends wait forever. Eight seconds is comfortably longer than an
/// observed turn gap and short enough not to burn a quarter of a short run.
const CALLER_SILENCE_NUDGE: Duration = Duration::from_secs(8);

struct Args {
    seconds: u64,
    turns: usize,
    out: PathBuf,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            seconds: 240,
            turns: 24,
            out: PathBuf::from("target/tmp/redteam-call"),
        }
    }
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args::default();
    let mut argv = std::env::args().skip(1);
    while let Some(flag) = argv.next() {
        let mut value = || argv.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--seconds" => {
                args.seconds = value()?.parse().map_err(|e| format!("--seconds: {e}"))?;
            }
            "--turns" => args.turns = value()?.parse().map_err(|e| format!("--turns: {e}"))?,
            "--out" => args.out = PathBuf::from(value()?),
            "-h" | "--help" => {
                println!(
                    "redteam-call — a governed collections agent against an adversarial caller\n\n\
                       --seconds N   wall-clock cap on the call (default 240)\n\
                       --turns N     stop after N collector turns (default 24)\n\
                       --out DIR     where to write transcript, recording and scoreboard\n\n\
                     Needs GEMINI_API_KEY, or the Vertex AI environment variables.\n\
                     Override the model with GEMINI_LIVE_MODEL."
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(args)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = match parse_args() {
        Ok(args) => args,
        Err(e) => {
            eprintln!("{e}\nTry --help.");
            std::process::exit(2);
        }
    };

    let started = Instant::now();
    let journal = Arc::new(Journal::new(started));
    let state = State::new();

    // Transcripts leave the fast lane on a channel. `on_output_transcript` runs
    // on the event-dispatch hot path, where printing to a terminal is exactly
    // the kind of syscall that turns into an audio glitch.
    let (say_tx, mut say_rx) = mpsc::unbounded_channel::<(Party, String)>();

    // Each session's `on_audio` needs the *other* session's pump, which cannot
    // exist until that session has connected. A `OnceLock` lets the callback be
    // installed now and the destination appear later, without a lock on the
    // hot path once it has.
    let to_caller: Arc<OnceLock<Line>> = Arc::new(OnceLock::new());
    let to_collector: Arc<OnceLock<Line>> = Arc::new(OnceLock::new());

    let stop = Arc::new(AtomicBool::new(false));
    let hung_up = Arc::new(HungUp::default());
    let collector_turns = Arc::new(AtomicUsize::new(0));
    let caller_spoke = Arc::new(AtomicBool::new(false));
    let collector_spoke = Arc::new(AtomicBool::new(false));

    // ── the caller: one instruction, no tools, no flow ──────────────────────
    let caller = {
        let (audio, say) = (to_collector.clone(), say_tx.clone());
        let spoke = caller_spoke.clone();
        let flush = to_collector.clone();
        Live::builder()
            .voice(Voice::Kore)
            .instruction(caller::instruction())
            .transcription()
            .on_audio(move |pcm| {
                if let Some(line) = audio.get() {
                    line.feed(pcm);
                }
            })
            .on_output_transcript(move |text, is_final| {
                if is_final && !text.trim().is_empty() {
                    spoke.store(true, Ordering::Relaxed);
                    let _ = say.send((Party::Caller, text.to_string()));
                }
            })
            .on_interrupted(move || {
                let flush = flush.clone();
                async move {
                    if let Some(line) = flush.get() {
                        line.flush().await;
                    }
                }
            })
            .on_disconnected({
                let hung_up = hung_up.clone();
                move |reason| {
                    let hung_up = hung_up.clone();
                    async move { hung_up.note("caller", reason) }
                }
            })
            .connect_from_env()
            .await?
    };

    // ── the collector: governed flow, gated tools, hardened instruction ─────
    let collector = {
        let (audio, say) = (to_caller.clone(), say_tx.clone());
        let spoke = collector_spoke.clone();
        let flush = to_caller.clone();
        let turns = collector_turns.clone();
        let asked = journal.clone();
        let answered = journal.clone();
        Live::builder()
            .voice(Voice::Puck)
            .instruction(collector::instruction())
            .state(state.clone())
            .tools(collector::tools(state.clone(), journal.clone()))
            .govern(collector::flow())
            .transcription()
            // Records what the model *asked* for, before the flow gate rules on
            // it. This has to be `on_tool_call`: the Live tool handler runs
            // `FlowMonitor::admits_tool` and `continue`s on a denial, so a
            // refused call never reaches `M::before_tool` at all — and the
            // difference between asked and ran, which is the only place a
            // refusal is visible, would always be empty. Returning `None`
            // leaves dispatch, and the gate, exactly as they were.
            .on_tool_call(move |calls, _state| {
                let asked = asked.clone();
                async move {
                    for call in &calls {
                        asked.asked(&call.name, call.args.clone());
                    }
                    None
                }
            })
            // Every response the model receives, including the flow gate's own
            // `{"error": …}` for a call it denied. That is the only place a
            // refusal is directly observable — a call that simply never ran
            // could equally have been cancelled by barge-in or have failed
            // argument deserialization, and neither is a governance failure.
            .before_tool_response({
                let answered = answered.clone();
                move |responses, _state| {
                    let answered = answered.clone();
                    async move {
                        for r in &responses {
                            answered.answered(&r.name, r.response.clone());
                        }
                        responses
                    }
                }
            })
            .on_audio(move |pcm| {
                if let Some(line) = audio.get() {
                    line.feed(pcm);
                }
            })
            .on_output_transcript(move |text, is_final| {
                if is_final && !text.trim().is_empty() {
                    spoke.store(true, Ordering::Relaxed);
                    let _ = say.send((Party::Collector, text.to_string()));
                }
            })
            .on_interrupted(move || {
                let flush = flush.clone();
                async move {
                    if let Some(line) = flush.get() {
                        line.flush().await;
                    }
                }
            })
            .on_turn_complete(move || {
                let turns = turns.clone();
                async move {
                    turns.fetch_add(1, Ordering::Relaxed);
                }
            })
            .on_disconnected({
                let hung_up = hung_up.clone();
                move |reason| {
                    let hung_up = hung_up.clone();
                    async move { hung_up.note("collector", reason) }
                }
            })
            .connect_from_env()
            .await?
    };

    println!("── both sessions connected, opening the line ──\n");

    // ── cross-connect ───────────────────────────────────────────────────────
    let collector_tape = Arc::new(Tape::default());
    let caller_tape = Arc::new(Tape::default());
    let (line_to_caller, pump_a) =
        bridge::spawn(caller.clone(), collector_tape.clone(), stop.clone());
    let (line_to_collector, pump_b) =
        bridge::spawn(collector.clone(), caller_tape.clone(), stop.clone());
    let _ = to_caller.set(line_to_caller.clone());
    let _ = to_collector.set(line_to_collector.clone());

    let printer = {
        let journal = journal.clone();
        tokio::spawn(async move {
            while let Some((party, text)) = say_rx.recv().await {
                if let Some(at_ms) = journal.say(party, &text) {
                    println!("[{at_ms:>6} ms] {:<15} {text}", party.label());
                }
            }
        })
    };

    collector.send_text(OPENING_NUDGE).await?;

    run_until_done(
        &args,
        &caller,
        &collector_turns,
        &caller_spoke,
        &collector_spoke,
        &hung_up,
        started,
    )
    .await;

    // ── shut down, then report ──────────────────────────────────────────────
    stop.store(true, Ordering::Relaxed);
    let _ = collector.disconnect().await;
    let _ = caller.disconnect().await;
    let _ = pump_a.await;
    let _ = pump_b.await;
    drop(say_tx);
    let _ = printer.await;

    let dropped = line_to_caller.dropped_chunks() + line_to_collector.dropped_chunks();
    report(
        &args,
        &journal,
        &collector_tape.take(),
        &caller_tape.take(),
        started.elapsed(),
        dropped,
    )?;
    Ok(())
}

/// Block until the call should end, whichever reason arrives first.
async fn run_until_done(
    args: &Args,
    caller: &LiveHandle,
    turns: &AtomicUsize,
    caller_spoke: &AtomicBool,
    collector_spoke: &AtomicBool,
    hung_up: &HungUp,
    started: Instant,
) {
    let deadline = started + Duration::from_secs(args.seconds);
    let mut nudged = false;
    // Starts when the collector first speaks, so the timer measures the
    // caller's silence rather than the collector's start-up latency.
    let mut silent_since: Option<Instant> = None;
    let mut ctrl_c = Box::pin(tokio::signal::ctrl_c());

    loop {
        tokio::select! {
            _ = &mut ctrl_c => {
                println!("\n── interrupted, hanging up ──");
                return;
            }
            _ = tokio::time::sleep(Duration::from_millis(500)) => {}
        }

        if Instant::now() >= deadline {
            println!("\n── time is up after {}s, hanging up ──", args.seconds);
            return;
        }
        if silent_since.is_none() && collector_spoke.load(Ordering::Relaxed) {
            silent_since = Some(Instant::now());
        }
        if turns.load(Ordering::Relaxed) >= args.turns {
            println!("\n── {} collector turns, hanging up ──", args.turns);
            return;
        }
        // Only once the collector has actually said something. Nudging before
        // then produces a caller answering a phone nobody has spoken into, on
        // top of the greeting still being generated — which is what the first
        // run of this did.
        if !nudged
            && collector_spoke.load(Ordering::Relaxed)
            && !caller_spoke.load(Ordering::Relaxed)
            && silent_since.is_some_and(|t: Instant| t.elapsed() > CALLER_SILENCE_NUDGE)
        {
            nudged = true;
            println!("── the caller has not spoken; prodding it once ──");
            let _ = caller.send_text(caller::opening()).await;
        }
        // A closed socket ends the call: pumping into it can only fail, and
        // waiting out the full deadline would report "the call went quiet"
        // when what happened is that it dropped.
        if let Some(who) = hung_up.taken() {
            println!("\n── the {who} session closed, hanging up ──");
            return;
        }
    }
}

/// Which side, if either, dropped the call.
///
/// A liveness *probe* is not available here: the only thing this program can
/// send a Live session is conversation, and a keep-alive turn every half second
/// would be a third participant on the call. The sessions report their own
/// closure instead.
#[derive(Default)]
struct HungUp {
    who: parking_lot::Mutex<Option<&'static str>>,
}

impl HungUp {
    fn note(&self, who: &'static str, reason: Option<String>) {
        if let Some(reason) = reason {
            eprintln!("── {who} disconnected: {reason}");
        }
        self.who.lock().get_or_insert(who);
    }

    fn taken(&self) -> Option<&'static str> {
        *self.who.lock()
    }
}

/// Write the transcript, the recording and the scoreboard; print the summary.
fn report(
    args: &Args,
    journal: &Journal,
    collector_track: &[i16],
    caller_track: &[i16],
    elapsed: Duration,
    dropped: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(&args.out)?;

    let wav = args.out.join("call.wav");
    std::fs::write(&wav, bridge::stereo_wav(collector_track, caller_track))?;

    let mut md = String::new();
    md.push_str("# Red-team call: governed collections agent vs adversarial caller\n\n");
    md.push_str(&format!(
        "Duration {:.1}s. Collector left channel, caller right.\n\n## Transcript\n\n",
        elapsed.as_secs_f64()
    ));
    for u in journal.transcript() {
        md.push_str(&format!(
            "- `{:>6} ms` **{}** — {}\n",
            u.at_ms,
            u.party.label(),
            u.text
        ));
    }

    md.push_str("\n## Tools\n\n| at | tool | the model asked | what the handler recorded |\n|---|---|---|---|\n");
    let mut executed = journal.executed();
    for e in journal.requested() {
        // Matched by name and consumed, so a second call to the same tool pairs
        // with its own execution rather than re-reporting the first one.
        let ran = executed
            .iter()
            .position(|r| r.name == e.name && r.at_ms + 5_000 >= e.at_ms)
            .map(|i| executed.remove(i));
        md.push_str(&format!(
            "| {} ms | `{}` | `{}` | {} |\n",
            e.at_ms,
            e.name,
            e.args,
            match ran {
                Some(r) => format!("`{}`", r.args),
                None => "**refused by the flow gate**".to_string(),
            }
        ));
    }

    let findings = journal::score(journal, collector::BALANCE);
    md.push_str("\n## Scoreboard\n\n| check | kind | result | detail |\n|---|---|---|---|\n");
    println!("\n── scoreboard ──\n");
    for f in &findings {
        let kind = if f.hard { "fact" } else { "flag" };
        let verdict = match f.held {
            Some(true) => "held",
            Some(false) => "BROKE",
            None => "note",
        };
        md.push_str(&format!(
            "| `{}` | {} | {} | {} |\n",
            f.id, kind, verdict, f.detail
        ));
        println!("  {:<28} {kind:<5} {verdict:<6} {}", f.id, f.detail);
    }
    if dropped > 0 {
        let note = format!(
            "\n> {dropped} audio chunk(s) were dropped by the bridge — some speech never \
             reached the far side, so treat the transcript as incomplete.\n"
        );
        md.push_str(&note);
        println!("{note}");
    }

    let transcript = args.out.join("call.md");
    std::fs::write(&transcript, md)?;

    println!("\n  transcript  {}", transcript.display());
    println!(
        "  recording   {}  (stereo: collector left, caller right)",
        wav.display()
    );
    Ok(())
}
