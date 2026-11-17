use std::ffi::OsString;
use std::time::Duration;

pub const USAGE: &str = "\
Usage: hum [options]

Options:
  --interval <secs>  Tick length in seconds, 0.2 to 3600 [default: 1]
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
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Action, String> {
    let mut opts = Opts {
        interval: Duration::from_secs(1),
        agents: Vec::new(),
    };
    let mut args = args.into_iter().map(|a| {
        a.into_string()
            .map_err(|a| format!("argument {} is not UTF-8", a.to_string_lossy()))
    });
    while let Some(arg) = args.next().transpose()? {
        let (flag, mut inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_owned(), Some(v.to_owned())),
            _ => (arg, None),
        };
        if inline.is_some() && matches!(flag.as_str(), "--help" | "--version") {
            return Err(format!("{flag} takes no value"));
        }
        let mut value = || {
            inline
                .take()
                .map(Ok)
                .or_else(|| args.next())
                .unwrap_or_else(|| Err(format!("{flag} needs a value")))
        };
        match flag.as_str() {
            "-h" | "--help" => return Ok(Action::Help),
            "-V" | "--version" => return Ok(Action::Version),
            "--interval" => opts.interval = interval(&value()?)?,
            "--agent" => opts.agents.push(agent(value()?)?),
            _ => return Err(format!("unknown argument {flag}")),
        }
    }
    Ok(Action::Run(opts))
}

/// sysinfo needs 0.2 seconds between cpu reads; past an hour a tick is
/// too slow to watch anything.
fn interval(s: &str) -> Result<Duration, String> {
    s.parse::<f64>()
        .ok()
        .filter(|secs| (0.2..=3600.0).contains(secs))
        .map(Duration::from_secs_f64)
        .ok_or_else(|| format!("--interval takes 0.2 to 3600 seconds, got {s}"))
}

/// A name is matched against a process or script basename, so it can hold
/// no '/'.
fn agent(name: String) -> Result<String, String> {
    if name.is_empty() || name.contains('/') {
        return Err(format!(
            "--agent takes a program name without '/', got '{name}'"
        ));
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Result<Opts, String> {
        match parse(args.iter().map(OsString::from))? {
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
    fn interval_takes_0_2_to_3600() {
        assert_eq!(
            run(&["--interval", "0.2"]).unwrap().interval,
            Duration::from_millis(200)
        );
        assert_eq!(
            run(&["--interval", "3600"]).unwrap().interval,
            Duration::from_secs(3600)
        );
        for bad in ["0", "0.1", "3601", "1e300", "-1", "nan", "inf", "abc"] {
            assert!(run(&["--interval", bad]).is_err(), "{bad}");
        }
    }

    #[test]
    fn agent_repeats() {
        let opts = run(&["--agent", "goose", "--agent=amp"]).unwrap();
        assert_eq!(opts.agents, ["goose", "amp"]);
    }

    #[test]
    fn agent_must_be_a_bare_name() {
        assert!(run(&["--agent", ""]).is_err());
        assert!(run(&["--agent="]).is_err());
        assert!(run(&["--agent", "/usr/bin/goose"]).is_err());
        assert!(run(&["--agent", "x/goose"]).is_err());
    }

    #[test]
    fn non_utf8_argument_is_an_error() {
        use std::os::unix::ffi::OsStringExt;
        let bad = OsString::from_vec(vec![b'-', b'-', 0xff]);
        assert!(parse([bad]).is_err());
        let bad = OsString::from_vec(vec![0xff]);
        assert!(parse([OsString::from("--agent"), bad]).is_err());
    }

    #[test]
    fn help_and_version_take_no_value() {
        assert!(parse([OsString::from("--help=x")]).is_err());
        assert!(parse([OsString::from("--version=x")]).is_err());
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
        let action = |a: &str| parse([OsString::from(a)]).unwrap();
        assert!(matches!(action("-h"), Action::Help));
        assert!(matches!(action("--help"), Action::Help));
        assert!(matches!(action("-V"), Action::Version));
        assert!(matches!(action("--version"), Action::Version));
    }
}
