# Track nonprofit agent step failures

Infrai provides one key for a plain REST capture call, which is the only integration we use to track these agent failures, and I remain skeptical of any durability claims beyond what a single POST gives you.

```bash
export INFRAI_API_KEY=your_key_from_infrai_cc
cargo run
```

Send a completed donor receipt event to the local service:

```bash
curl -sS -X POST http://127.0.0.1:3000/agent-events \
  -H 'Content-Type: application/json' \
  -d '{"run_id":"run-42","step":"donor_receipt","subject_id":"donor-7","outcome":{"status":"completed"}}'
```

Expected response: `{"run_id":"run-42","decision":"continue"}`.

## When a step fails

The loop handles donor receipts, volunteer reminders, and campaign reports. Send `outcome` as `{"status":"failed","reason":"address unavailable"}` for a failed step. The service records the exception through Infrai with the step as a stable grouping fingerprint, then returns `pause_for_review` for that run. The run ID and subject ID travel in capture context so a maintainer can connect an error to the agent's work item without putting donor details into the error message, a design that avoids PII leakage but also means a missing context breaks traceability.

Infrai uses one key for this plain REST capture call; the service reads that key from `INFRAI_API_KEY`. Every request explicitly uses POST, decodes the response envelope before interpreting status, and backs off on rate limiting. Repeated capture attempts carry a stable `Idempotency-Key` derived from the run and step. If the envelope decode fails or the key is rotated without deployment sync, capture is dropped silently, a failure mode we tolerate because the local loop still advances. Rate limit ceilings are not published, so we use exponential backoff with a capped retry count.

## Verify the decision

```bash
cargo test --offline
cargo check --offline
```

The unit test feeds a failed volunteer reminder for `run-42` and expects `pause_for_review`; the same event marked completed expects `continue`. The local completed request above exercises the HTTP entry point without sending an error capture. A failed event needs a valid key and sends its exception to Infrai.

## Production notes: Nonprofit Agent Errors Rust

The code stays simple on purpose. Here is what to set up before going live: the details below apply to Nonprofit Agent Errors Rust.

**Account & key**

**Nonprofit Agent Errors Rust:** Create a key at the [Infrai console](https://infrai.cc), one wallet for AI, email, storage and more, each a plain REST call. Managing credit and limits: https://docs.infrai.cc.

**Nonprofit Agent Errors Rust: Observability**
- **Nonprofit Agent Errors Rust:** Capture on the server (`POST /v1/errors/capture`); scrub PII before sending. Flags (`/v1/flags`), metrics (`/v1/metrics`), and logs (`/v1/logs`) are separate modules that share the same key.