use std::fmt;

/// Names the session a row belongs to, e.g. `claude/hum·gate`. Kept apart
/// until it is fit to a width, so a cut can keep the session whole.
#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    pub kind: &'static str,
    /// None when the session's cwd is unknown; shows as `?`.
    pub project: Option<String>,
    /// See [`crate::agents::Agent::session`].
    pub session: Option<String>,
}

impl Label {
    /// `kind/project·session` up to `max` chars, see [`fit_tagged`].
    pub fn fit(&self, max: usize) -> String {
        fit_tagged(
            &format!("{}/{}", self.kind, self.project()),
            self.session.as_deref(),
            max,
        )
    }

    /// `project·session` up to `max` chars, for rows that show the kind
    /// apart.
    pub fn fit_project(&self, max: usize) -> String {
        fit_tagged(self.project(), self.session.as_deref(), max)
    }

    fn project(&self) -> &str {
        self.project.as_deref().unwrap_or("?")
    }
}

impl fmt::Display for Label {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}/{}", self.kind, self.project())?;
        match &self.session {
            Some(s) => write!(f, "·{s}"),
            None => Ok(()),
        }
    }
}

/// `text·session` up to `max` chars. A cut shortens `text` and keeps the
/// session whole; a session too long for that drops `text` and is cut.
fn fit_tagged(text: &str, session: Option<&str>, max: usize) -> String {
    let Some(session) = session else {
        return cut(text, max);
    };
    let tag = format!("·{session}");
    let room = max.saturating_sub(tag.chars().count());
    if room == 0 {
        return cut(&tag, max);
    }
    format!("{}{tag}", cut(text, room))
}

/// `text` up to `max` chars, ending in `…` when cut.
pub fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(project: &str, session: Option<&str>) -> Label {
        Label {
            kind: "claude",
            project: Some(project.into()),
            session: session.map(Into::into),
        }
    }

    #[test]
    fn fit_keeps_the_session() {
        assert_eq!(label("hum", None).fit(10), "claude/hum");
        assert_eq!(label("hum", Some("gate")).fit(15), "claude/hum·gate");
        assert_eq!(label("hum", Some("gate")).fit(12), "claude…·gate");
        assert_eq!(label("hum", Some("verylongname")).fit(9), "·verylon…");
    }

    #[test]
    fn fit_project_leaves_out_the_kind() {
        assert_eq!(label("hum", Some("gate")).fit_project(9), "hum·gate");
        assert_eq!(label("dotfiles", Some("gate")).fit_project(9), "dot…·gate");
    }

    #[test]
    fn unknown_project_shows_as_a_question_mark() {
        let unknown = Label {
            kind: "claude",
            project: None,
            session: Some("gate".into()),
        };
        assert_eq!(unknown.to_string(), "claude/?·gate");
        assert_eq!(unknown.fit_project(9), "?·gate");
    }
}
