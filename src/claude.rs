use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use sysinfo::Pid;

use crate::host::Host;

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
pub fn read_session(config: &Path, pid: Pid, host: &impl Host) -> Option<SessionFile> {
    let file = host.read(&config.join("sessions").join(format!("{pid}.json")))?;
    same_process(&file, pid, host).then(|| SessionFile::parse(&file))
}

/// False when the file's `procStart` differs from `pid`'s start time in
/// clock ticks, i.e. the file outlived its process and the pid was reused.
/// Its `startedAt` is no guard: on a live session it read 223s after the
/// process start. Files without `procStart` pass without reading the stat;
/// a `procStart` that is not a number fails.
fn same_process(file: &str, pid: Pid, host: &impl Host) -> bool {
    let start = match top_level(file, "procStart") {
        None => return true,
        Some(Field::Str(v) | Field::Raw(v)) => v.trim().parse::<u64>(),
    };
    let Ok(start) = start else { return false };
    host.stat(pid).is_some_and(|stat| stat.start == start)
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
    host: &impl Host,
) -> Vec<Subagent> {
    let projects = config.join("projects");
    let in_project = |p: &Path| p.join(&session.0).join("subagents");
    // Claude Code names long paths differently, so the dirs are searched
    // when the name misses
    let named = cwd.map(|cwd| projects.join(project_dir(cwd)));
    // the subagents dir appears with the first subagent, so only a missing
    // project dir means the name missed
    let Some(dir) = named
        .filter(|p| host.is_dir(p))
        .map(|p| in_project(&p))
        .or_else(|| {
            host.list(&projects)?
                .into_iter()
                .map(|p| in_project(&p))
                .find(|d| host.is_dir(d))
        })
    else {
        return Vec::new();
    };
    let Some(entries) = host.list(&dir) else {
        return Vec::new();
    };
    let quiet_since = now
        .checked_sub(SUBAGENT_QUIET)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let mut found: Vec<(SystemTime, Subagent)> = entries
        .into_iter()
        .filter_map(|path| {
            let id = path
                .file_name()?
                .to_str()?
                .strip_prefix("agent-")?
                .strip_suffix(".meta.json")?;
            let meta = host.times(&path)?;
            // checked before the meta is read, so subagents that ended long
            // ago cost two stats each; a transcript not yet created counts as
            // written with the meta
            let written = host
                .times(&dir.join(format!("agent-{id}.jsonl")))
                .map_or(meta.modified, |t| t.modified);
            if written < quiet_since {
                return None;
            }
            // its mtime moves when the subagent finishes, its birth time not
            let started = meta.created.unwrap_or(meta.modified);
            let kind = subagent(&host.read(&path)?)?;
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

/// Claude config dirs in memory for tests in other modules, so they need no
/// JSON of their own.
#[cfg(test)]
pub mod fixture {
    use super::*;
    use crate::host::MemHost;

    /// The config dir the fixtures write to.
    pub const CONFIG: &str = "/c";

    /// A fixed now, so file times need no sleeps between writes.
    pub fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    }

    /// Writes into `host` session `5e55` in the project of cwd `/x/hum`,
    /// holding `subagents` as (id, meta, seconds since its transcript was
    /// written), metas written a second apart in order, the last at
    /// [`now`].
    pub fn subagents(host: &mut MemHost, subagents: &[(&str, &str, u64)]) {
        let dir = Path::new(CONFIG).join("projects/-x-hum/5e55/subagents");
        for (n, &(id, meta, quiet)) in subagents.iter().enumerate() {
            let back = Duration::from_secs((subagents.len() - 1 - n) as u64);
            host.file(
                dir.join(format!("agent-{id}.meta.json")),
                meta,
                now() - back,
            );
            let quiet = Duration::from_secs(quiet);
            host.file(dir.join(format!("agent-{id}.jsonl")), "", now() - quiet);
        }
    }

    impl SessionFile {
        /// The file as Claude Code writes it.
        pub fn json(&self) -> String {
            let quote = |s: &str| format!(r#""{}""#, s.replace('\\', r"\\").replace('"', r#"\""#));
            let mut fields = Vec::new();
            if let Some(cwd) = &self.cwd {
                fields.push(format!(r#""cwd":{}"#, quote(&cwd.to_string_lossy())));
            }
            if let Some(busy) = self.busy {
                let status = if busy { "busy" } else { "idle" };
                fields.push(format!(r#""status":"{status}""#));
            }
            if let Some(id) = &self.id {
                fields.push(format!(r#""sessionId":{}"#, quote(&id.0)));
            }
            let name = match &self.name {
                Some(Name::Given(n)) => Some((n, "user")),
                Some(Name::Derived(n)) => Some((n, "derived")),
                None => None,
            };
            if let Some((name, source)) = name {
                fields.push(format!(r#""name":{},"nameSource":"{source}""#, quote(name)));
            }
            format!("{{{}}}", fields.join(","))
        }
    }

    #[test]
    fn json_round_trips_through_parse() {
        let file = SessionFile {
            cwd: Some(PathBuf::from(r"/x/a\b")),
            busy: Some(true),
            id: Some(SessionId("5e55".into())),
            name: Some(Name::Given(r#"new "one"\ é"#.into())),
        };
        assert_eq!(SessionFile::parse(&file.json()), file);
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::{self, CONFIG, now};
    use super::*;
    use crate::host::{MemHost, Stat};

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

    /// A host where pid 1 started at clock tick `start`.
    fn started(start: u64) -> MemHost {
        let mut host = MemHost::default();
        host.stats.insert(Pid::from_u32(1), Stat { start, cpu: 0 });
        host
    }

    /// [`same_process`] for pid 1.
    fn same(file: &str, host: &MemHost) -> bool {
        same_process(file, Pid::from_u32(1), host)
    }

    #[test]
    fn file_of_earlier_process_on_pid_is_stale() {
        let file = r#"{"cwd":"/home/u/code/hum","procStart":"1223063","name":"gate"}"#;
        assert!(same(file, &started(1223063)));
        assert!(!same(file, &started(1300000)));
        // stat gone, e.g. the process exited
        assert!(!same(file, &MemHost::default()));
    }

    #[test]
    fn proc_start_as_number_or_garbage() {
        let host = started(1223063);
        assert!(same(r#"{"procStart":1223063,"cwd":"/x"}"#, &host));
        assert!(!same(r#"{"procStart":"soon"}"#, &host));
        assert!(!same(r#"{"procStart":{"t":1223063}}"#, &host));
        assert!(!same(r#"{"procStart":null}"#, &host));
    }

    #[test]
    fn missing_proc_start_skips_stat() {
        let host = MemHost::default();
        assert!(same(r#"{"cwd":"/x"}"#, &host));
        assert!(host.reads.borrow().is_empty());
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
        let mut host = MemHost::default();
        fixture::subagents(
            &mut host,
            &[
                ("a2", r#"{"agentType":"Explore"}"#, 0),
                ("a1", r#"{"agentType":"Plan"}"#, 650),
                ("a3", r#"{"agentType":"Explore","finished":true}"#, 0),
                // stopped: Claude Code never marks it finished
                ("a0", r#"{"agentType":"Explore"}"#, 670),
            ],
        );
        let running = |cwd: &str, session: &str| {
            let found = running_subagents(
                Path::new(CONFIG),
                Some(Path::new(cwd)),
                &id(session),
                now(),
                &host,
            );
            kinds(&found)
                .into_iter()
                .map(|(id, kind)| format!("{id} {kind}"))
                .collect::<Vec<_>>()
        };
        assert_eq!(running("/x/hum", "5e55"), ["a2 Explore", "a1 Plan"]);
        // a cwd whose project dir name differs is searched for
        assert_eq!(running("/y", "5e55"), running("/x/hum", "5e55"));
        assert!(running("/x/hum", "beef").is_empty());
    }

    #[test]
    fn subagent_without_a_transcript_yet_is_running() {
        let mut host = MemHost::default();
        let meta = Path::new(CONFIG).join("projects/-x-hum/5e55/subagents/agent-a1.meta.json");
        host.file(meta, r#"{"agentType":"Explore"}"#, now());
        let cwd = Some(Path::new("/x/hum"));
        let found = running_subagents(Path::new(CONFIG), cwd, &id("5e55"), now(), &host);
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
