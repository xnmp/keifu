# Agent Instructions

This project uses **GitHub Issues** for issue tracking, via the `gh` CLI.

## Debugging the TUI

To verify TUI behavior, rendering, keybindings, mouse, focus, or async loading,
drive the real app headlessly via its debug server — `cargo test` cannot see the
screen. See the `debug-tui` skill (`.claude/skills/debug-tui/SKILL.md`) and
`docs/debugging.md`. In short: `keifu --debug-listen 127.0.0.1:PORT` accepts
NDJSON commands (`keys`/`mouse`/`dump`/`state`) over TCP, and `keifu --log-file
PATH` writes a `tracing` log plus an exit-time perf summary (`KEIFU_LOG` sets
the level).

## Quick Reference

```bash
gh issue list                                  # Find available work
gh issue view <number>                         # View issue details
gh issue create --title "..." --body "..."     # File new work
gh issue close <number>                        # Complete work
```

## Non-Interactive Shell Commands

**ALWAYS use non-interactive flags** with file operations to avoid hanging on confirmation prompts.

Shell commands like `cp`, `mv`, and `rm` may be aliased to include `-i` (interactive) mode on some systems, causing the agent to hang indefinitely waiting for y/n input.

**Use these forms instead:**
```bash
# Force overwrite without prompting
cp -f source dest           # NOT: cp source dest
mv -f source dest           # NOT: mv source dest
rm -f file                  # NOT: rm file

# For recursive operations
rm -rf directory            # NOT: rm -r directory
cp -rf source dest          # NOT: cp -r source dest
```

**Other commands that may prompt:**
- `scp` - use `-o BatchMode=yes` for non-interactive
- `ssh` - use `-o BatchMode=yes` to fail instead of prompting
- `apt-get` - use `-y` flag
- `brew` - use `HOMEBREW_NO_AUTO_UPDATE=1` env var
