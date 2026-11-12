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

/// A type followed by field names: `User.orders`, `Query.viewer.orders`.
///
/// It matches a field that is reached through that chain of fields, starting from a selection on that type. A
/// root type is only ever selected on at the root, so a pattern that starts with one is an exact path.
#[derive(Debug)]
struct Pattern {
    /// The pattern as written, for reporting which rule was used.
    text: String,
    /// The type the chain starts on.
    anchor: String,
    /// The field names, outermost first.
    fields: Vec<String>,
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

    /// Finds one setting among the rules that match `path`. The longest pattern wins.
    fn setting<T>(&self, path: &[Step<'_>], get: impl Fn(&Rule) -> Option<T>) -> Option<(T, &str)> {
        self.rules
            .iter()
            .filter(|rule| rule.pattern.matches(path))
            .filter_map(|rule| get(rule).map(|value| (value, &rule.pattern)))
            .max_by_key(|(_, pattern)| pattern.fields.len())
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
    /// Reads a pattern such as `User.orders`.
    fn parse(text: &str) -> Result<Self, String> {
        let mut names = text.split('.').map(str::to_owned);
        let anchor = names.next().unwrap_or_default();
        let fields: Vec<String> = names.collect();
        let well_formed = !fields.is_empty() && is_name(&anchor) && fields.iter().all(|field| is_name(field));
        if !well_formed {
            return Err(format!(
                "the rule `{text}` is not a pattern. A pattern is a type name followed by one or more field names, \
                 with dots between them, such as `User.orders` or `Query.viewer.orders`"
            ));
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
        chain.first().is_some_and(|step| step.parent == self.anchor)
            && chain.iter().zip(&self.fields).all(|(step, field)| step.field == field)
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
