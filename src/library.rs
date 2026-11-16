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
//! shown and sent is what the generator itself would send.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use apollo_compiler::ExecutableDocument;
use apollo_compiler::ast::{self, Definition, Document, OperationType};
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
    /// The file's text, as written, followed by each shared fragment it uses.
    pub(crate) text: String,
    /// The file it was read from.
    pub(crate) file: PathBuf,
    /// The files of the shared fragments appended to its text, each once, in the order first used.
    pub(crate) fragment_files: Vec<PathBuf>,
    /// The names of the shared fragments appended to its text.
    shared: Vec<String>,
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
            strip(&mut document, client, used);
            let fragments = document.definitions.iter().filter_map(|definition| match definition {
                Definition::FragmentDefinition(fragment) => Some(fragment),
                _ => None,
            });
            for fragment in fragments {
                let mut spread = Vec::new();
                spreads(&fragment.selection_set, &mut spread);
                by_name.entry(fragment.name.to_string()).or_default().push(Shared {
                    name: fragment.name.to_string(),
                    file: file.clone(),
                    used: false,
                    text: fragment.to_string(),
                    spreads: spread,
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
        // The text as written is kept unless a directive was removed, in which case it is printed from what is left.
        // Either way the canonical text is printed from the parsed document later, so comments never reach a server.
        if strip(&mut document, client, used) {
            text = document.to_string();
        }
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
        let variables = operation.variables.iter().map(ToString::to_string).collect();
        let mut pending = Vec::new();
        spreads(&operation.selection_set, &mut pending);
        let (text, fragment_files, shared) = match shelf {
            Some(shelf) => with_shared(text, &document, pending, shelf).map_err(|conflict| {
                (
                    Some(name.to_string()),
                    format!("the saved operation {} {conflict}", file.display()),
                )
            })?,
            None => (text, Vec::new(), Vec::new()),
        };
        Ok(Self {
            name: name.to_string(),
            kind,
            description,
            variables,
            text,
            file: file.to_path_buf(),
            fragment_files,
            shared,
        })
    }
}

/// Removes every use of a directive named in `client` from the operations and fragments in `document`: on the
/// definitions themselves, on variables, and on every field, fragment spread and inline fragment. Adds each name it
/// removed to `used`, and says whether it removed any.
fn strip(document: &mut Document, client: &[String], used: &mut BTreeSet<String>) -> bool {
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
            }
            !listed
        });
    };
    for definition in &mut document.definitions {
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

/// `text`, the text of `document`, followed by every fragment from `shelf` that is reached from the spreads in
/// `pending` and not defined in `document`, each once, with the files they came from and their names. A fragment the
/// document defines wins over a shared one of the same name, and its own spreads are followed too. A name found
/// nowhere is left for validation to report.
///
/// Returns what is wrong, to follow the file's name, when it reaches a shared fragment that is defined twice.
fn with_shared(
    mut text: String,
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
    pub(crate) fn plan(&self, loaded: &Loaded, name: &str, endpoint: &Endpoint) -> Result<Plan, String> {
        Plan::new(loaded, name, endpoint, &self.text, &Variables::new())
    }

    /// The message for a call that named this operation when it no longer validates for `endpoint`. `error` is
    /// what planning it said.
    pub(crate) fn broken(&self, endpoint: &str, error: &str) -> String {
        let mut out = format!(
            "The saved operation `{}` ({}{}) no longer validates against the schema of `{endpoint}`, so it cannot be \
             run. The API has probably changed since it was saved. Tell the user, who can fix the file. Meanwhile you \
             can write the operation yourself, checking it with `estimate_cost`. What is wrong:",
            self.name,
            self.file.display(),
            self.with_fragments()
        );
        for problem in problems(error) {
            let _ = write!(out, "\n- {problem}");
        }
        if let Some(hint) = directive_hint(error) {
            let _ = write!(out, "\n{hint}");
        }
        out
    }
}

impl Saved {
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

/// What to tell the calling agent about an unknown directive in a saved operation, when `error` reports one. Code
/// generators let operations carry directives that only they read, and the user can list those.
pub(crate) fn directive_hint(error: &str) -> Option<&'static str> {
    error.contains("cannot find directive").then_some(
        "If a directive the schema does not have is one that only the user's code generator reads, the user can \
         name it in `client_directives` for this endpoint, and fieldtrip will remove it from saved operations before \
         they are checked and sent. A misspelled directive has to be fixed in the file.",
    )
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
