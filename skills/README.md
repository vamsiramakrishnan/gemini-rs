# Skills for coding agents

Skills that teach a coding agent (Claude Code, or any agent that reads
`SKILL.md` skills) to build with gemini-rs. Each directory is one skill: a
`SKILL.md` with the workflow, and `references/` the agent reads when the skill
points to them.

| Skill | Use it to |
|---|---|
| [`gemini-voice-workflow`](gemini-voice-workflow/SKILL.md) | Build or change a Gemini Live voice agent as an `agent.json` spec: brief, draft, `adk spec check` and `plan`, offline scenarios, tool stubs |

To install one for Claude Code, copy its directory into the project's
`.claude/skills/` or into `~/.claude/skills/`. The skills drive the `adk` CLI;
install it from this repository with
`cargo install --path tools/gemini-adk-cli-rs`.

Every JSON fragment in a skill's references has been run through
`adk spec check` and `adk spec test`. When the spec language or the runtime
changes, re-run them.
