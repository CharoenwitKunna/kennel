# Kennel

A small Rust desktop app for running several AI coding agents side by side. Each agent runs in a
**real interactive terminal** hosting the actual Claude Code or OpenCode TUI, with live status,
token/context tracking, and persistent sessions and layout.

- **Real terminals:** every agent runs in its own pseudo-terminal (Windows ConPTY) attached to a
  Win32 Job Object (no orphaned child trees), emulated with `alacritty_terminal` and drawn with
  egui. Includes pixel-aligned vector box-drawing and block elements, full keyboard/mouse/IME
  support, `Ctrl+click` to open URLs and file paths, right-click copy/paste, drag-and-drop file
  path pasting, and incremental scrollback search (`Ctrl+Shift+F`).
- **Focus and Grid views:** one full-size terminal with browser-style tabs, or every running agent
  tiled with resizable splitters (double-click any divider to reset).
- **Sessions & layout persist:** stopping and restarting an agent resumes its conversation
  (`--resume` for Claude, `--session` for OpenCode), and agents running at exit auto-resume on next
  launch. Window size, position, maximized state, active view, sidebar width, and grid split ratios
  are saved automatically.
- **Tokens & context:**
  - Claude: reads token counts from transcripts and live context usage from its status line.
  - OpenCode: reads token totals via `opencode session export`.
- **Status & attention:** each agent shows Working (pulsing), Waiting for answer (`?` badge), Ready,
  or Exited, with unread output counters and an amber dot when an unfocused agent needs you.
- **Storage:** agents, settings, and window layout are kept in `%APPDATA%\Kennel\state.json`. No
  API keys are stored; each CLI uses its own login.

## Shortcuts

| Shortcut | Action |
|---|---|
| `Ctrl+Shift+G` | Toggle Focus / Grid view |
| `Ctrl+Shift+N` | Open "New agent" menu |
| `Ctrl+Shift+B` | Toggle sidebar |
| `Ctrl+Shift+F` | Search active terminal scrollback (`Enter` / `Shift+Enter` next/prev, `Esc` close) |
| `Ctrl+Shift+1`–`9` | Jump to agent 1–9 in Focus view |
| `Ctrl+=` / `Ctrl+-` / `Ctrl+0` | Increase / decrease / reset terminal font size |
| `Shift+PageUp` / `PageDown` / `Home` / `End` | Scroll terminal history |
| `Ctrl+Click` | Open URL or file path under cursor |
| Right-click | Copy selection, or paste clipboard if nothing is selected |
| Drag & drop files | Paste quoted file paths into the target terminal |
| `Ctrl+V` with an image or copied files | Paste the files' paths; an image is saved to `%TEMP%\kennel-pastes` as PNG first |

## Download

Get `kennel.exe` from the [Releases page](https://github.com/CharoenwitKunna/kennel/releases/latest).
Kennel checks for new releases itself and offers a one-click update (Settings → Updates).
Every push to `main` also builds an exe under *Actions → latest run → Artifacts*.

## CI and releases

- **CI** (every push and PR): `rustfmt` and `clippy -D warnings`, unit tests, a release build,
  and a smoke test of the exe.
- **Releasing**: *Actions → Release → Run workflow* (or `gh workflow run release.yml -f bump=patch`)
  bumps the version, tags it, and CI publishes the release with `kennel.exe` and its SHA-256.
  Pushing a `v*` tag by hand works too, as long as it matches the version in `Cargo.toml`.
- **Dependabot** opens weekly PRs for crate and GitHub Actions updates.

## Build locally

```powershell
cargo build --release          # target\release\kennel.exe
cargo test                     # unit tests (fast)
cargo build; cargo live        # live end-to-end tests against your logged-in CLIs
```

The live tests open real Claude Code and OpenCode sessions, so they need both CLIs installed and
logged in, and they use a little real usage.

## Permissions

| Setting           | Claude Code                  | OpenCode |
|-------------------|------------------------------|----------|
| Ask before acting | default (it asks in the TUI) | default  |
| Auto-accept edits | `acceptEdits`                | `--auto` |
| Full auto         | `bypassPermissions`          | `--auto` |

## Notes

- OpenCode uses the model you last picked inside it with `/models`. Per-launch config only
  reaches a `--standalone` server, which has no login, so Kennel leaves the model to OpenCode.
- Claude's status line is pointed at `kennel.exe --statusline <id>`. That small helper records
  live session metadata (including context fill) and prints a one-line footer inside Claude's TUI.
- Optional per-agent instructions in the Edit dialog are passed as `--append-system-prompt` for
  Claude and `--prompt` on a fresh session for OpenCode.
