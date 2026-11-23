use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use sysinfo::Pid;

/// How long a subagent's transcript may go unwritten before it counts as
/// stopped, see [`running_subagents`]. A minute past Claude Code's longest
/// Bash timeout, 10 minutes, so a subagent waiting on one command stays
/// listed.
const SUBAGENT_QUIET: Duration = Duration::from_secs(660);

/// Claude Code's file for a running session. Its format is internal and
/// may change, so every field is optional.
#[derive(Default)]
#[cfg_attr(test, derive(Clone, Debug, PartialEq))]
pub struct SessionFile {
    pub cwd: Option<PathBuf>,
    /// The file's `status`: Claude Code writes `busy` for a whole turn, tool
    /// calls and API waits included, and `idle` at the prompt. Other values
    /// are unknown and give None.
    pub busy: Option<bool>,
    pub id: Option<SessionId>,
    pub name: Option<Name>,
}

/// Claude Code's `sessionId`, which names the directory of its subagents.
/// Only hex digits and `-`, since it comes from a file and ends up in a
/// path.
#[cfg_attr(test, derive(Clone, Debug, PartialEq))]
pub struct SessionId(String);

/// A non-empty session name, by its `nameSource`.
#[cfg_attr(test, derive(Clone, Debug, PartialEq))]
pub enum Name {
    /// Given by the user or by Claude Code: `user` or `auto`.
    Given(String),
    /// Derived from the first prompt, prefixed `<project>-`.
    Derived(String),
}

impl SessionFile {
    fn parse(json: &str) -> Self {
        let name = top_level_str(json, "name").filter(|n| !n.is_empty());
        Self {
            cwd: top_level_str(json, "cwd").map(PathBuf::from),
            busy: match top_level_str(json, "status").as_deref() {
                Some("busy") => Some(true),
                Some("idle") => Some(false),
                _ => None,
            },
            id: top_level_str(json, "sessionId")
                .filter(|id| {
                    !id.is_empty() && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
                })
                .map(SessionId),
            name: match top_level_str(json, "nameSource").as_deref() {
                Some("user" | "auto") => name.map(Name::Given),
                Some("derived") => name.map(Name::Derived),
                _ => None,
            },
        }
    }
}

/// A subagent Claude Code runs inside its own process, so it has no pid.
pub struct Subagent {
    /// The `<id>` in `agent-<id>.meta.json`.
    pub id: String,
    /// The meta file's `name`, else its `agentType`, e.g. `Explore`.
    pub kind: String,
}

/// Where [`read_session`] looks, from `$CLAUDE_CONFIG_DIR` and `$HOME`.
pub fn config(config_dir: Option<OsString>, home: Option<&Path>) -> Option<PathBuf> {
    config_dir
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| Some(home?.join(".claude")))
}

/// A running session's file under `$CLAUDE_CONFIG_DIR` or `~/.claude`.
/// None when the file is from an earlier process on the same pid, see
/// [`same_process`].
pub fn read_session(config: &Path, pid: Pid) -> Option<SessionFile> {
    let file = fs::read_to_string(config.join("sessions").join(format!("{pid}.json"))).ok()?;
    let stat = || fs::read_to_string(format!("/proc/{pid}/stat")).ok();
    same_process(&file, stat).then(|| SessionFile::parse(&file))
}

/// False when the file's `procStart` differs from the start time in clock
/// ticks in `/proc/<pid>/stat`, i.e. the file outlived its process and the
/// pid was reused. Its `startedAt` is no guard: on a live session it read
/// 223s after the process start. Files without `procStart` pass without
/// reading `stat`; a `procStart` that is not a number fails.
fn same_process(file: &str, stat: impl FnOnce() -> Option<String>) -> bool {
    let start = match top_level(file, "procStart") {
        None => return true,
        Some(Field::Str(v) | Field::Raw(v)) => v.trim().parse::<u64>(),
    };
    let Ok(start) = start else { return false };
    stat().is_some_and(|stat| stat_field(&stat, 22).is_some_and(|t| t.parse() == Ok(start)))
}

/// Field `n` of a `/proc/<pid>/stat` line, numbered from 1 as in proc(5),
/// for `n` of 3 on; None below. The comm in field 2 may hold spaces and `)`,
/// so fields are counted from the last `)`.
pub fn stat_field(stat: &str, n: usize) -> Option<&str> {
    let (_, rest) = stat.rsplit_once(')')?;
    rest.split_whitespace().nth(n.checked_sub(3)?)
}

/// Subagents still running in session `session` at `cwd`, oldest first.
/// Claude Code keeps each one's `agent-<id>.meta.json` and transcript
/// `agent-<id>.jsonl` under `projects/<cwd>/<session>/subagents`, and adds
/// `"finished":true` to the meta when it ends, but not when it is stopped.
/// So one counts as running only while its transcript changed within
/// [`SUBAGENT_QUIET`] of `now`.
pub fn running_subagents(
    config: &Path,
    cwd: Option<&Path>,
    session: &SessionId,
    now: SystemTime,
) -> Vec<Subagent> {
    let projects = config.join("projects");
    let in_project = |p: &Path| p.join(&session.0).join("subagents");
    // Claude Code names long paths differently, so the dirs are searched
    // when the name misses
    let named = cwd.map(|cwd| projects.join(project_dir(cwd)));
    // the subagents dir appears with the first subagent, so only a missing
    // project dir means the name missed
    let Some(dir) = named
        .filter(|p| p.is_dir())
        .map(|p| in_project(&p))
        .or_else(|| {
            fs::read_dir(&projects)
                .ok()?
                .filter_map(Result::ok)
                .map(|p| in_project(&p.path()))
                .find(|d| d.is_dir())
        })
    else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let quiet_since = now
        .checked_sub(SUBAGENT_QUIET)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let mut found: Vec<(SystemTime, Subagent)> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name();
            let id = name
                .to_str()?
                .strip_prefix("agent-")?
                .strip_suffix(".meta.json")?;
            let meta = e.metadata().ok()?;
            // checked before the meta is read, so subagents that ended long
            // ago cost two stats each; a transcript not yet created counts as
            // written with the meta
            let written = fs::metadata(dir.join(format!("agent-{id}.jsonl")))
                .and_then(|t| t.modified())
                .or_else(|_| meta.modified())
                .ok()?;
            if written < quiet_since {
                return None;
            }
            // its mtime moves when the subagent finishes, its birth time not
            let started = meta.created().or_else(|_| meta.modified()).ok()?;
            let kind = subagent(&fs::read_to_string(e.path()).ok()?)?;
            let id = id.to_string();
            Some((started, Subagent { id, kind }))
        })
        .collect();
    found.sort_by(|(a, x), (b, y)| a.cmp(b).then_with(|| x.id.cmp(&y.id)));
    found.into_iter().map(|(_, s)| s).collect()
}

/// The name of Claude Code's project dir for `cwd`: each UTF-16 unit but
/// ASCII letters and digits as `-`, as its JavaScript counts them.
fn project_dir(cwd: &Path) -> String {
    let mut name = String::new();
    for c in cwd.to_string_lossy().chars() {
        match c.is_ascii_alphanumeric() {
            true => name.push(c),
            false => name.extend(std::iter::repeat_n('-', c.len_utf16())),
        }
    }
    name
}

/// A subagent meta file's kind, see [`Subagent`]. None once it finished,
/// or when it names no kind.
fn subagent(meta: &str) -> Option<String> {
    if let Some(Field::Raw(v)) = top_level(meta, "finished")
        && v.trim() == "true"
    {
        return None;
    }
    let named = |key| top_level_str(meta, key).filter(|n| !n.is_empty());
    named("name").or_else(|| named("agentType"))
}

/// The string value of `key` in a JSON object's top level, skipping nested
/// objects and arrays. None when the key is missing, not a string, or the
/// JSON does not parse up to it.
fn top_level_str(json: &str, key: &str) -> Option<String> {
    match top_level(json, key)? {
        Field::Str(v) => Some(v),
        Field::Raw(_) => None,
    }
}

/// A top-level JSON value: a string unescaped, anything else as raw text.
enum Field {
    Str(String),
    Raw(String),
}

/// The value of `key` in a JSON object's top level, see [`top_level_str`].
fn top_level(json: &str, key: &str) -> Option<Field> {
    let mut chars = json.chars().peekable();
    let skip_ws = |chars: &mut Chars| while chars.next_if(|c| c.is_whitespace()).is_some() {};
    skip_ws(&mut chars);
    chars.next_if_eq(&'{')?;
    loop {
        skip_ws(&mut chars);
        let k = json_string(&mut chars)?;
        skip_ws(&mut chars);
        chars.next_if_eq(&':')?;
        skip_ws(&mut chars);
        let value = if chars.peek() == Some(&'"') {
            Field::Str(json_string(&mut chars)?)
        } else {
            Field::Raw(skip_json_value(&mut chars)?)
        };
        if k == key {
            return Some(value);
        }
        skip_ws(&mut chars);
        chars.next_if_eq(&',')?;
    }
}

type Chars<'a> = std::iter::Peekable<std::str::Chars<'a>>;

/// Reads a JSON string, quotes included, and returns it unescaped.
fn json_string(chars: &mut Chars) -> Option<String> {
    chars.next_if_eq(&'"')?;
    let mut out = String::new();
    loop {
        match chars.next()? {
            '"' => return Some(out),
            '\\' => out.push(match chars.next()? {
                'n' => '\n',
                't' => '\t',
                'r' => '\r',
                'b' => '\u{8}',
                'f' => '\u{c}',
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    char::from_u32(u32::from_str_radix(&hex, 16).ok()?).unwrap_or('\u{fffd}')
                }
                c => c,
            }),
            c => out.push(c),
        }
    }
}

/// Skips a non-string JSON value, nested objects and arrays included, up to
/// the `,` or `}` after it. Returns the text skipped outside nested strings,
/// e.g. `12` for a number.
fn skip_json_value(chars: &mut Chars) -> Option<String> {
    let mut depth = 0usize;
    let mut raw = String::new();
    loop {
        match *chars.peek()? {
            '"' => {
                json_string(chars)?;
            }
            ',' | '}' | ']' if depth == 0 => return Some(raw),
            c => {
                match c {
                    '{' | '[' => depth += 1,
                    '}' | ']' => depth -= 1,
                    _ => {}
                }
                raw.push(c);
                chars.next();
            }
        }
    }
}

/// Claude config dirs on disk for tests in other modules, so they need no
/// JSON of their own.
#[cfg(test)]
pub mod fixture {
    use super::*;

    /// A config dir with session `5e55` in the project of cwd `/x/hum`,
    /// holding `subagents` as (id, meta, seconds since its transcript was
    /// written), metas written in order. Returns the dir and now.
    pub fn subagent_dir(name: &str, subagents: &[(&str, &str, u64)]) -> (PathBuf, SystemTime) {
        let config = std::env::temp_dir().join(format!("hum-{name}-{}", std::process::id()));
        let dir = config.join("projects/-x-hum/5e55/subagents");
        fs::create_dir_all(&dir).unwrap();
        let now = SystemTime::now();
        for &(id, meta, quiet) in subagents {
            // past the kernel's coarse file timestamps, so start times differ
            std::thread::sleep(Duration::from_millis(20));
            fs::write(dir.join(format!("agent-{id}.meta.json")), meta).unwrap();
            let transcript = dir.join(format!("agent-{id}.jsonl"));
            let file = fs::File::create(&transcript).unwrap();
            file.set_modified(now - Duration::from_secs(quiet)).unwrap();
        }
        (config, now)
    }

    /// [`subagent_dir`] with one running `Explore` subagent, and the file of
    /// session `5e55` at `/x/hum`, reporting idle, for `pid`.
    pub fn idle_with_subagent(name: &str, pid: u32) -> (PathBuf, SystemTime) {
        let (config, now) = subagent_dir(name, &[("a1", r#"{"agentType":"Explore"}"#, 0)]);
        let sessions = config.join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        let file = r#"{"cwd":"/x/hum","sessionId":"5e55","status":"idle"}"#;
        fs::write(sessions.join(format!("{pid}.json")), file).unwrap();
        (config, now)
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::subagent_dir;
    use super::*;

    const HUM: &str = "/home/u/code/hum";

    fn id(id: &str) -> SessionId {
        SessionId(id.into())
    }

    #[test]
    fn session_file_parsed_once_into_fields() {
        let file = SessionFile::parse(
            r#"{"pid":1,"cwd":"/home/u/code/hum","status":"busy","sessionId":"14b4-39b0",
            "name":"gate","nameSource":"user"}"#,
        );
        assert_eq!(
            file,
            SessionFile {
                cwd: Some(PathBuf::from(HUM)),
                busy: Some(true),
                id: Some(id("14b4-39b0")),
                name: Some(Name::Given("gate".into())),
            }
        );
    }

    #[test]
    fn status_other_than_busy_or_idle_is_unknown() {
        let busy = |status: &str| SessionFile::parse(&format!(r#"{{"status":"{status}"}}"#)).busy;
        assert_eq!(busy("idle"), Some(false));
        assert_eq!(busy("compacting"), None);
    }

    #[test]
    fn name_by_its_source() {
        let name = |name: &str, source: &str| {
            SessionFile::parse(&format!(r#"{{"name":"{name}","nameSource":"{source}"}}"#)).name
        };
        assert_eq!(name("exec", "auto"), Some(Name::Given("exec".into())));
        assert_eq!(
            name("hum-fix-ci", "derived"),
            Some(Name::Derived("hum-fix-ci".into()))
        );
        assert_eq!(name("x", "other"), None);
        assert_eq!(name("", "user"), None);
        assert_eq!(SessionFile::parse(r#"{"name":"gate"}"#).name, None);
    }

    #[test]
    fn garbage_file_has_no_fields() {
        assert_eq!(SessionFile::parse("{not json"), SessionFile::default());
    }

    #[test]
    fn session_id_must_be_a_plain_id() {
        let parsed = |sid: &str| SessionFile::parse(&format!(r#"{{"sessionId":"{sid}"}}"#)).id;
        // resolves to the same dir, but a session file could point anywhere
        assert_eq!(parsed("5e55/../5e55"), None);
        assert_eq!(parsed(""), None);
        assert_eq!(parsed("5e55"), Some(id("5e55")));
    }

    #[test]
    fn file_of_earlier_process_on_pid_is_stale() {
        let file = r#"{"cwd":"/home/u/code/hum","procStart":"1223063","name":"gate"}"#;
        let stat = |start: u64| {
            move || {
                Some(format!(
                    "81045 (claude) S 1 2 3 {} {start} 0",
                    "0 ".repeat(15)
                ))
            }
        };
        assert!(same_process(file, stat(1223063)));
        assert!(!same_process(file, stat(1300000)));
        assert!(!same_process(file, || Some("garbage".into())));
        // stat gone, e.g. the process exited
        assert!(!same_process(file, || None));
        // a comm with spaces and parens
        let odd = format!("81045 (a) b) S 1 2 3 {} 1223063 0", "0 ".repeat(15));
        assert!(same_process(file, || Some(odd)));
    }

    #[test]
    fn proc_start_as_number_or_garbage() {
        let stat = || Some(format!("1 (claude) S 1 2 3 {} 1223063 0", "0 ".repeat(15)));
        assert!(same_process(r#"{"procStart":1223063,"cwd":"/x"}"#, stat));
        assert!(!same_process(r#"{"procStart":"soon"}"#, stat));
        assert!(!same_process(r#"{"procStart":{"t":1223063}}"#, stat));
        assert!(!same_process(r#"{"procStart":null}"#, stat));
    }

    #[test]
    fn missing_proc_start_skips_stat() {
        let stat = || -> Option<String> { panic!("stat read without procStart") };
        assert!(same_process(r#"{"cwd":"/x"}"#, stat));
    }

    #[test]
    fn empty_config_dir_falls_back_to_home() {
        let home = Some(Path::new("/home/u"));
        assert_eq!(
            config(Some(OsString::new()), home),
            Some(PathBuf::from("/home/u/.claude"))
        );
        assert_eq!(config(Some("/c".into()), home), Some(PathBuf::from("/c")));
    }

    #[test]
    fn nested_names_are_not_top_level() {
        let json = r#"{"formerNames":[{"name":"old","nameSource":"user"}],"meta":{"cwd":"/x"},
            "cwd":"/home/u/code/hum","nameSource":"user","name":"new \"one\"\\ é"}"#;
        assert_eq!(
            top_level_str(json, "name").as_deref(),
            Some("new \"one\"\\ é")
        );
        assert_eq!(top_level_str(json, "cwd").as_deref(), Some(HUM));
        let only_former =
            r#"{"cwd":"/home/u/code/hum","formerNames":[{"name":"old","nameSource":"user"}]}"#;
        assert_eq!(top_level_str(only_former, "name"), None);
        assert_eq!(top_level_str(r#"{"name":3}"#, "name"), None);
        assert_eq!(top_level_str("", "name"), None);
    }

    #[test]
    fn meta_names_a_running_subagent() {
        let meta = r#"{"agentType":"Explore","description":"x","spawnDepth":1}"#;
        assert_eq!(subagent(meta).as_deref(), Some("Explore"));
        let named = r#"{"agentType":"general-purpose","name":"code-review"}"#;
        assert_eq!(subagent(named).as_deref(), Some("code-review"));
    }

    #[test]
    fn finished_or_garbage_meta_is_no_subagent() {
        let meta = r#"{"agentType":"Explore","spawnDepth":1,"finished":true}"#;
        assert_eq!(subagent(meta), None);
        let meta = r#"{"agentType":"Explore","finished":false}"#;
        assert!(subagent(meta).is_some());
        assert_eq!(subagent("{not json"), None);
        assert_eq!(subagent(r#"{"agentType":""}"#), None);
    }

    fn kinds(found: &[Subagent]) -> Vec<(&str, &str)> {
        found
            .iter()
            .map(|s| (s.id.as_str(), s.kind.as_str()))
            .collect()
    }

    #[test]
    fn running_subagents_in_start_order() {
        let (config, now) = subagent_dir(
            "subagents",
            &[
                ("a2", r#"{"agentType":"Explore"}"#, 0),
                ("a1", r#"{"agentType":"Plan"}"#, 650),
                ("a3", r#"{"agentType":"Explore","finished":true}"#, 0),
                // stopped: Claude Code never marks it finished
                ("a0", r#"{"agentType":"Explore"}"#, 670),
            ],
        );
        let cwd = Some(Path::new("/x/hum"));
        let found = running_subagents(&config, cwd, &id("5e55"), now);
        // a cwd whose project dir name differs is searched for
        let searched = running_subagents(&config, Some(Path::new("/y")), &id("5e55"), now);
        let missing = running_subagents(&config, cwd, &id("beef"), now);
        fs::remove_dir_all(&config).unwrap();
        assert_eq!(kinds(&found), [("a2", "Explore"), ("a1", "Plan")]);
        assert_eq!(kinds(&searched), kinds(&found));
        assert!(missing.is_empty());
    }

    #[test]
    fn subagent_without_a_transcript_yet_is_running() {
        let (config, now) = subagent_dir("fresh", &[]);
        let dir = config.join("projects/-x-hum/5e55/subagents");
        fs::write(dir.join("agent-a1.meta.json"), r#"{"agentType":"Explore"}"#).unwrap();
        let found = running_subagents(&config, Some(Path::new("/x/hum")), &id("5e55"), now);
        fs::remove_dir_all(&config).unwrap();
        assert_eq!(kinds(&found), [("a1", "Explore")]);
    }

    #[test]
    fn project_dir_counts_utf16_units() {
        assert_eq!(
            project_dir(Path::new("/home/u/code/hum")),
            "-home-u-code-hum"
        );
        assert_eq!(project_dir(Path::new("/a.b/é/😀")), "-a-b-----");
    }
}
