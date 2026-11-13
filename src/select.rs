//! `select`: a small path into a response, such as `data.repository.pullRequests.nodes[*].number`, that picks the
//! part the calling agent wants after the response has arrived.
//!
//! A path is a chain of steps. A name steps into an object, `[N]` takes one item of a list, and `[*]` takes every
//! item. A path with `[*]` in it gives a list of every match, in order.

use std::fmt::Write as _;

use serde_json::Value;

use crate::suggest::backticked;

/// How a path is written, for error messages.
const SYNTAX: &str = "Write a path from the top of the response, such as `data.user.name` or \
     `data.repository.pullRequests.nodes[*].number`: names joined by `.`, `[0]` for one item of a list, and `[*]` for \
     every item.";

/// How many of an object's names to list when a path names one it does not have.
const MAX_KEYS: usize = 20;

/// One step of a path.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    /// Into the member of an object with this name.
    Key(String),
    /// The item of a list at this position.
    Index(usize),
    /// Every item of a list.
    All,
}

/// A parsed `select` path.
#[derive(Debug)]
pub(crate) struct Path {
    /// The path as the calling agent wrote it.
    text: String,
    steps: Vec<Step>,
}

/// What a path picked out of a response.
#[derive(Debug)]
pub(crate) struct Selected {
    /// The value at the path, or the list of every match when the path has `[*]`. `None` when nothing matched.
    pub(crate) value: Option<Value>,
    /// One line that says what was selected, or why nothing was.
    pub(crate) line: String,
}

impl Selected {
    /// What is selected from a body that is not JSON, which cannot happen for a body already split into parts.
    pub(crate) fn nothing() -> Self {
        Self {
            value: None,
            line: "`select` matched nothing: the response is not JSON.".to_owned(),
        }
    }
}

impl Path {
    /// Parses a path.
    ///
    /// # Errors
    ///
    /// Returns a message that shows how a path is written when `text` is not one.
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let invalid = |why: &str| format!("`select` is not a path: {why}. {SYNTAX} Nothing was sent.");
        let mut steps = Vec::new();
        for part in text.split('.') {
            let (name, brackets) = part
                .split_once('[')
                .map_or((part, None), |(name, rest)| (name, Some(rest)));
            if name.is_empty() {
                return Err(invalid(if part.is_empty() {
                    "it is empty, or has two dots in a row"
                } else {
                    "a `[` has to follow a name, as in `nodes[*]`"
                }));
            }
            if !name
                .chars()
                .all(|character| character.is_alphanumeric() || character == '_')
            {
                return Err(invalid(&format!("`{name}` is not a name")));
            }
            steps.push(Step::Key(name.to_owned()));
            let Some(mut rest) = brackets else { continue };
            loop {
                let Some((inside, after)) = rest.split_once(']') else {
                    return Err(invalid("a `[` is not closed"));
                };
                steps.push(match inside {
                    "*" => Step::All,
                    number => Step::Index(
                        number
                            .parse()
                            .map_err(|_error| invalid(&format!("`[{number}]` is neither `[*]` nor a position")))?,
                    ),
                });
                if after.is_empty() {
                    break;
                }
                rest = after
                    .strip_prefix('[')
                    .ok_or_else(|| invalid(&format!("`{after}` follows a `]` without a `.`")))?;
            }
        }
        Ok(Self {
            text: text.to_owned(),
            steps,
        })
    }

    /// Whether the path starts at the response's `errors`.
    pub(crate) fn is_in_errors(&self) -> bool {
        self.steps.first() == Some(&Step::Key("errors".to_owned()))
    }

    /// Picks this path out of `response`.
    pub(crate) fn select(&self, response: &Value) -> Selected {
        let mut current = vec![response];
        let mut missed = None;
        for (index, step) in self.steps.iter().enumerate() {
            let next: Vec<&Value> = current
                .iter()
                .flat_map(|value| -> Vec<&Value> {
                    match (step, value) {
                        (Step::Key(name), Value::Object(map)) => map.get(name).into_iter().collect(),
                        (Step::Index(position), Value::Array(items)) => items.get(*position).into_iter().collect(),
                        (Step::All, Value::Array(items)) => items.iter().collect(),
                        _ => Vec::new(),
                    }
                })
                .collect();
            if next.is_empty() && missed.is_none() {
                // An empty list leaves nothing to look at, which is a result and not a miss.
                missed = current.first().map(|value| self.missed(index, value));
            }
            current = next;
        }
        if self.steps.contains(&Step::All) {
            let count = current.len();
            let noun = if count == 1 { "match" } else { "matches" };
            let why = missed.map_or_else(String::new, |why| format!(" The first miss: {why}"));
            return Selected {
                value: Some(Value::Array(current.into_iter().cloned().collect())),
                line: format!("Selected `{}`: {count} {noun}, as a list.{why}", self.text),
            };
        }
        current.first().map_or_else(
            || Selected {
                value: None,
                line: format!(
                    "`select` matched nothing: {}",
                    missed.unwrap_or_else(|| "the path leads nowhere.".to_owned())
                ),
            },
            |value| Selected {
                value: Some((*value).clone()),
                line: format!("Selected `{}`.", self.text),
            },
        )
    }

    /// Why step `index` matched nothing, given an example of what it was applied to.
    fn missed(&self, index: usize, value: &Value) -> String {
        let mut reached = String::new();
        for step in self.steps.iter().take(index) {
            match step {
                Step::Key(name) if reached.is_empty() => reached.push_str(name),
                Step::Key(name) => {
                    let _ = write!(reached, ".{name}");
                }
                Step::Index(position) => {
                    let _ = write!(reached, "[{position}]");
                }
                Step::All => reached.push_str("[*]"),
            }
        }
        let at = if reached.is_empty() {
            "the response".to_owned()
        } else {
            format!("`{reached}`")
        };
        let step = self.steps.get(index);
        match (step, value) {
            (Some(Step::Key(name)), Value::Object(map)) if map.is_empty() => format!("{at} has no `{name}`."),
            (Some(Step::Key(name)), Value::Object(map)) => {
                let more = map.len().saturating_sub(MAX_KEYS);
                let more = if more == 0 {
                    String::new()
                } else {
                    format!(" and {more} more")
                };
                let keys = backticked(map.keys().take(MAX_KEYS).map(String::as_str));
                format!("{at} has no `{name}`. It has {keys}{more}.")
            }
            (Some(Step::Index(position)), Value::Array(items)) => {
                format!("{at} has {} items, so there is no `[{position}]`.", items.len())
            }
            (_, Value::Null) => format!("{at} is null."),
            (Some(Step::Key(_)), _) => format!("{at} is not an object."),
            _ => format!("{at} is not a list."),
        }
    }
}
