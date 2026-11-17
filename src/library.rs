//! The operation library: named operations the user trusts, read from a directory of `.graphql` files.
//!
//! Each file holds one named query or mutation, plus any fragments of its own. The comment lines at the top of the
//! file are its description. A second directory, named by `fragments`, can hold fragments that many operations share:
//! each operation gets the shared fragments it spreads, and the ones they spread, appended to its text, so from then
//! on it is exactly as if its file had held them. The library is config: it is read once, at startup, and no tool can
//! change it. Whether an operation still fits the schema is checked whenever the schema loads, so an operation broken
//! by a change to the API is reported rather than refused at startup.
//!
//! A code generator can let its operations carry directives that only it reads. `client_directives` names them, and
//! every use of one is removed from the operations and the shared fragments as they are read, so what is validated,
//! shown and sent is what the generator itself would send. A variable that only such a directive used goes with it,
//! since an operation may not declare a variable it does not use.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use apollo_compiler::ExecutableDocument;
use apollo_compiler::ast::{self, Definition, Document, OperationType};
use apollo_compiler::executable::{Selection, SelectionSet};

use crate::config::Endpoint;
use crate::glossary::Glossary;
use crate::plan::{Invalid, Kind, Plan, Variables};
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
    /// The shared fragments, when `fragments` names a directory.
    pub(crate) fragments: Option<Shelf>,
    /// The operation files left out because something is wrong with them, in file order.
    pub(crate) rejected: Vec<Rejected>,
    /// The directives removed from every operation and shared fragment as it is read, in name order.
    pub(crate) client_directives: Vec<String>,
    /// The names in `client_directives` that some operation or shared fragment used.
    used_directives: BTreeSet<String>,
}

/// A file, or a name several files share, left out of the library because something is wrong with it. The rest of the
/// library loads without it, since the directory may belong to another codebase.
#[derive(Debug)]
pub(crate) struct Rejected {
    /// The file, or every file that uses a name more than one may not.
    pub(crate) files: Vec<PathBuf>,
    /// The operation's name, when it has one, so that a call naming it can say why it is missing.
    pub(crate) name: Option<String>,
    /// What is wrong, naming the file.
    pub(crate) problem: String,
}

/// The shared fragments of a library, as `fieldtrip check` reports them.
#[derive(Debug)]
pub(crate) struct Shelf {
    /// The directory the fragments were read from.
    pub(crate) source: PathBuf,
    /// Each fragment, in name order. Names are unique.
    pub(crate) fragments: Vec<Shared>,
    /// The files, or the fragments in them, left out because something is wrong with them, in file order.
    pub(crate) rejected: Vec<Rejected>,
    /// The fragment names defined more than once, with the files that define each. None of them is used.
    conflicts: BTreeMap<String, Vec<PathBuf>>,
}

/// One shared fragment.
#[derive(Debug)]
pub(crate) struct Shared {
    /// The fragment's name.
    pub(crate) name: String,
    /// The file it was read from.
    pub(crate) file: PathBuf,
    /// Whether any operation in the library uses it.
    pub(crate) used: bool,
    /// The fragment, printed from the parsed file.
    text: String,
    /// The names of the fragments it spreads, in the order they appear.
    spreads: Vec<String>,
    /// The variables it still uses, once the client directives are removed.
    variables: BTreeSet<String>,
    /// The variables that the client directives removed from it used.
    removed: Removed,
}

/// The variables that removed client directives used, each with the names of those directives.
type Removed = BTreeMap<String, BTreeSet<String>>;

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
    /// The file's text, as written, followed by each shared fragment it uses.
    pub(crate) text: String,
    /// The file it was read from.
    pub(crate) file: PathBuf,
    /// The files of the shared fragments appended to its text, each once, in the order first used.
    pub(crate) fragment_files: Vec<PathBuf>,
    /// The names of the shared fragments appended to its text.
    shared: Vec<String>,
    /// The variables the file declares that only client directives used, each with the names of those directives.
    /// They are removed along with the directives, so the operation does not declare them.
    dropped: Removed,
}

impl Library {
    /// Reads every `.graphql` and `.gql` file directly in `directory`, and the shared fragments in `fragments`,
    /// when it is given, removing every use of the directives named in `client_directives`.
    ///
    /// A file that does not hold exactly one named query or mutation is left out, as is every file that uses an
    /// operation name another file uses too, and every operation that spreads a shared fragment defined twice.
    ///
    /// # Errors
    ///
    /// Returns a message naming the directory when one cannot be read.
    pub(crate) fn load(
        directory: &Path,
        fragments: Option<&Path>,
        client_directives: &[String],
    ) -> Result<Self, String> {
        let mut used_directives = BTreeSet::new();
        let mut shelf = fragments
            .map(|fragments| Shelf::load(fragments, client_directives, &mut used_directives))
            .transpose()?;
        let mut rejected = Vec::new();
        let mut by_name: BTreeMap<String, Vec<Saved>> = BTreeMap::new();
        for file in graphql_files("operations", directory)? {
            match Saved::read(&file, shelf.as_ref(), client_directives, &mut used_directives) {
                Ok(saved) => by_name.entry(saved.name.clone()).or_default().push(saved),
                Err(left_out) => rejected.push(left_out),
            }
        }
        let mut operations = Vec::with_capacity(by_name.len());
        for (name, mut group) in by_name {
            if group.len() == 1 {
                operations.append(&mut group);
                continue;
            }
            // Running either file could run the one the user did not mean, so neither runs.
            let files: Vec<PathBuf> = group.into_iter().map(|saved| saved.file).collect();
            let problem = format!(
                "the operation name `{name}` is used in {}, so {} loaded. Each saved operation needs its own name.",
                listed(&files),
                if files.len() == 2 {
                    "neither is"
                } else {
                    "none of them is"
                }
            );
            rejected.push(Rejected {
                files,
                name: Some(name),
                problem,
            });
        }
        rejected.sort_by(|left, right| left.files.cmp(&right.files));
        if let Some(shelf) = &mut shelf {
            for shared in &mut shelf.fragments {
                shared.used = operations.iter().any(|saved| saved.shared.contains(&shared.name));
            }
        }
        Ok(Self {
            source: Some(directory.to_path_buf()),
            operations,
            fragments: shelf,
            rejected,
            client_directives: client_directives.to_vec(),
            used_directives,
        })
    }

    /// The names in `client_directives` that no operation or shared fragment uses, in name order.
    pub(crate) fn unused_client_directives(&self) -> Vec<&str> {
        self.client_directives
            .iter()
            .filter(|name| !self.used_directives.contains(*name))
            .map(String::as_str)
            .collect()
    }

    /// Whether the endpoint has no saved operations, and no files that were left out either.
    pub(crate) fn is_empty(&self) -> bool {
        self.operations.is_empty() && self.rejected.is_empty() && self.left_out_fragments().is_empty()
    }

    /// How many operation files were left out.
    pub(crate) fn operation_files_left_out(&self) -> usize {
        self.rejected.iter().map(|left_out| left_out.files.len()).sum()
    }

    /// How many shared fragment files were left out, whole or in part.
    pub(crate) fn fragment_files_left_out(&self) -> usize {
        self.left_out_fragments()
            .iter()
            .map(|left_out| left_out.files.len())
            .sum()
    }

    /// The problems in the shared fragments.
    pub(crate) fn left_out_fragments(&self) -> &[Rejected] {
        self.fragments.as_ref().map_or(&[], |shelf| shelf.rejected.as_slice())
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
        if let Some(left_out) = self
            .rejected
            .iter()
            .find(|left_out| left_out.name.as_deref() == Some(name))
        {
            return Err(format!(
                "`{endpoint}` has a saved operation called `{name}`, but it was left out, so it cannot be run: {}. Tell \
                 the user, who can fix it. Meanwhile you can write the operation yourself, checking it with \
                 `estimate_cost`.",
                left_out.problem
            ));
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
        self.operations.iter().filter(|saved| saved.holds(&terms)).collect()
    }

    /// The operations that match `query`, each with the glossary terms it was found through, written
    /// `term → Target, Target`. Those found through `glossary` come first.
    ///
    /// A term of `glossary` in `query` matches an operation that uses one of the term's targets, as `uses` would,
    /// or one whose name or description holds the term. Every other word has to be in the name or description.
    pub(crate) fn matching_with<'library>(
        &'library self,
        query: &str,
        glossary: &Glossary,
        loaded: &Loaded,
    ) -> Vec<(&'library Saved, Vec<String>)> {
        let (found, rest) = glossary.found(query);
        if found.is_empty() {
            return self
                .matching(query)
                .into_iter()
                .map(|saved| (saved, Vec::new()))
                .collect();
        }
        // A target the schema lacks is no selector, and matches nothing. `introspect` reports it.
        let selectors: Vec<Vec<(&str, Selector)>> = found
            .iter()
            .map(|term| {
                term.targets
                    .iter()
                    .filter_map(|target| Some((target.as_str(), Selector::parse(target, loaded).ok()?)))
                    .collect()
            })
            .collect();
        let mut matching: Vec<(&Saved, Vec<String>)> = self
            .operations
            .iter()
            .filter(|saved| saved.holds(&rest))
            .filter_map(|saved| {
                let mut through = Vec::new();
                for (term, selectors) in found.iter().zip(&selectors) {
                    let used: Vec<&str> = selectors
                        .iter()
                        .filter(|(_, selector)| saved.uses(loaded, std::slice::from_ref(selector)))
                        .map(|(target, _)| *target)
                        .collect();
                    if !used.is_empty() {
                        through.push(format!("{} → {}", term.text, used.join(", ")));
                    } else if !saved.holds(term.words()) {
                        return None;
                    }
                }
                Some((saved, through))
            })
            .collect();
        matching.sort_by_key(|(_, through)| through.is_empty());
        matching
    }
}

impl Saved {
    /// Whether the operation's name or description holds every one of `words`, which are in lower case.
    fn holds(&self, words: &[String]) -> bool {
        let name = self.name.to_lowercase();
        let description = self.description.as_deref().unwrap_or_default().to_lowercase();
        words
            .iter()
            .all(|word| name.contains(word.as_str()) || description.contains(word.as_str()))
    }
}

/// The `.graphql` and `.gql` files directly in `directory`, in name order. `setting` names the config key that
/// named the directory, for the error.
fn graphql_files(setting: &str, directory: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = std::fs::read_dir(directory).map_err(|error| {
        format!(
            "`{setting}` names {}, which is not a directory that can be read ({error}). Create it, or remove \
             `{setting}`.",
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
    Ok(files)
}

/// Reads and parses `file`. Errors are worded by `problem`, which is given what went wrong.
fn parse(file: &Path, problem: impl Fn(&str) -> String) -> Result<(String, Document), String> {
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
    Ok((text, document))
}

/// Adds the names of the fragments spread in `selections`, at any depth, to `out`, in the order they appear.
fn spreads(selections: &[ast::Selection], out: &mut Vec<String>) {
    for selection in selections {
        match selection {
            ast::Selection::Field(field) => spreads(&field.selection_set, out),
            ast::Selection::InlineFragment(inline) => spreads(&inline.selection_set, out),
            ast::Selection::FragmentSpread(spread) => out.push(spread.fragment_name.to_string()),
        }
    }
}

/// `files` in words: `both A and B` for two, `A, B and C` for more.
fn listed(files: &[PathBuf]) -> String {
    let shown: Vec<String> = files.iter().map(|file| file.display().to_string()).collect();
    match shown.as_slice() {
        [first, second] => format!("both {first} and {second}"),
        [rest @ .., last] if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => shown.join(""),
    }
}

impl Shelf {
    /// Reads every `.graphql` and `.gql` file directly in `directory`, each holding only fragment definitions. A
    /// file that does not is left out, and so is a fragment whose name more than one definition uses. Every use of a
    /// directive named in `client` is removed, and its name added to `used`.
    fn load(directory: &Path, client: &[String], used: &mut BTreeSet<String>) -> Result<Self, String> {
        let mut by_name: BTreeMap<String, Vec<Shared>> = BTreeMap::new();
        let mut rejected = Vec::new();
        for file in graphql_files("fragments", directory)? {
            let problem = |what: &str| format!("the shared fragment file {} {what}", file.display());
            let left_out = |problem: String| Rejected {
                files: vec![file.clone()],
                name: None,
                problem,
            };
            let mut document = match parse(&file, problem) {
                Ok((_, document)) => document,
                Err(error) => {
                    rejected.push(left_out(error));
                    continue;
                }
            };
            let only_fragments = document
                .definitions
                .iter()
                .all(|definition| matches!(definition, Definition::FragmentDefinition(_)));
            if !only_fragments {
                rejected.push(left_out(problem(
                    "holds a definition that is not a fragment, so it is left out. A file in `fragments` holds only \
                     fragment definitions. An operation goes in `operations`, one per file.",
                )));
                continue;
            }
            for definition in &mut document.definitions {
                let mut removed = Removed::new();
                strip_definition(definition, client, used, &mut removed);
                let Definition::FragmentDefinition(fragment) = &*definition else {
                    continue;
                };
                let mut spread = Vec::new();
                spreads(&fragment.selection_set, &mut spread);
                let mut variables = BTreeSet::new();
                definition_variables(definition, &mut variables);
                by_name.entry(fragment.name.to_string()).or_default().push(Shared {
                    name: fragment.name.to_string(),
                    file: file.clone(),
                    used: false,
                    text: fragment.to_string(),
                    spreads: spread,
                    variables,
                    removed,
                });
            }
        }
        let mut fragments = Vec::with_capacity(by_name.len());
        let mut conflicts = BTreeMap::new();
        for (name, mut group) in by_name {
            if group.len() == 1 {
                fragments.append(&mut group);
                continue;
            }
            let files: Vec<PathBuf> = group.into_iter().map(|shared| shared.file).collect();
            let problem = format!(
                "the fragment name `{name}` is defined in {}, so {} used, and an operation that spreads it is left \
                 out. Each shared fragment needs its own name.",
                listed(&files),
                if files.len() == 2 {
                    "neither is"
                } else {
                    "none of them is"
                }
            );
            rejected.push(Rejected {
                files: files.clone(),
                name: None,
                problem,
            });
            conflicts.insert(name, files);
        }
        rejected.sort_by(|left, right| left.files.cmp(&right.files));
        Ok(Self {
            source: directory.to_path_buf(),
            fragments,
            rejected,
            conflicts,
        })
    }

    /// The shared fragment called `name`.
    fn get(&self, name: &str) -> Option<&Shared> {
        self.fragments
            .binary_search_by(|shared| shared.name.as_str().cmp(name))
            .ok()
            .and_then(|index| self.fragments.get(index))
    }
}

impl Saved {
    /// Reads one operation file, appending the shared fragments from `shelf` that it uses and does not define. Every
    /// use of a directive named in `client` is removed, and its name added to `used`.
    fn read(
        file: &Path,
        shelf: Option<&Shelf>,
        client: &[String],
        used: &mut BTreeSet<String>,
    ) -> Result<Self, Rejected> {
        let left_out = |name: Option<&str>, problem: String| Rejected {
            files: vec![file.to_path_buf()],
            name: name.map(ToOwned::to_owned),
            problem,
        };
        Self::parse(file, shelf, client, used).map_err(|(name, problem)| left_out(name.as_deref(), problem))
    }

    /// Reads one operation file, or says what is wrong with it and, when it has one, its name.
    fn parse(
        file: &Path,
        shelf: Option<&Shelf>,
        client: &[String],
        used: &mut BTreeSet<String>,
    ) -> Result<Self, (Option<String>, String)> {
        let problem = |what: &str| (None, format!("the saved operation {} {what}", file.display()));
        let (mut text, mut document) = parse(file, |what| problem(what).1).map_err(|error| (None, error))?;
        let description = description(&text);
        let mut removed = Removed::new();
        let mut changed = strip(&mut document, client, used, &mut removed);
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
        let name = name.to_string();
        let mut pending = Vec::new();
        spreads(&operation.selection_set, &mut pending);
        let (appended, fragment_files, shared) = match shelf {
            Some(shelf) => with_shared(&document, pending, shelf).map_err(|conflict| {
                (
                    Some(name.clone()),
                    format!("the saved operation {} {conflict}", file.display()),
                )
            })?,
            None => (String::new(), Vec::new(), Vec::new()),
        };

        // A variable that only removed directives used would now be declared and unused, which is not valid, so it
        // goes too. One that was unused all along is not in `removed`, and stays to be reported.
        let mut still_used = BTreeSet::new();
        for definition in &document.definitions {
            definition_variables(definition, &mut still_used);
        }
        for shared in shared.iter().filter_map(|name| shelf.and_then(|shelf| shelf.get(name))) {
            still_used.extend(shared.variables.iter().cloned());
            for (variable, directives) in &shared.removed {
                removed
                    .entry(variable.clone())
                    .or_default()
                    .extend(directives.iter().cloned());
            }
        }
        removed.retain(|variable, _| !still_used.contains(variable));
        let mut dropped = Removed::new();
        let mut variables = Vec::new();
        for definition in &mut document.definitions {
            let Definition::OperationDefinition(operation) = definition else {
                continue;
            };
            if operation
                .variables
                .iter()
                .any(|variable| removed.contains_key(variable.name.as_str()))
            {
                operation.make_mut().variables.retain(|variable| {
                    let Some((variable, directives)) = removed.remove_entry(variable.name.as_str()) else {
                        return true;
                    };
                    dropped.insert(variable, directives);
                    false
                });
                changed = true;
            }
            variables = operation.variables.iter().map(ToString::to_string).collect();
        }
        // The text as written is kept unless something was removed, in which case it is printed from what is left.
        // Either way the canonical text is printed from the parsed document later, so comments never reach a server.
        if changed {
            text = document.to_string();
        }
        text.push_str(&appended);
        Ok(Self {
            name,
            kind,
            description,
            variables,
            text,
            file: file.to_path_buf(),
            fragment_files,
            shared,
            dropped,
        })
    }
}

/// Removes every use of a directive named in `client` from the operations and fragments in `document`. Adds each
/// name it removed to `used` and each variable those uses took to `variables`, and says whether it removed any.
fn strip(document: &mut Document, client: &[String], used: &mut BTreeSet<String>, variables: &mut Removed) -> bool {
    let mut removed = false;
    for definition in &mut document.definitions {
        removed |= strip_definition(definition, client, used, variables);
    }
    removed
}

/// Adds the variables in `value`, at any depth, to `out`.
fn value_variables(value: &ast::Value, out: &mut BTreeSet<String>) {
    match value {
        ast::Value::Variable(name) => {
            out.insert(name.to_string());
        }
        ast::Value::List(items) => items.iter().for_each(|item| value_variables(item, out)),
        ast::Value::Object(fields) => fields.iter().for_each(|(_, field)| value_variables(field, out)),
        _ => {}
    }
}

/// Adds the variables in the arguments of `directives` to `out`.
fn directive_variables(directives: &ast::DirectiveList, out: &mut BTreeSet<String>) {
    for argument in directives.iter().flat_map(|directive| &directive.arguments) {
        value_variables(&argument.value, out);
    }
}

/// Adds every variable that `selections` use, at any depth, in an argument of a field or of a directive, to `out`.
fn selection_variables(selections: &[ast::Selection], out: &mut BTreeSet<String>) {
    for selection in selections {
        match selection {
            ast::Selection::Field(field) => {
                for argument in &field.arguments {
                    value_variables(&argument.value, out);
                }
                directive_variables(&field.directives, out);
                selection_variables(&field.selection_set, out);
            }
            ast::Selection::InlineFragment(inline) => {
                directive_variables(&inline.directives, out);
                selection_variables(&inline.selection_set, out);
            }
            ast::Selection::FragmentSpread(spread) => directive_variables(&spread.directives, out),
        }
    }
}

/// Adds every variable that the operation or fragment `definition` uses to `out`.
fn definition_variables(definition: &Definition, out: &mut BTreeSet<String>) {
    match definition {
        Definition::OperationDefinition(operation) => {
            directive_variables(&operation.directives, out);
            selection_variables(&operation.selection_set, out);
        }
        Definition::FragmentDefinition(fragment) => {
            directive_variables(&fragment.directives, out);
            selection_variables(&fragment.selection_set, out);
        }
        _ => {}
    }
}

/// Removes every use of a directive named in `client` from the operation or fragment `definition`: on the definition
/// itself, on variables, and on every field, fragment spread and inline fragment. Adds each name it removed to
/// `used`, files each variable those uses took under it in `variables`, and says whether it removed any.
fn strip_definition(
    definition: &mut Definition,
    client: &[String],
    used: &mut BTreeSet<String>,
    variables: &mut Removed,
) -> bool {
    if client.is_empty() {
        return false;
    }
    let mut removed = false;
    let mut list = |directives: &mut ast::DirectiveList| {
        directives.retain(|directive| {
            let name = directive.name.as_str();
            let listed = client.iter().any(|client| client == name);
            if listed {
                used.insert(name.to_owned());
                removed = true;
                let mut taken = BTreeSet::new();
                for argument in &directive.arguments {
                    value_variables(&argument.value, &mut taken);
                }
                for variable in taken {
                    variables.entry(variable).or_default().insert(name.to_owned());
                }
            }
            !listed
        });
    };
    match definition {
        Definition::OperationDefinition(operation) => {
            let operation = operation.make_mut();
            list(&mut operation.directives);
            for variable in &mut operation.variables {
                list(&mut variable.make_mut().directives);
            }
            strip_selections(&mut operation.selection_set, &mut list);
        }
        Definition::FragmentDefinition(fragment) => {
            let fragment = fragment.make_mut();
            list(&mut fragment.directives);
            strip_selections(&mut fragment.selection_set, &mut list);
        }
        _ => {}
    }
    removed
}

/// Calls `list` on the directives of every selection in `selections`, at any depth.
fn strip_selections(selections: &mut [ast::Selection], list: &mut dyn FnMut(&mut ast::DirectiveList)) {
    for selection in selections {
        match selection {
            ast::Selection::Field(field) => {
                let field = field.make_mut();
                list(&mut field.directives);
                strip_selections(&mut field.selection_set, list);
            }
            ast::Selection::InlineFragment(inline) => {
                let inline = inline.make_mut();
                list(&mut inline.directives);
                strip_selections(&mut inline.selection_set, list);
            }
            ast::Selection::FragmentSpread(spread) => list(&mut spread.make_mut().directives),
        }
    }
}

/// The text of every fragment from `shelf` that is reached from the spreads in `pending` and not defined in
/// `document`, each once and ready to follow the document's own text, with the files they came from and their names. A fragment the
/// document defines wins over a shared one of the same name, and its own spreads are followed too. A name found
/// nowhere is left for validation to report.
///
/// Returns what is wrong, to follow the file's name, when it reaches a shared fragment that is defined twice.
fn with_shared(
    document: &Document,
    pending: Vec<String>,
    shelf: &Shelf,
) -> Result<(String, Vec<PathBuf>, Vec<String>), String> {
    let local: BTreeMap<&str, &ast::FragmentDefinition> = document
        .definitions
        .iter()
        .filter_map(|definition| match definition {
            Definition::FragmentDefinition(fragment) => Some((fragment.name.as_str(), &**fragment)),
            _ => None,
        })
        .collect();
    let mut text = String::new();
    let mut pending = VecDeque::from(pending);
    let mut seen = HashSet::new();
    let mut files = Vec::new();
    let mut names = Vec::new();
    while let Some(name) = pending.pop_front() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let mut next = Vec::new();
        if let Some(fragment) = local.get(name.as_str()) {
            spreads(&fragment.selection_set, &mut next);
        } else if let Some(shared) = shelf.get(&name) {
            text.push_str("\n\n");
            text.push_str(&shared.text);
            next.clone_from(&shared.spreads);
            if !files.contains(&shared.file) {
                files.push(shared.file.clone());
            }
            names.push(name);
        } else if let Some(defined) = shelf.conflicts.get(&name) {
            return Err(format!(
                "spreads the shared fragment `{name}`, which is defined in {}, so it is left out. Each shared \
                 fragment needs its own name.",
                listed(defined)
            ));
        }
        pending.extend(next);
    }
    Ok((text, files, names))
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
    pub(crate) fn plan(&self, loaded: &Loaded, name: &str, endpoint: &Endpoint) -> Result<Plan, Invalid> {
        Plan::new(loaded, name, endpoint, &self.text, &Variables::new())
    }

    /// The message for a call that named this operation when it no longer validates for `endpoint`. `error` is
    /// what planning it said.
    pub(crate) fn broken(&self, endpoint: &str, error: &Invalid) -> String {
        let mut out = format!(
            "The saved operation `{}` ({}{}) no longer validates against the schema of `{endpoint}`, so it cannot be \
             run. The API has probably changed since it was saved. Tell the user, who can fix the file. Meanwhile you \
             can write the operation yourself, checking it with `estimate_cost`. What is wrong:",
            self.name,
            self.file.display(),
            self.with_fragments()
        );
        for problem in error.problems() {
            let _ = write!(out, "\n- {problem}");
        }
        if let Some(hint) = error.directive_hint() {
            let _ = write!(out, "\n{hint}");
        }
        out
    }
}

impl Saved {
    /// `variables` without the ones this operation no longer declares because only client directives used them, and
    /// a note for the calling agent about each one that was passed. A generated client would pass them, so they are
    /// left out rather than refused.
    pub(crate) fn sendable(&self, variables: &Variables) -> (Variables, Option<String>) {
        let mut kept = variables.clone();
        let mut note = String::new();
        for (variable, directives) in &self.dropped {
            if kept.remove(variable).is_none() {
                continue;
            }
            let directives: Vec<String> = directives.iter().map(|directive| format!("`@{directive}`")).collect();
            if !note.is_empty() {
                note.push('\n');
            }
            let _ = write!(
                note,
                "Left out `${variable}`: the saved operation `{}` used it only in {}, which fieldtrip removes as a \
                 client directive, so the operation no longer declares it and it is not sent. `list_operations` \
                 shows the variables it takes.",
                self.name,
                directives.join(" and ")
            );
        }
        (kept, (!note.is_empty()).then_some(note))
    }

    /// `, with shared fragments from FILES` when it uses any, to follow the operation's own file in a message.
    pub(crate) fn with_fragments(&self) -> String {
        if self.fragment_files.is_empty() {
            return String::new();
        }
        let files: Vec<String> = self
            .fragment_files
            .iter()
            .map(|file| file.display().to_string())
            .collect();
        format!(", with shared fragments from {}", files.join(", "))
    }
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
