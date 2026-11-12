//! The cost map: rules that replace the built-in cost or multiplier of the fields they match.
//!
//! The built-in costs are guesses. A cost map is where the owner of an API writes down what they know: this list
//! has about three items, that field is expensive. It is part of the user's configuration, read once at startup,
//! and nothing a tool call says can change it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// One field on the way from the root of an operation down to the field being costed.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Step<'doc> {
    /// The type the field is selected on.
    pub(crate) parent: &'doc str,
    /// The field's name. Aliases play no part.
    pub(crate) field: &'doc str,
}

/// The rules of one endpoint. Empty when the endpoint has no cost map.
#[derive(Debug, Default)]
pub(crate) struct CostMap {
    /// The file the rules were read from.
    pub(crate) source: Option<PathBuf>,
    rules: Vec<Rule>,
}

/// One rule: a pattern and what it sets for the fields that match.
#[derive(Debug)]
struct Rule {
    pattern: Pattern,
    cost: Option<u64>,
    multiplier: Option<u64>,
}

/// A type followed by field names: `User.orders`, `Query.viewer.orders`, `*.orders`.
///
/// It matches a field that is reached through that chain of fields, starting from a selection on that type. A
/// root type is only ever selected on at the root, so a pattern that starts with one is an exact path. `*` stands
/// for any one name.
#[derive(Debug)]
struct Pattern {
    /// The pattern as written, for reporting which rule was used.
    text: String,
    /// The type the chain starts on.
    anchor: Part,
    /// The field names, outermost first.
    fields: Vec<Part>,
}

/// One part of a pattern.
#[derive(Debug, PartialEq, Eq)]
enum Part {
    /// A type or field name.
    Name(String),
    /// `*`: any one name.
    Any,
}

impl Part {
    /// Reads one part, or `None` when it is neither a name nor `*`.
    fn parse(text: &str) -> Option<Self> {
        if text == "*" {
            Some(Self::Any)
        } else {
            is_name(text).then(|| Self::Name(text.to_owned()))
        }
    }

    /// Whether this part stands for `name`.
    fn is(&self, name: &str) -> bool {
        match self {
            Self::Name(own) => own == name,
            Self::Any => true,
        }
    }

    /// Whether this part is a name, not a `*`.
    const fn is_name(&self) -> bool {
        matches!(self, Self::Name(_))
    }
}

/// One rule as written in the file.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRule {
    cost: Option<u64>,
    multiplier: Option<u64>,
}

impl CostMap {
    /// Reads the cost map at `path`.
    ///
    /// # Errors
    ///
    /// Returns a message naming the file, and the rule when one is at fault.
    pub(crate) fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("could not read the cost map {}: {error}", path.display()))?;
        let raw: BTreeMap<String, RawRule> = toml::from_str(&text).map_err(|error| {
            let line = error.span().map_or_else(String::new, |span| {
                format!(
                    " at line {}",
                    text.bytes().take(span.start).filter(|byte| *byte == b'\n').count() + 1
                )
            });
            format!(
                "the cost map {} is not valid{line}: {}",
                path.display(),
                error.message()
            )
        })?;
        let rules = raw
            .into_iter()
            .map(|(pattern, rule)| Rule::from_raw(&pattern, &rule))
            .collect::<Result<_, String>>()
            .map_err(|problem| format!("the cost map {}: {problem}", path.display()))?;
        Ok(Self {
            source: Some(path.to_path_buf()),
            rules,
        })
    }

    /// How many rules the map has.
    pub(crate) const fn len(&self) -> usize {
        self.rules.len()
    }

    /// The cost a rule sets for the field at the end of `path`, with that rule's pattern as written.
    pub(crate) fn cost(&self, path: &[Step<'_>]) -> Option<(u64, &str)> {
        self.setting(path, |rule| rule.cost)
    }

    /// The multiplier a rule sets for the field at the end of `path`, with that rule's pattern as written.
    pub(crate) fn multiplier(&self, path: &[Step<'_>]) -> Option<(u64, &str)> {
        self.setting(path, |rule| rule.multiplier)
    }

    /// Finds one setting among the rules that match `path` and set it. The most specific pattern wins.
    fn setting<T>(&self, path: &[Step<'_>], get: impl Fn(&Rule) -> Option<T>) -> Option<(T, &str)> {
        self.rules
            .iter()
            .filter(|rule| rule.pattern.matches(path))
            .filter_map(|rule| get(rule).map(|value| (value, &rule.pattern)))
            .max_by_key(|(_, pattern)| pattern.specificity())
            .map(|(value, pattern)| (value, pattern.text.as_str()))
    }
}

impl Rule {
    /// Checks one rule from the file.
    fn from_raw(pattern: &str, raw: &RawRule) -> Result<Self, String> {
        if raw.cost.is_none() && raw.multiplier.is_none() {
            return Err(format!("the rule `{pattern}` sets neither `cost` nor `multiplier`"));
        }
        Ok(Self {
            pattern: Pattern::parse(pattern)?,
            cost: raw.cost,
            multiplier: raw.multiplier,
        })
    }
}

impl Pattern {
    /// Reads a pattern such as `User.orders` or `*.orders`.
    fn parse(text: &str) -> Result<Self, String> {
        let not_a_pattern = || {
            format!(
                "the rule `{text}` is not a pattern. A pattern is a type name followed by one or more field names, \
                 with dots between them, such as `User.orders` or `Query.viewer.orders`. `*` stands for any one name"
            )
        };
        let mut parts = text.split('.').map(Part::parse);
        let anchor = parts.next().flatten().ok_or_else(not_a_pattern)?;
        let fields: Vec<Part> = parts.collect::<Option<_>>().ok_or_else(not_a_pattern)?;
        if fields.is_empty() {
            return Err(not_a_pattern());
        }
        Ok(Self {
            text: text.to_owned(),
            anchor,
            fields,
        })
    }

    /// Whether the field at the end of `path` is reached through this pattern's chain.
    fn matches(&self, path: &[Step<'_>]) -> bool {
        let Some(start) = path.len().checked_sub(self.fields.len()) else {
            return false;
        };
        let Some(chain) = path.get(start..) else { return false };
        chain.first().is_some_and(|step| self.anchor.is(step.parent))
            && chain.iter().zip(&self.fields).all(|(step, field)| field.is(step.field))
    }

    /// How specific the pattern is, for choosing between rules that match the same field. Greater wins.
    ///
    /// More parts beat fewer. Then more names beat more stars. Then a name nearer the field beats one further
    /// away, so `*.orders` beats `User.*`.
    fn specificity(&self) -> (usize, usize, Vec<bool>) {
        let nearest_first: Vec<bool> = self
            .fields
            .iter()
            .rev()
            .chain(std::iter::once(&self.anchor))
            .map(Part::is_name)
            .collect();
        let names = nearest_first.iter().filter(|is_name| **is_name).count();
        (nearest_first.len(), names, nearest_first)
    }
}

/// Whether `text` is a GraphQL name.
fn is_name(text: &str) -> bool {
    let mut characters = text.chars();
    characters
        .next()
        .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}
