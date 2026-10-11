# Context steering: what the model is offered, and who decides

Status: implemented in part (PR #100). Date: 2026-10-11.

How should `contextUpdate` drive progressive discovery and keep the Live
model's context small? And how can a fast decision model (a "type 1"
system, here Jev) drive it, with or without a language model? This note
records the model the SDK now follows, the measurements behind it, what is
built, and what is next.

## What a `contextUpdate` is, exactly

It replaces two things, the declared tools and the system instruction, and
nothing else. It does not edit the conversation. Everything else the runtime
sends (stage postures and grounds, task context, per-turn modifiers) goes out
as content turns, which append to the conversation and cannot be taken back.

Four measured properties decide how to use it (Gemini 3.8 Live, Google AI,
`examples/context-update-spike`):

| Property | Measurement |
|---|---|
| It shrinks every turn's prompt | 12 declarations down to 1 cut the prompt from about 1,650 to about 700 tokens per turn |
| It invalidates the server's prefix cache | Send one only when the tools or the instruction change |
| It is processed in order with other input | Sent before a tool response, it is in effect when the model reads that response |
| It does not reach into a reply being formed | A tool declared before the caller's question: used in 5 of 5 replies. Declared 0, 300 or 800 ms after it: 0 of 5 each. Declared before the response to a call the model made for it: 5 of 5, about 450 ms later than declared ahead |

The last row is the one that shapes everything else. A decision made at the
turn boundary, after the caller's words, can change what the model has for
the *next* turn, not this one, unless a call is pending.

## Three channels, three lifetimes

| Channel | Carries | Changes | Sent as |
|---|---|---|---|
| Capabilities | The tools the model can call | When the step or the foreground task changes | `contextUpdate` tools |
| Standing instruction | Persona, catalog, the foreground skill's instruction | Rarely: a task gains or loses focus | `contextUpdate` instruction |
| Facts | Grounds, extracted values, results | Every turn | A context turn |
| Conversation | What was said | Always | The server's history, with sliding-window compression |

The rule is that what the model can do goes through `contextUpdate`, and
what it knows goes through turns. A value that changes every turn does not
belong in the instruction, where each change would invalidate the cache;
an instruction that does not change does not belong in a turn, where each
copy stays in the history. Task sessions broke the second half: every change
to a task's values re-sent the skill's whole instruction as "Current task
context (replaces earlier task context)", which replaced nothing. Under
`contextUpdate` the instruction now rides the system instruction and the
turn carries only what changed.

## When an update takes effect: sync points

1. **Before the caller's next turn.** An update sent at the turn boundary
   serves the next turn.
2. **Before a tool response.** The model is waiting, so an update sent first
   serves the same turn.
3. **Never mid-reply.** An update while the model forms or speaks a reply is
   accepted but does not change it.

So there are three ways to have the right tools in front of the model when
the caller asks for something.

## Progressive discovery, three ways

**Pull: the model asks for a capability.** It calls an entry point; the
runtime declares the capability's tools before the response. Measured: 5 of
5, about 450 ms slower than declared ahead. Skills now work this way: each
skill has a typed `start_{skill}` whose parameters are its inputs, and
starting it declares its tools before the response. The typed entry point
matters on its own: given only `task_control`'s free-form `input` object,
Gemini 3.8 Live left it out and retried the same failing start over a
hundred times in one turn.

**Offer ahead: the flow graph says what could open.** The runtime declares
the active steps' tools and also those of steps one decision away, and the
gate decides each call when it is made. "One decision away" is three-valued
logic over guards: every `decided` atom is unknown, since its question is
asked again about each turn, and everything else is evaluated as it stands
(`Guard::possible`). A step is on the frontier when that evaluation leaves
its eligibility unknown rather than false. Digressions whose trigger is a
decision offer their first steps' tools the same way. When the model calls
one of these tools, the gate asks exactly the questions in its way about the
turn in progress (about 250 ms with Jev), relatches, and admits the call
only if the step opened. No language model is involved in the offer, and the
decision is a typed answer with a probability.

**Predict: decide before the caller stops.** A decision model can answer on
the words recognised so far and declare what the caller is heading towards
before the turn ends. Measured offline on twelve caller lines against the
collections spec's five skills, asking Jev at every word boundary which skill
the caller needs:

| | Jev | `gemini-3.5-flash-lite` (budget 64) |
|---|---|---|
| Whole line right | 12/12 | 12/12 |
| Latency p50 / p90 | 319 / 416 ms | 714 / 820 ms |
| Prefixes confidently wrong | 0 of 163 at 0.85 | 11 of 163 (a label, no confidence) |
| Settles on the right skill (≥ 0.85, and stays) | 9 of 12 lines, at 47% of the line (median) | Always labels, so it cannot say "not yet" |

Loading by probability mass instead of a single pick works earlier: loading
the skills that cover 95% of Jev's probability put the right one in front of
the model in 81% of prefixes in the middle third of the line and 98% in the
last third, 1.4 to 1.5 skills on average out of five. A single label from a
language model cannot do this; it has no probability to threshold. Not built
yet: see Next.

## Type 1 without a language model

A decision model answers typed questions (yes/no, a choice, a score) with
probabilities, writes no text, and costs about 220 to 320 ms. Its roles in
context steering need no generation:

- **Deciding at the gate** whether a frontier step or digression opens, and
  whether a commit's confirmation was given. Fail-closed: an unsure answer
  admits nothing.
- **Routing over a catalog**, as a choice with probabilities, and loading by
  probability mass.
- **Relevance**: whether a block of facts matters now.

The content is authored: stage postures, grounds, skill instructions. The
decision model only selects among them, which keeps every context the model
sees reviewable and testable offline.

## Type 1 with a language model

Generation is needed only where the content does not exist yet:

- **The Live model pulls a capability** (a typed start). The decision model
  checks the pull where it matters: a transfer the caller did not ask for
  is refused at the gate (in a live run the model, unable to change a
  booking, tried `handoff_to_staff`; the caller had not asked for a person,
  and the call was refused).
- **"Decide whether, write what":** the decision model decides whether the
  conversation holds something the state does not (a caller's account of a
  dispute), and a text model writes the note off the critical path, where its
  latency does not hold up a turn.

In a governed flow the typed state is already the summary: verified
identity, chosen slot, booking reference. Grounding those in the facts
channel is what lets the sliding window drop old turns safely.

## Invariants

1. What the gate could admit in this turn is declared: the active steps'
   tools, commit tools before their guard, the frontier and the decidable
   digressions' entry tools.
2. Declared is not admitted. The gate decides every call.
3. A tool whose call is still open stays declared until its result is
   delivered.
4. An update is sent only when the tools or the instruction change, and
   before the tool response it matters for.
5. What is declared is observable: session state `declared_tools`.

## What PR #100 builds

- **Skills on `contextUpdate`.** A task session on a model that accepts it
  declares `task_control` and each `start_{skill}`, then the foreground
  skill's flow-offered tools; the skill instruction rides the system
  instruction; the context turn carries only changes; open calls keep their
  tool. On by default where the model accepts it; another steering mode
  turns it off.
- **Typed entry points** for every skill, with parameters projected onto
  what function declarations accept.
- **The flow frontier**: `Guard::possible`, `FlowMonitor::frontier_steps`,
  `frontier_questions`, digression entry offers, and the gate scope that
  asks their questions.
- **Probes**: the `discovery` probe in the spike, `tests/skills_live.rs`.

Skills, measured live (Gemini 3.8 Live, collections spec, typed caller):

| Arm | Prompt tokens, turn 1 / 2 / 3 (median of 3) | Journeys completed | Calls to another skill's tools |
|---|---|---|---|
| Foreground skill only (`contextUpdate`) | 3,076 / 3,914 / 4,846 | 3/3 | 0 |
| Every skill's tools | 4,845 / 6,191 / 6,494 | 3/3 | 0 |

The foreground skill's tools were callable in the turn that started it: in
every scoped run the first turn called `start_payment_history` and then
`verify_identity`, which the start had just declared. In two of three runs
per arm it also looked up the history in that turn; in the third it did so a
turn later, in both arms. Run it with
`cargo test -p gemini-adk-fluent-rs --test skills_live -- --ignored --nocapture`.

Flows, the live A/B with the frontier (11 scenarios, typed and spoken):
the Jev arm passed 10 of 11 in both, the handoff digression in every run,
and the slowest gate wait was 774 ms.

## Next

- **Offer on partial words.** Ask the governing questions on the recognised
  prefix and widen what is offered before the caller stops. Offer only,
  never admit: the gate still decides on the whole turn.
- **Catalogs larger than a handful of skills.** Declare the entry points of
  the skills covering 95% of a routing decision's probability, plus a
  catalog lookup, instead of every entry point.
- **Decisions in skill sessions.** A session with skills cannot have
  `decisions` yet, so skills' flows cannot use `decided` guards.
- **Arguments in guards.** A commit's arguments should be bound to what was
  checked; the pharmacy fixture submitted a refill for a different
  prescription than the one checked until its flow required the medication
  first.
- **An event for each update**, with its reason, for Studio and traces.
