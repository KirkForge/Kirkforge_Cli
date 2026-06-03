# ADR 002: TUI Framework and Rendering

## Status

Accepted

## Date

2026-06-03

## Context

The Rust CLI needs a terminal user interface. It must run on three machines — an 8GB C-50 laptop, a 16GB 2012 laptop, and an ARM Huawei P30 — with no GPU, no hardware acceleration, and variable terminal emulator quality. The UI requirements are:

- Chat view with streaming token rendering (model output arrives token by token over SSE/NDJSON)
- Code viewer with syntax highlighting (file reads displayed inline)
- Input bar with command history and edit buffer
- File tree or project overview (optional, for navigation)
- Status bar showing model, connection state, token count

The TUI must not be the bottleneck. Deadline rendering, janky scroll, or flicker defeats the purpose of a native client.

## Decision

Use **ratatui** with **crossterm** backend, rendering on a single-threaded event loop with a render-dirty flag.

### Why ratatui + crossterm over alternatives

| Alternative | Why rejected |
|---|---|
| cursive | Widget-based, opinionated layout. Hard to do streaming token rendering without fighting the widget lifecycle. Pool of custom widgets too small |
| termion | Linux-only. The P30 might run Termux on Android — different TTY. Also stale, unmaintained |
| bare crossterm | No layout system. Building a chat TUI from raw terminal escapes is writing ratatui badly |
| yew/ink! (WASM) | Wrong stack. Browser-in-terminal has overhead, defeats the static binary goal |
| ncurses-rs | C bindings. Static cross-compile gets harder. License (GPL) is friction |

ratatui gives us an immediate-mode renderer — redraw the full frame on each tick. No widget tree to update, no diffing, no lifecycle. Streaming tokens: buffer the incoming text, redraw when the buffer changes. That's the natural fit for an SSE stream.

### Event loop

```rust
loop {
    // Non-blocking poll
    if event_available() {
        handle_input(event);
        dirty = true;
    }

    // Non-blocking read from Ollama stream
    if let Some(chunk) = ollama_stream.try_recv() {
        append_to_conversation(chunk);
        dirty = true;
    }

    if dirty {
        terminal.draw(|f| render_ui(f, &state))?;
        dirty = false;
    }

    // Yield to OS when idle — C-50 has no cycles to spare
    if !dirty && ollama_stream.is_idle() {
        std::thread::sleep(Duration::from_millis(16)); // ~60fps cap
    }
}
```

### Streaming token rendering

Ollama's `/api/chat` returns NDJSON. Each chunk has `{ message: { content: "..." }, done: bool }`. The chat view renders a `Vec<Message>` where the last assistant message is "live" — appending on each chunk. ratatui's `Paragraph` widget handles this natively with scrollable text.

No virtual DOM. No reconciliation. Just buffer → render.

## Consequences

**Positive:**
- Single binary, no runtime deps beyond what crossterm needs (termios on Linux/Android)
- Immediate-mode rendering maps directly to SSE streaming — no widget lifecycle impedance
- ratatui's flexbox layout (constraints) handles resize for free
- Cross-platform: crossterm supports Linux, macOS, Windows, Android (Termux)
- Easy to test rendering by instantiating State + calling render functions

**Negative:**
- Immediate-mode means full redraw each frame. On the C-50's 1024x600 screen with a slow TTY, this might flicker. Mitigation: double-buffer (ratatui does this by default via crossterm's alternate screen), and dirty-flag checks to skip redraw when nothing changed
- Custom scrollback buffer needed — ratatui provides `Scrollbar` but chat history with variable-height tokens needs manual offset tracking
- No built-in Markdown or syntax highlighting without adding a parser (syntect or tree-sitter for highlighting; a lightweight Markdown parser for rendering)

## Open Questions

- Should syntax highlighting be lazy (render-on-scroll) or eager (highlight the visible window)? Lazy is cheaper on the C-50. Answer this during milestone 2 when tool views land.
- Markdown rendering depth: bold/italic/code/inline-code only? Or tables, blockquotes, headings, lists? Start minimal, extend when a model actually generates those elements.