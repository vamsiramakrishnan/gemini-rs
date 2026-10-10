# Jev through Vercel AI Gateway (AI SDK)

One call to TypeSafe AI's Jev, a decision model, through Vercel AI Gateway with
the TypeScript AI SDK (`experimental_decide`). It asks the three question types
the Gemini Live runtime uses about one caller turn: a boolean (did the caller
confirm?), a choice (what happens next?) and a score (how frustrated are they?).
It prints the answers, the latency and the token usage.

```bash
# .env.local at the repository root (git-ignored)
AI_GATEWAY_API_KEY=...

npm install
npm run decide
```

Jev needs paid AI Gateway credits; on the free tier the Gateway answers 403
("Free tier users do not have access to this model").

The runtime integration is Rust: `gemini_adk_rs::decision` (feature
`ai-gateway`) and `decide` entries in a session spec. See
[Decision Models (Jev)](../../docs/user-guide/decisions.md).
