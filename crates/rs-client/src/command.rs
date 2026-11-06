//! Building RealityScan CLI command strings.
//!
//! Commands are rendered the way RealityScan's command line expects them:
//! `-name arg1 arg2`, with arguments quoted when they contain spaces.
//! Arguments that would be ambiguous (embedded quotes, line breaks) are
//! rejected rather than guessed at.

use std::fmt::Write as _;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CommandError {
    #[error("invalid command name {0:?}")]
    InvalidName(String),
    #[error("argument {arg:?} of -{command} contains a character RealityScan's command line can't carry")]
    InvalidArgument { command: String, arg: String },
}

/// One CLI command and its arguments, e.g. `-add "Z:\photos\img 1.jpg"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    name: String,
    args: Vec<String>,
}

impl Command {
    /// `name` without the leading dash, e.g. `"align"`.
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), args: Vec::new() }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn args(&self) -> &[String] {
        &self.args
    }

    fn render_into(&self, out: &mut String) -> Result<(), CommandError> {
        if self.name.is_empty() || !self.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(CommandError::InvalidName(self.name.clone()));
        }
        out.push('-');
        out.push_str(&self.name);
        for arg in &self.args {
            if arg.contains(['"', '\n', '\r', '\0']) {
                return Err(CommandError::InvalidArgument { command: self.name.clone(), arg: arg.clone() });
            }
            // Quote anything that could split or be mistaken for a command.
            let needs_quotes = arg.is_empty() || arg.starts_with('-') || arg.contains(char::is_whitespace);
            if needs_quotes {
                let _ = write!(out, " \"{arg}\"");
            } else {
                let _ = write!(out, " {arg}");
            }
        }
        Ok(())
    }

    // Common commands. Paths are Windows paths as RealityScan sees them;
    // use `crate::path::to_wine` to convert Linux paths.

    pub fn new_scene() -> Self {
        Self::new("newScene")
    }
    pub fn load(project: impl Into<String>) -> Self {
        Self::new("load").arg(project)
    }
    pub fn save(project: impl Into<String>) -> Self {
        Self::new("save").arg(project)
    }
    pub fn add(image: impl Into<String>) -> Self {
        Self::new("add").arg(image)
    }
    pub fn add_folder(folder: impl Into<String>) -> Self {
        Self::new("addFolder").arg(folder)
    }
    pub fn align() -> Self {
        Self::new("align")
    }
    pub fn select_maximal_component() -> Self {
        Self::new("selectMaximalComponent")
    }
    pub fn set_reconstruction_region_auto() -> Self {
        Self::new("setReconstructionRegionAuto")
    }
    pub fn calculate_preview_model() -> Self {
        Self::new("calculatePreviewModel")
    }
    pub fn calculate_normal_model() -> Self {
        Self::new("calculateNormalModel")
    }
    pub fn calculate_high_model() -> Self {
        Self::new("calculateHighModel")
    }
    pub fn calculate_texture() -> Self {
        Self::new("calculateTexture")
    }
    /// Emits an `ON_TAG` event when the sequence reaches this point.
    pub fn tag(name: impl Into<String>) -> Self {
        Self::new("tag").arg(name)
    }
    /// `-set "key=value"` for application/project settings.
    pub fn set(key: &str, value: &str) -> Self {
        Self::new("set").arg(format!("{key}={value}"))
    }
    pub fn quit() -> Self {
        Self::new("quit")
    }
}

/// An ordered list of commands sent as one sequence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandSeq(Vec<Command>);

impl CommandSeq {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, cmd: Command) -> &mut Self {
        self.0.push(cmd);
        self
    }

    pub fn then(mut self, cmd: Command) -> Self {
        self.0.push(cmd);
        self
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn commands(&self) -> &[Command] {
        &self.0
    }

    pub fn render(&self) -> Result<String, CommandError> {
        let mut out = String::new();
        for (i, cmd) in self.0.iter().enumerate() {
            if i > 0 {
                out.push(' ');
            }
            cmd.render_into(&mut out)?;
        }
        Ok(out)
    }
}

impl From<Command> for CommandSeq {
    fn from(cmd: Command) -> Self {
        Self(vec![cmd])
    }
}

impl FromIterator<Command> for CommandSeq {
    fn from_iter<I: IntoIterator<Item = Command>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_plain_and_quoted_arguments() {
        let seq = CommandSeq::new()
            .then(Command::new_scene())
            .then(Command::add(r"Z:\home\me\RealityScan 2.2\jxl_test.jxl"))
            .then(Command::add(r"Z:\photos\a.jpg"))
            .then(Command::tag("ADDED"));
        assert_eq!(
            seq.render().unwrap(),
            r#"-newScene -add "Z:\home\me\RealityScan 2.2\jxl_test.jxl" -add Z:\photos\a.jpg -tag ADDED"#
        );
    }

    #[test]
    fn quotes_dash_and_empty_arguments() {
        let cmd = Command::new("simplify").arg("-1").arg("");
        assert_eq!(CommandSeq::from(cmd).render().unwrap(), r#"-simplify "-1" """#);
    }

    #[test]
    fn set_renders_key_value() {
        let seq = CommandSeq::from(Command::set("appIncSubdirs", "true"));
        assert_eq!(seq.render().unwrap(), "-set appIncSubdirs=true");
    }

    #[test]
    fn rejects_embedded_quotes_and_newlines() {
        assert!(CommandSeq::from(Command::add(r#"Z:\a"b.jpg"#)).render().is_err());
        assert!(CommandSeq::from(Command::add("Z:\\a\nb.jpg")).render().is_err());
    }

    #[test]
    fn rejects_bad_names() {
        assert!(CommandSeq::from(Command::new("")).render().is_err());
        assert!(CommandSeq::from(Command::new("align -quit")).render().is_err());
    }
}
