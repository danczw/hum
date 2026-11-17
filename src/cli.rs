use std::time::Duration;

pub const USAGE: &str = "\
Usage: hum [options]

Options:
  --interval <secs>  Tick length in seconds, fractions allowed [default: 1]
  --agent <name>     Also treat processes named <name> as agents; repeatable
  -h, --help         Print this help
  -V, --version      Print the version";

pub struct Opts {
    pub interval: Duration,
    pub agents: Vec<String>,
}

pub enum Action {
    Run(Opts),
    Help,
    Version,
}

/// Parses the arguments after the program name.
pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Action, String> {
    let mut opts = Opts {
        interval: Duration::from_secs(1),
        agents: Vec::new(),
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_owned(), Some(v.to_owned())),
            _ => (arg, None),
        };
        let mut value = || {
            inline
                .clone()
                .or_else(|| args.next())
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag.as_str() {
            "-h" | "--help" => return Ok(Action::Help),
            "-V" | "--version" => return Ok(Action::Version),
            "--interval" => opts.interval = interval(&value()?)?,
            "--agent" => opts.agents.push(value()?),
            _ => return Err(format!("unknown argument {flag}")),
        }
    }
    Ok(Action::Run(opts))
}

fn interval(s: &str) -> Result<Duration, String> {
    s.parse::<f64>()
        .ok()
        .and_then(|secs| Duration::try_from_secs_f64(secs).ok())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| format!("--interval needs a number of seconds above 0, got {s}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Result<Opts, String> {
        match parse(args.iter().map(|a| a.to_string()))? {
            Action::Run(opts) => Ok(opts),
            _ => Err("not a run".into()),
        }
    }

    #[test]
    fn defaults() {
        let opts = run(&[]).unwrap();
        assert_eq!(opts.interval, Duration::from_secs(1));
        assert!(opts.agents.is_empty());
    }

    #[test]
    fn interval_takes_fractions() {
        assert_eq!(
            run(&["--interval", "0.5"]).unwrap().interval,
            Duration::from_millis(500)
        );
        assert_eq!(
            run(&["--interval=0.5"]).unwrap().interval,
            Duration::from_millis(500)
        );
    }

    #[test]
    fn interval_must_be_a_number_above_zero() {
        for bad in ["0", "-1", "nan", "inf", "abc"] {
            assert!(run(&["--interval", bad]).is_err(), "{bad}");
        }
    }

    #[test]
    fn agent_repeats() {
        let opts = run(&["--agent", "goose", "--agent=amp"]).unwrap();
        assert_eq!(opts.agents, ["goose", "amp"]);
    }

    #[test]
    fn unknown_flag_or_missing_value_is_an_error() {
        assert!(run(&["--nope"]).is_err());
        assert!(run(&["extra"]).is_err());
        assert!(run(&["--agent"]).is_err());
        assert!(run(&["--interval"]).is_err());
    }

    #[test]
    fn help_and_version() {
        let action = |a: &str| parse([a.to_string()]).unwrap();
        assert!(matches!(action("-h"), Action::Help));
        assert!(matches!(action("--help"), Action::Help));
        assert!(matches!(action("-V"), Action::Version));
        assert!(matches!(action("--version"), Action::Version));
    }
}
