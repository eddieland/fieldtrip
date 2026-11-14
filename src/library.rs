//! The operation library: named operations the user trusts, read from a directory of `.graphql` files.
//!
//! Each file holds one named query or mutation, plus any fragments it uses. The comment lines at the top of the file
//! are its description. The library is config: it is read once, at startup, and no tool can change it. Whether an
//! operation still fits the schema is checked whenever the schema loads, so an operation broken by a change to the
//! API is reported rather than refused at startup.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use apollo_compiler::ExecutableDocument;
use apollo_compiler::ast::{Definition, Document, OperationType};
use apollo_compiler::executable::{Selection, SelectionSet};

use crate::config::Endpoint;
use crate::plan::{Kind, Plan, Variables};
use crate::schema::Loaded;
use crate::suggest::{backticked, nearest};

/// The file extensions read as operations.
const EXTENSIONS: [&str; 2] = ["graphql", "gql"];

/// How many near names to offer for an unknown operation name.
const SUGGESTIONS: usize = 5;

/// The operations of one endpoint. Empty when the endpoint names no `operations` directory.
#[derive(Debug, Default)]
pub(crate) struct Library {
    /// The directory the operations were read from.
    pub(crate) source: Option<PathBuf>,
    /// The operations, in name order. Names are unique.
    pub(crate) operations: Vec<Saved>,
}

/// One operation from the library.
#[derive(Debug)]
pub(crate) struct Saved {
    /// The operation's name, which is how `execute` refers to it.
    pub(crate) name: String,
    /// Whether it is a query or a mutation.
    pub(crate) kind: Kind,
    /// The comment at the top of the file, on one line.
    pub(crate) description: Option<String>,
    /// The variables it declares, as written in GraphQL, such as `$id: ID!`.
    pub(crate) variables: Vec<String>,
    /// The file's text, as written.
    pub(crate) text: String,
    /// The file it was read from.
    pub(crate) file: PathBuf,
}

impl Library {
    /// Reads every `.graphql` and `.gql` file directly in `directory`.
    ///
    /// # Errors
    ///
    /// Returns a message naming the directory or the file when the directory cannot be read, a file does not hold
    /// exactly one named query or mutation, or two files use the same operation name.
    pub(crate) fn load(directory: &Path) -> Result<Self, String> {
        let entries = std::fs::read_dir(directory).map_err(|error| {
            format!(
                "`operations` names {}, which is not a directory that can be read ({error}). Create it, or remove \
                 `operations`.",
                directory.display()
            )
        })?;
        let mut files: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_file()
                    && path
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| EXTENSIONS.contains(&extension))
            })
            .collect();
        files.sort();

        let mut operations: Vec<Saved> = Vec::with_capacity(files.len());
        for file in files {
            let saved = Saved::read(&file)?;
            if let Some(earlier) = operations.iter().find(|earlier| earlier.name == saved.name) {
                return Err(format!(
                    "the operation name `{}` is used in both {} and {}. Each saved operation needs its own name.",
                    saved.name,
                    earlier.file.display(),
                    file.display()
                ));
            }
            operations.push(saved);
        }
        operations.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(Self {
            source: Some(directory.to_path_buf()),
            operations,
        })
    }

    /// Whether the endpoint has no saved operations.
    pub(crate) const fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }

    /// The operation called `name`.
    ///
    /// # Errors
    ///
    /// Returns a message for the calling agent when there is none, with the nearest names.
    pub(crate) fn get(&self, endpoint: &str, name: &str) -> Result<&Saved, String> {
        if let Some(saved) = self.operations.iter().find(|saved| saved.name == name) {
            return Ok(saved);
        }
        if self.operations.is_empty() {
            return Err(format!(
                "`{endpoint}` has no saved operations, so there is none called `{name}`. Pass the GraphQL text as \
                 `operation` instead."
            ));
        }
        let names = self.operations.iter().map(|saved| saved.name.as_str());
        let near = nearest(name, names, SUGGESTIONS);
        let mut out = format!("`{endpoint}` has no saved operation called `{name}`. Names are case-sensitive.");
        if !near.is_empty() {
            let _ = write!(out, " Nearest names: {}.", backticked(near));
        }
        out.push_str(" `list_operations` lists them all.");
        Err(out)
    }

    /// The operations whose name or description holds every word of `query`, ignoring case. Every operation when
    /// `query` has no words.
    pub(crate) fn matching(&self, query: &str) -> Vec<&Saved> {
        let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        self.operations
            .iter()
            .filter(|saved| {
                let name = saved.name.to_lowercase();
                let description = saved.description.as_deref().unwrap_or_default().to_lowercase();
                terms
                    .iter()
                    .all(|term| name.contains(term.as_str()) || description.contains(term.as_str()))
            })
            .collect()
    }
}

impl Saved {
    /// Reads one operation file.
    fn read(file: &Path) -> Result<Self, String> {
        let problem = |what: &str| format!("the saved operation {} {what}", file.display());
        let text = std::fs::read_to_string(file).map_err(|error| problem(&format!("could not be read: {error}")))?;
        let document = Document::parse(&text, file).map_err(|invalid| {
            let first = invalid.errors.iter().next().map(|diagnostic| {
                let position = diagnostic.line_column_range().map_or_else(String::new, |range| {
                    format!(" at {}:{}", range.start.line, range.start.column)
                });
                format!("{}{position}", diagnostic.error)
            });
            problem(&format!("is not valid GraphQL: {}", first.unwrap_or_default()))
        })?;
        let mut operations = document.definitions.iter().filter_map(|definition| match definition {
            Definition::OperationDefinition(operation) => Some(operation),
            _ => None,
        });
        let operation = match (operations.next(), operations.next()) {
            (Some(operation), None) => operation,
            (None, _) => return Err(problem("holds no query or mutation. A file holds exactly one.")),
            (Some(_), Some(_)) => {
                return Err(problem(
                    "holds more than one operation. Put each in its own file, with the fragments it uses.",
                ));
            }
        };
        let kind = match operation.operation_type {
            OperationType::Query => Kind::Query,
            OperationType::Mutation => Kind::Mutation,
            OperationType::Subscription => return Err(problem("is a subscription, which fieldtrip does not support")),
        };
        let Some(name) = &operation.name else {
            return Err(problem(
                "has no name. Name it, as in `query OrderById($id: ID!) { ... }`: the name is how `execute` refers \
                 to it.",
            ));
        };
        Ok(Self {
            name: name.to_string(),
            kind,
            description: description(&text),
            variables: operation.variables.iter().map(ToString::to_string).collect(),
            text,
            file: file.to_path_buf(),
        })
    }
}

/// The comment lines at the top of `text`, joined on one line. Blank lines before them are skipped, and the first
/// line that is not a comment ends them.
fn description(text: &str) -> Option<String> {
    let words: Vec<&str> = text
        .lines()
        .map(str::trim)
        .skip_while(|line| line.is_empty())
        .map_while(|line| line.strip_prefix('#'))
        .flat_map(str::split_whitespace)
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

impl Saved {
    /// Validates the operation against `loaded` and estimates its cost with no variables given. `name` is the
    /// endpoint's name.
    ///
    /// # Errors
    ///
    /// Returns the validation errors when the operation no longer fits the schema.
    pub(crate) fn plan(&self, loaded: &Loaded, name: &str, endpoint: &Endpoint) -> Result<Plan, String> {
        Plan::new(loaded, name, endpoint, &self.text, &Variables::new())
    }

    /// The message for a call that named this operation when it no longer validates for `endpoint`. `error` is
    /// what planning it said.
    pub(crate) fn broken(&self, endpoint: &str, error: &str) -> String {
        let mut out = format!(
            "The saved operation `{}` ({}) no longer validates against the schema of `{endpoint}`, so it cannot be \
             run. The API has probably changed since it was saved. Tell the user, who can fix the file. Meanwhile you \
             can write the operation yourself, checking it with `estimate_cost`. What is wrong:",
            self.name,
            self.file.display()
        );
        for problem in problems(error) {
            let _ = write!(out, "\n- {problem}");
        }
        out
    }
}

/// The problems in an error from [`Plan::new`], without the advice around them.
pub(crate) fn problems(error: &str) -> Vec<&str> {
    let listed: Vec<&str> = error.lines().filter_map(|line| line.strip_prefix("- ")).collect();
    if listed.is_empty() { vec![error.trim()] } else { listed }
}

/// A filter on what an operation selects: a type, or a chain of fields written as in a cost map.
///
/// `Order` matches an operation that selects a field of `Order` or a field that returns one. `User.orders` matches
/// one that selects `orders` on a `User`, and `Query.user.orders` only that exact path from the root. `*` stands for
/// any one name. Fragments are followed.
#[derive(Debug)]
pub(crate) struct Selector {
    /// The type the chain starts on.
    anchor: Part,
    /// The field names, outermost first. Empty for a bare type.
    fields: Vec<Part>,
}

/// One part of a [`Selector`].
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
            return Some(Self::Any);
        }
        let mut characters = text.chars();
        let is_name = characters
            .next()
            .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
            && characters.all(|character| character == '_' || character.is_ascii_alphanumeric());
        is_name.then(|| Self::Name(text.to_owned()))
    }

    /// Whether this part stands for `name`.
    fn is(&self, name: &str) -> bool {
        match self {
            Self::Name(own) => own == name,
            Self::Any => true,
        }
    }
}

/// One field on the way from the root of an operation down to a selected field.
#[derive(Debug, Clone, Copy)]
struct Step<'doc> {
    /// The type the field is selected on.
    parent: &'doc str,
    /// The field's name.
    field: &'doc str,
    /// The named type the field returns, without list or non-null wrappers.
    returns: &'doc str,
}

impl Selector {
    /// Reads `text` and checks its names against `loaded`.
    ///
    /// # Errors
    ///
    /// Returns a message for the calling agent when `text` is not a pattern, or names a type or a first field that
    /// the schema does not have, with the nearest names.
    pub(crate) fn parse(text: &str, loaded: &Loaded) -> Result<Self, String> {
        let text = text.trim();
        let not_a_pattern = || {
            format!(
                "`{text}` in `uses` is not a pattern. Give a type name, such as `Order`, or a type followed by field \
                 names with dots between them, such as `Order.payment` or `Query.order.lineItems`. `*` stands for \
                 any one name."
            )
        };
        let mut parts = text.split('.').map(Part::parse);
        let anchor = parts.next().flatten().ok_or_else(not_a_pattern)?;
        let fields: Vec<Part> = parts.collect::<Option<_>>().ok_or_else(not_a_pattern)?;
        if anchor == Part::Any && fields.is_empty() {
            return Err(not_a_pattern());
        }
        if let Part::Name(type_name) = &anchor {
            if !loaded.has_type(type_name) {
                let near = nearest(type_name, loaded.type_names(), SUGGESTIONS);
                let mut out =
                    format!("`{text}` in `uses` names the type `{type_name}`, which the schema does not have.");
                if !near.is_empty() {
                    let _ = write!(out, " Nearest types: {}.", backticked(near));
                }
                return Err(out);
            }
            if let Some(Part::Name(field)) = fields.first() {
                let names = loaded.field_names(type_name);
                if !names.contains(&field.as_str()) {
                    let near = nearest(field, names, SUGGESTIONS);
                    let mut out = format!("`{text}` in `uses`: the type `{type_name}` has no field `{field}`.");
                    if !near.is_empty() {
                        let _ = write!(out, " Nearest fields: {}.", backticked(near));
                    }
                    return Err(out);
                }
            }
        }
        Ok(Self { anchor, fields })
    }

    /// Whether the field at the end of `path` is one this selector picks out.
    fn matches(&self, path: &[Step<'_>]) -> bool {
        let Some(last) = path.last() else { return false };
        if self.fields.is_empty() {
            return self.anchor.is(last.parent) || self.anchor.is(last.returns);
        }
        let Some(chain) = path
            .len()
            .checked_sub(self.fields.len())
            .and_then(|start| path.get(start..))
        else {
            return false;
        };
        chain.first().is_some_and(|step| self.anchor.is(step.parent))
            && chain.iter().zip(&self.fields).all(|(step, field)| field.is(step.field))
    }
}

impl Saved {
    /// Whether the operation matches every one of `selectors` against `loaded`. An operation that no longer
    /// validates matches none, since its fields cannot be placed in the schema.
    pub(crate) fn uses(&self, loaded: &Loaded, selectors: &[Selector]) -> bool {
        if selectors.is_empty() {
            return true;
        }
        let Ok(document) = ExecutableDocument::parse_and_validate(loaded.schema(), self.text.as_str(), &self.file)
        else {
            return false;
        };
        let Some(operation) = document.operations.iter().next() else {
            return false;
        };
        let mut found = vec![false; selectors.len()];
        walk(&document, &operation.selection_set, &mut Vec::new(), &mut |path| {
            for (found, selector) in found.iter_mut().zip(selectors) {
                *found = *found || selector.matches(path);
            }
        });
        found.into_iter().all(|found| found)
    }
}

/// Calls `visit` with the path to every field selected under `set`, following fragments. Introspection fields such
/// as `__typename` are left out.
fn walk<'doc>(
    document: &'doc ExecutableDocument,
    set: &'doc SelectionSet,
    path: &mut Vec<Step<'doc>>,
    visit: &mut dyn FnMut(&[Step<'doc>]),
) {
    for selection in &set.selections {
        match selection {
            Selection::Field(field) => {
                if field.name.starts_with("__") {
                    continue;
                }
                path.push(Step {
                    parent: set.ty.as_str(),
                    field: field.name.as_str(),
                    returns: field.definition.ty.inner_named_type().as_str(),
                });
                visit(path);
                walk(document, &field.selection_set, path, visit);
                path.pop();
            }
            Selection::InlineFragment(inline) => walk(document, &inline.selection_set, path, visit),
            Selection::FragmentSpread(spread) => {
                if let Some(fragment) = spread.fragment_def(document) {
                    walk(document, &fragment.selection_set, path, visit);
                }
            }
        }
    }
}
