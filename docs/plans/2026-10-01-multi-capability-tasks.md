# Multi-capability voice tasks

One Live session keeps the speaking identity and transport. Installed skills
describe reusable capabilities. Each activation owns a task identity, input
revision, private state, and optional governed conversation. Independent tasks
can wait for work concurrently, with one task in the foreground.

## Ownership and layers

L2 compiles `SkillSpec` contracts, tool bindings and conversations into
`CompiledSkill`. L1's `TaskRuntime` owns activations and operations. The existing
Live control lane hosts that reducer and delivers model updates. L0 continues
to carry the provider protocol. Offline replay invokes the same reducer with
controlled tools.

An invocation receives copied state and tools bound to that copy. A completion
can publish only to its owning task and revision. Each task admits one pending
operation, preventing conflicting state publication. Switching tasks preserves
the suspended task's accepted state and flow. Finishing a child resumes its
parent. Speech interruption does not imply cancellation of business work.

The alternative of adding another task-session actor was rejected because the
Live control lane already serializes lifecycle and model delivery. Static,
skill-qualified tool declarations preserve argument schemas; runtime admission
restricts them to the foreground skill.

## External effects

Tools declare read or commit effects. A commit creates an immutable approval
proposal bound to the operation, arguments, task, and revision. Approval and
reconciliation are trusted application commands, unavailable to model tools.
An atomic execution permit prevents a revoked proposal from starting later.
Started commits retain receipts even when the caller cancels or revises a task.
An uncertain outcome requires reconciliation before retrying its operation key.

Workers cannot reach the session writer. Late results remain with their owner;
another task receives no result payload. Foreground delivery and background
receipts use the existing provider scheduling protocol.

## Authoring and verification

Studio edits the same `SessionSpec` used by Rust authoring, CLI commands and
bundle export. Its task panel consumes typed runtime snapshots and commands.
The support example composes billing, FAQ, diagnostics and human handoff with
controlled tools. Scenarios and transport tests cover admission, approval,
cancellation, revisions, stale completions, retries, and task resumption.

Task-owned extraction, computed values, watchers, temporal patterns and memory
now run through reducer-owned service jobs and accepted effects. Each task has
one state-writing work slot, so service acceptance cannot race tool snapshots.
The Live adapter pins input before task routing and delivers speaking effects
only for their current owner. Existing extraction promotions, computed ordering,
watch predicates, temporal detectors and the independent memory engine are shared
with the single-workflow path. A separate Live-owned service map was rejected
because it would create another state-acceptance boundary and leave replay behind.

Unsupported combinations fail validation. Conversation frame extractors, resolver
slots, verbatim matching, stage voice timing, middleware and complete redaction
still need adapters. Snapshots are observations, not durable restore points.
See [the task guide](../user-guide/skills-and-tasks.md) for the supported API.
