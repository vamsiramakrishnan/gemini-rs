# From a spec to a project

A session spec (`agent.json`) describes the whole agent as data: model,
instruction, conversation or flow, tool declarations, and tests. What data
cannot carry is the tool implementations. `adk spec codegen` generates a
project in Rust, Python or Go around the spec. It has one typed function per
tool the spec declares as a mock.

```bash
adk spec codegen agent.json --lang python --out booking
```

Each generated function returns the spec's mock response until you replace
its body, so a new project behaves exactly like the spec does offline.
Whatever the language, the declaration in `agent.json` is what the model
sees, and the spec's `set_state` and `save_response_as` still apply to what
the function returns. Tools that already have an `http` or `mcp` binding
keep it and get no stub.

## What each language generates

| | Rust | Python | Go |
|---|---|---|---|
| Tools | `src/tools.rs`, registered in process | `tools.py`, served over MCP | `tools.go`, served over MCP |
| Arguments | a `Deserialize` struct per tool | typed keyword parameters | a struct with `json` tags |
| Runs the session | `cargo run` | a runtime that loads `agent.json` | a runtime that loads `agent.json` |
| Checks | `cargo test`: the spec validates, its tests and scenarios pass | `python -m unittest`: the server serves what `agent.json` binds | `go test`: the same, through an in-memory MCP client |

A Rust project compiles the spec in (`include_str!`) and passes the stubs to
[`SessionSpec::apply`](flow-json.md#tools-mock-http-mcp) with
`SpecResources::implement`.

Python and Go projects are MCP tool servers, built on the official `mcp`
package and the official Go SDK. Their `agent.json` binds each stubbed tool
to the server with the tool's `mcp` field (`python3 server.py` or `go run .`),
so any runtime that loads the spec starts the server and calls the tools
there. One protocol covers every language, and the same server works with
any other MCP client.

Parameter names that aren't identifiers in the target language, such as a
`from` parameter in Python or a `type` field in Rust, are renamed in the code
and mapped back to the declared name on the wire.

## Working with a project

```bash
adk spec test agent.json                    # validate; run tests and scenarios offline
adk spec call agent.json book_table '{"party_size": 4}'   # one call through its binding
adk spec run agent.json                     # a live session (text; audio with --features voice)
```

`adk spec call` calls a tool the way a session does: through its in-process
mock, HTTP binding or MCP server. It then prints the result and the state the
call wrote. Run it from a Python or Go project's directory to exercise that
project's server without a model.

`adk spec codegen` doesn't overwrite existing files unless you pass `--force`,
because the stubs are where your implementations go. For a Rust project built
against a local checkout of this repository, pass `--sdk-path <repo>`.

The Studio's `POST /api/flows/project` endpoint returns the same files as
JSON: `{"spec": …, "lang": "go"}`.

## Keeping generated projects honest

`scripts/check-generated-projects.sh` (`just generated-projects`) generates
all three projects for every Flow Studio gallery spec. It format-checks,
lints, builds and tests each one, then calls a tool through a Python and a Go
server. CI runs it on every change.
