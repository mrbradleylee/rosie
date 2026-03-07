# Rosie TUI Keymap Spec

## 1) Scope
This keymap applies to full-screen TUI mode only.

Command-line shortcuts remain outside this spec:
- `rosie --cmd`: keep existing command-mode behavior (current prompt flow with `e/r/q`).
- `rosie --ask "..."`: quick one-shot chat path (no TUI keymap).
- Default `rosie`: chat-only TUI.

## 2) Goals
- Vim-first navigation and editing.
- Predictable mode model (`Normal`, `Insert`, `Command`, `Modal`).
- Fast session switching and model selection.
- Minimal chord complexity for MVP.

## 3) Modes
- `Normal`: default mode for movement and actions.
- `Insert`: text entry in composer only.
- `Command`: colon prompt for typed commands.
- `Modal`: transient dialogs (confirm/delete/model selector/help).

Mode rules:
- App starts in `Normal`.
- `Esc` always exits `Insert`/`Command` and returns to `Normal`.
- `Esc` closes non-destructive modals; destructive modals require explicit confirm/cancel.

## 4) Focus Targets
- `sessions` pane
- `transcript` pane
- `composer` pane
- `modal` (takes exclusive focus)

When no modal is open, one pane is always focused.

## 5) Global Bindings (Normal)
- `h/j/k/l`: move focus or cursor contextually (see pane tables).
- `H`: previous pane focus.
- `L`: next pane focus.
- `1`: focus sessions.
- `2`: focus transcript.
- `3`: focus composer.
- `Tab`: cycle focus forward.
- `Shift+Tab`: cycle focus backward.
- `gg`: jump to top of current list/view.
- `G`: jump to bottom of current list/view.
- `/`: start search in active pane.
- `n`: next search result.
- `N`: previous search result.
- `:`: open command prompt.
- `?`: open keymap help modal.
- `q`: quit app (or close non-critical modal).

## 6) Composer Bindings
### Normal mode (composer focused)
- `i`: enter insert mode at cursor.
- `a`: enter insert mode after cursor.
- `A`: enter insert mode at end of line.
- `o`: insert newline below and enter insert mode.
- `Enter`: send message.
- `R`: regenerate last assistant response.
- `Ctrl+c`: cancel in-flight generation.

### Insert mode (composer focused)
- `Esc`: return to normal mode.
- `Shift+Enter`: newline.
- `Enter`: send message.
- `Ctrl+w`: delete previous word.
- `Ctrl+u`: delete to line start.

## 7) Sessions Pane Bindings (Normal)
- `j/k`: move selection down/up.
- `Enter` or `l`: open selected session.
- `h`: collapse/leave session pane focus.
- `n`: create new session.
- `r`: rename selected session.
- `x`: archive/unarchive selected session.
- `dd`: delete selected session (opens confirm modal).
- `m`: open model selector modal for selected session.
- `*`: toggle pin/unpin (optional; behind feature flag until implemented).

## 8) Transcript Pane Bindings (Normal)
- `j/k`: scroll down/up.
- `Ctrl+d`: half-page down.
- `Ctrl+u`: half-page up.
- `g` then `g`: jump oldest message.
- `G`: jump latest message.
- `y`: copy selected message to clipboard.
- `Y`: copy full assistant response block.
- `R`: regenerate last assistant response.

## 9) Model Selector Modal (Modal)
- `j/k`: move model selection.
- `/`: filter models.
- `Enter`: apply model to current session.
- `d`: set selected model as default.
- `gr`: refresh model list from Ollama.
- `Esc` or `q`: close modal.

## 10) Search Behavior
- `/` opens an inline search prompt scoped to active pane.
- Search scopes:
  - sessions pane: session title + tags/metadata fields (when added)
  - transcript pane: message content
- `n/N` repeat last search in same scope.

## 11) Command Prompt (`:`)
Supported initial commands:
- `:new` create session.
- `:rename` rename focused session.
- `:archive` archive focused session.
- `:unarchive` unarchive focused session.
- `:model` open model selector.
- `:quit` quit app.

Rules:
- Unknown command shows non-blocking error in status line.
- Command history is session-local for MVP (can be global later).

## 12) Conflict and Precedence Rules
- Modal keybindings always win while modal is open.
- Insert mode only captures editing keys in composer focus.
- If a key has pane meaning and global meaning, pane meaning wins.
- Multi-key sequences (`gg`, `dd`, `gr`) use a timeout (recommended: 400ms).

## 13) Reserved Keys (Do Not Bind Yet)
- `s`: future split/thread features.
- `t`: future transcript view toggles.
- `z` prefixed motions for fold/expand behavior.

## 14) Accessibility / Fallback
- Arrow keys mirror `h/j/k/l` navigation.
- `Ctrl+n`/`Ctrl+p` mirror next/prev list selection in all lists.
- `F1` opens help (same as `?`).

## 15) MVP Set (Implement First)
Implement first and keep stable:
- Focus/pane navigation: `Tab`, `Shift+Tab`, `1/2/3`, `j/k`, `gg`, `G`
- Composer: `i`, `Esc`, `Enter`, `Shift+Enter`, `Ctrl+c`
- Sessions: `n`, `r`, `x`, `dd`, `Enter`
- Model selector: `m`, `j/k`, `Enter`, `gr`, `Esc`
- Help/quit: `?`, `q`

Everything else can follow in a second pass.
