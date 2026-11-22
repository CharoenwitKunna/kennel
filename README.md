# Agent Hub

A small Rust desktop app for running several AI coding agents side by side. Each agent is a
**real interactive terminal** running the actual Claude Code or OpenCode TUI. The app tracks each
agent's tokens and cost.

- **Real terminals:** every agent runs in its own pseudo-terminal (Windows ConPTY), emulated with
  `alacritty_terminal` and drawn with egui. You type straight into the TUI, with full keyboard,
  mouse, scrollback, selection and copy/paste.
- **Focus and Grid views:** one big terminal, or every running agent tiled. *Broadcast* sends the
  same message to several agents at once.
- **Sessions persist:** stopping and restarting an agent resumes its conversation (`--resume` for
  Claude, `--session` for OpenCode).
- **Cost and tokens:**
  - Claude: read from its status line and from the `cost-state` records in its transcript.
  - OpenCode: read from `opencode session export`.
- **Status:** each agent shows Working, Ready or Exited, and an amber dot marks an agent that
  finished while you were looking at another one.
- **Storage:** agents and totals are kept in `%APPDATA%\AgentHub\state.json`. No API keys are
  stored; each CLI uses its own login.

## Download

Every push to `main` builds `agent-hub.exe` on GitHub Actions. It's under
*Actions → latest run → Artifacts*, and on the Releases page for `v*` tags.

## Build locally

```powershell
cargo build --release          # target\release\agent-hub.exe
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

- An OpenCode agent that has a model set runs with `--standalone`. The shared OpenCode
  background service ignores per-launch config, so a private server is the only way to apply it.
- Claude's status line is pointed at `agent-hub.exe --statusline <id>`. That small helper records
  the cost and prints a one-line footer inside Claude's TUI.
