# Efficiency Improvements for Rosie

This document reflects the current Rosie implementation and prioritizes optimizations that appear both correct and worthwhile.

## Recommended Work

## 1. Cache Rendered Transcript Segments (High Impact)

### Why it matters
Rosie redraws the TUI frequently and rebuilds transcript lines on every frame. For assistant messages, that means re-running markdown parsing and code highlighting for content that is often unchanged.

The most promising target is not just caching the markdown parser output alone, but caching rendered transcript fragments per message and invalidating them when:
- the message content changes
- the theme changes
- the transcript width changes

### Suggested approach
- Add a render cache keyed by message identity plus render inputs such as width and theme key.
- Cache fully rendered message rows, not just partial markdown spans.
- Invalidate only the assistant message that is actively streaming instead of rebuilding all prior assistant output every frame.

### Expected impact
This should reduce repeated work during idle redraws and long conversations, especially for assistant responses that include markdown or fenced code blocks.

---

## 2. Reduce SQLite Writes During Streaming (High Impact)

### Why it matters
The current persistence pattern updates the assistant message content repeatedly while tokens stream in. That means the database can be written many times for a single answer.

This is likely a more meaningful database optimization than batching the initial insert and final update into one transaction.

### Suggested approach
- Debounce assistant-content persistence during streaming.
- Or persist only on completion, cancellation, and error.
- If crash recovery during streaming is important, persist periodically on a timer or token threshold rather than on every UI polling cycle.

### Expected impact
Fewer SQLite writes, less lock churn, and simpler persistence behavior during long responses.

---

## 3. Reuse a Shared `reqwest::Client` (Low-Medium Impact)

### Why it matters
Rosie currently creates a new `reqwest::Client` for chat streaming, model discovery, and title generation. Reusing one shared client is good practice and may improve connection reuse.

The likely benefit is modest because Rosie usually talks to a local Ollama server over `http://localhost:11434`, so this is not primarily about TLS handshakes.

### Suggested approach
- Create one `reqwest::Client` during app startup.
- Store it in shared app state or pass it into the async helpers that call Ollama.
- Reuse it for `stream_ollama_chat`, `fetch_ollama_models`, and `generate_session_title`.

### Expected impact
Small but worthwhile cleanup with some potential latency and allocation savings.

---

## 4. Cache Model Discovery Briefly (Low Impact)

### Why it matters
The model picker currently fetches model names each time it opens. That is correct behavior, but a short cache would avoid repeated calls when the picker is opened repeatedly in a short window.

### Suggested approach
- Cache the fetched model list for a short TTL such as 30 to 60 seconds.
- Invalidate the cache when the Ollama host changes.
- Keep the UX explicit if cached data is being shown.

### Expected impact
Minor reduction in redundant network calls and slightly faster picker opens.

---

## 5. Avoid Unnecessary Session Refreshes (Low Impact)

### Why it matters
`refresh_sessions` is called after many operations, and some of those calls are necessary only because session metadata may have changed. A lightweight dirty flag could avoid a subset of redundant reads.

### Suggested approach
- Add `sessions_dirty` state.
- Mark it when session metadata changes, such as create, rename, delete, model change, or message-count affecting operations.
- Skip `list_sessions()` when no session-visible state has changed.

### Expected impact
Small reduction in database reads and less churn in session-list UI updates.

## Not Recommended As Written

## 6. Batched SQLite Operations

This was previously framed as combining a few small writes into one transaction at the end of streaming. That is not the main issue in the current code. The real cost comes from repeated assistant-content updates during streaming, so reducing write frequency is a better target than adding a batch API.

## 7. Async Task Cleanup via Awaiting Aborted Handles

The current use of `JoinHandle::abort()` is not, by itself, evidence of leaked resources or undefined behavior. Waiting on aborted handles would also require a different control-flow shape than the current synchronous cancellation path. This is not a clear optimization target.

## 8. Configuration or Theme Resolution Caching

Theme resolution happens when opening or applying themes, not on every render frame. Caching it would add state with little payoff unless profiling shows theme file reads are unexpectedly expensive.

## 9. Transcript Scroll Caching

`transcript_scroll` itself is simple state, not an expensive computation. Time-based throttling on scroll updates would likely harm responsiveness. If transcript rendering is slow, the optimization target should be transcript rendering and line measurement instead.

## Already Implemented

## 10. Incremental Stream Parsing

The stream parser already processes response chunks incrementally, splits complete lines as they arrive, and retains only the unfinished remainder between chunks. This should not be tracked as future work.

## 11. Syntect Asset Initialization Caching

Syntax and theme assets for code highlighting are already memoized globally with `OnceLock`. Further optimization should focus on caching rendered output, not re-solving asset initialization.

## Notes

- Profile before and after any change with long conversations and large code blocks.
- Prefer cache invalidation tied to message content, width, and theme rather than broad global resets.
- Keep optimizations simple unless profiling demonstrates a real bottleneck.
