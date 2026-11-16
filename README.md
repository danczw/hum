# hum

[![CI](https://github.com/danczw/hum/actions/workflows/ci.yml/badge.svg)](https://github.com/danczw/hum/actions/workflows/ci.yml)

A small terminal monitor for machines running coding agents. Built with [ratatui](https://ratatui.rs) and [sysinfo](https://crates.io/crates/sysinfo). Linux only.

- CPU, memory and swap
- Agents (claude, codex, aider, gemini, ...) with their process trees
- Listening TCP ports, tagged with the agent that owns them
- Orphans: processes an agent started that outlived their parent (detected while hum runs)

```sh
cargo run --release
```

Keys: `q` quits, `?` lists keys and columns, `b` switches between borderless and rounded sections (also `HUM_STYLE=borderless|rounded`, a dev toggle until one style is picked).

## License

Apache-2.0, see [LICENSE](LICENSE).
