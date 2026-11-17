# hum

[![CI](https://github.com/danczw/hum/actions/workflows/ci.yml/badge.svg)](https://github.com/danczw/hum/actions/workflows/ci.yml)

A small terminal monitor for machines running coding agents. Built with [ratatui](https://ratatui.rs) and [sysinfo](https://crates.io/crates/sysinfo). Linux only.

- CPU, memory and swap
- Agents (claude, codex, aider, gemini, ...) with their process trees, marked `●` while busy and `○` while idle
- Listening TCP ports, tagged with the agent that owns them
- Orphans: processes an agent started that outlived their parent (detected while hum runs)

```sh
cargo run --release
```

Keys: `j`/`k` or arrows move a cursor, `Tab` switches section, `Enter` expands or collapses an agent tree, `e`/`c` expand or collapse all, `1` shows a bar per CPU core, `Esc` hides the cursor, `q` quits, `?` lists all keys.

## License

Apache-2.0, see [LICENSE](LICENSE).
