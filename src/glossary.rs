//! The glossary: the user's words for things in a schema.
//!
//! People describe a task in the words of the product they use, such as "PR" or "draft", and a schema spells them
//! its own way, such as `PullRequest` and `PullRequest.isDraft`. A glossary is where the user writes the mapping
//! down, so a search for their word finds the schema's name. It is part of the user's configuration, read once at
//! startup, and nothing a tool call says can change it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The terms of one endpoint. Empty when the endpoint has no glossary.
#[derive(Debug, Default)]
pub(crate) struct Glossary {
    /// The file the terms were read from.
    pub(crate) source: Option<PathBuf>,
    /// The terms, in the order of their text.
    terms: Vec<Term>,
}

/// One term and what it stands for in the schema.
#[derive(Debug)]
pub(crate) struct Term {
    /// The term as the user wrote it, with single spaces between its words.
    pub(crate) text: String,
    /// The term's words, in lower case.
    words: Vec<String>,
    /// What the term stands for, each a `Type` or a `Type.member`, in the order written.
    pub(crate) targets: Vec<String>,
}

/// The file as written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGlossary {
    #[serde(default)]
    glossary: BTreeMap<String, Vec<String>>,
}

impl Glossary {
    /// Reads the glossary at `path`.
    ///
    /// # Errors
    ///
    /// Returns a message naming the file, and the term when one is at fault.
    pub(crate) fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("could not read the glossary {}: {error}", path.display()))?;
        let raw: RawGlossary = toml::from_str(&text).map_err(|error| {
            let line = error.span().map_or_else(String::new, |span| {
                format!(
                    " at line {}",
                    text.bytes().take(span.start).filter(|byte| *byte == b'\n').count() + 1
                )
            });
            format!(
                "the glossary {} is not valid{line}: {}. Terms go under `[glossary]`, each with a list of targets, \
                 such as `\"draft\" = [\"PullRequest.isDraft\"]`",
                path.display(),
                error.message()
            )
        })?;
        let mut terms: Vec<Term> = Vec::with_capacity(raw.glossary.len());
        for (text, targets) in raw.glossary {
            let term = Term::from_raw(&text, targets)
                .map_err(|problem| format!("the glossary {}: {problem}", path.display()))?;
            if let Some(earlier) = terms.iter().find(|earlier| earlier.words == term.words) {
                return Err(format!(
                    "the glossary {}: the terms `{}` and `{text}` are the same term, because terms are matched \
                     ignoring case and spacing. Keep one of them, with the targets of both",
                    path.display(),
                    earlier.text
                ));
            }
            terms.push(term);
        }
        Ok(Self {
            source: Some(path.to_path_buf()),
            terms,
        })
    }

    /// How many terms the glossary has.
    pub(crate) const fn len(&self) -> usize {
        self.terms.len()
    }

    /// Every term, for checking the glossary against a schema.
    pub(crate) fn terms(&self) -> &[Term] {
        &self.terms
    }

    /// The terms that `query` holds as whole words, in the order they appear, and the words of `query` that belong
    /// to no term, in lower case. Where terms overlap, the longest wins.
    pub(crate) fn found(&self, query: &str) -> (Vec<&Term>, Vec<String>) {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let mut found: Vec<&Term> = Vec::new();
        let mut rest = Vec::new();
        let mut at = 0;
        while let Some(word) = words.get(at) {
            let here = self
                .terms
                .iter()
                .filter(|term| words.get(at..at + term.words.len()) == Some(term.words.as_slice()))
                .max_by_key(|term| term.words.len());
            if let Some(term) = here {
                if !found.iter().any(|earlier| std::ptr::eq(*earlier, term)) {
                    found.push(term);
                }
                at += term.words.len();
            } else {
                rest.push(word.clone());
                at += 1;
            }
        }
        (found, rest)
    }
}

impl Term {
    /// Checks one term from the file.
    fn from_raw(text: &str, targets: Vec<String>) -> Result<Self, String> {
        let words: Vec<String> = text.split_whitespace().map(str::to_lowercase).collect();
        if words.is_empty() {
            return Err(format!(
                "the term `{text}` has no words in it. A term is a word or a few, such as `PR` or `pull request`"
            ));
        }
        if targets.is_empty() {
            return Err(format!(
                "the term `{text}` has no targets. Give it the schema's names for it, such as `[\"PullRequest\"]`, or \
                 remove it"
            ));
        }
        if let Some(target) = targets.iter().find(|target| !is_target(target)) {
            return Err(format!(
                "the term `{text}` has the target `{target}`, which is not a name in a schema. A target is a type, \
                 such as `PullRequest`, or a type and one of its fields, input fields or enum values, such as \
                 `PullRequest.isDraft`"
            ));
        }
        Ok(Self {
            text: text.split_whitespace().collect::<Vec<_>>().join(" "),
            words,
            targets,
        })
    }

    /// The term's words, in lower case.
    pub(crate) fn words(&self) -> &[String] {
        &self.words
    }
}

/// Whether `text` is `Type` or `Type.member`.
fn is_target(text: &str) -> bool {
    let mut parts = text.split('.');
    let named = parts.next().is_some_and(is_name) && parts.next().is_none_or(is_name);
    named && parts.next().is_none()
}

/// Whether `text` is a GraphQL name.
fn is_name(text: &str) -> bool {
    let mut characters = text.chars();
    characters
        .next()
        .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}
