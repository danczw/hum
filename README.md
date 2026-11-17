# hum

[![CI](https://github.com/danczw/hum/actions/workflows/ci.yml/badge.svg)](https://github.com/danczw/hum/actions/workflows/ci.yml)

A small terminal monitor for machines running coding agents. Built with [ratatui](https://ratatui.rs) and [sysinfo](https://crates.io/crates/sysinfo). Linux only.

- CPU, memory and swap
- Agents (claude, codex, aider, gemini, ...) with their process trees, marked `●` while busy and `○` while idle
- Listening TCP ports of agents and orphans, tagged with their owner. A `*` after the port (`:3000*`) marks a bind to a non-loopback address such as `0.0.0.0`; `a` shows every port
- Orphans: processes an agent started that outlived their parent. Claude Code marks its processes, so its orphans show even when they predate hum; for other agents hum catches them only while it runs

```sh
cargo run --release
```

Keys: `j`/`k` or arrows move a cursor, `Tab` switches section, `Enter` expands or collapses an agent tree, `e`/`c` expand or collapse all, `1` shows a bar per CPU core, `a` shows all ports, `Esc` hides the cursor, `q` quits, `?` lists all keys.

## License

Apache-2.0, see [LICENSE](LICENSE).
