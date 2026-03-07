# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in gemini-rs, please report it responsibly:

1. **Do not** open a public GitHub issue
2. Email the maintainer directly or use [GitHub's private vulnerability reporting](https://github.com/vamsiramakrishnan/gemini-rs/security/advisories/new)
3. Include a description, reproduction steps, and potential impact

We aim to respond within 48 hours and will coordinate disclosure timelines with you.

## Scope

Security concerns for this project include:

- **Credential leakage** — API keys or Bearer tokens exposed in logs, error messages, or network traffic
- **WebSocket injection** — Malformed server messages causing unexpected behavior
- **Dependency vulnerabilities** — Known CVEs in transitive dependencies

## Best Practices

When using gemini-rs in production:

- Never hardcode API keys — use environment variables or secret managers
- Use `VertexAIAuth::with_token_refresher()` for long-running services
- Keep dependencies updated: `cargo audit` catches known vulnerabilities
