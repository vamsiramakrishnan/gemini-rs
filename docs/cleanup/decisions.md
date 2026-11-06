# Cleanup decisions

Public API and semver decisions made during the cleanup. Each entry: what,
why, the alternative considered, and whether it breaks the 4.x API (a break
needs Vamsi's sign-off before it lands).

| # | Decision | Breaks 4.x? | Status |
|---|---|---|---|
| D1 | **GoAway handling at the wire layer.** `session_loop.rs` returns `DisconnectReason::GoAway` the moment a `goAway` frame arrives, and the connection reconnects with the latest resumption handle. Anything the server still sends on the old connection during `timeLeft` (the end of the model's current sentence, a final transcript) is lost; `session_lifecycle::go_away_reaches_the_callback_and_the_event_stream_with_its_deadline` pins this. Alternative: keep reading the old connection until the server closes it or `timeLeft` elapses, then reconnect. Google's documentation describes `timeLeft` as the time before the server terminates the connection, which argues for draining. Not changed in the cleanup: it is protocol behaviour, and the choice needs Vamsi. | No (behavioural) | Open |
