//! What `list_operations` and `search_schema` show of an endpoint's saved operations.

use std::fmt::Write as _;

use crate::config::{Endpoint, Mutations};
use crate::library::{self, Saved};
use crate::plan::Kind;
use crate::schema::Loaded;
use crate::suggest::backticked;
use crate::text::excerpt;

/// How many operations `list_operations` shows in full. Past this, each gets one line.
const FULL_LISTING: usize = 5;

/// How many operation names a `list_operations` that matched nothing offers instead.
const NAMES_OFFERED: usize = 20;

/// One saved operation on one line: name, kind, cost or `BROKEN`, variables, the glossary terms it was found
/// `through` and the start of its description.
pub(crate) fn summary(schema: &Loaded, name: &str, endpoint: &Endpoint, saved: &Saved, through: &[String]) -> String {
    let mut line = match saved.plan(schema, name, endpoint) {
        Ok(plan) => format!("{} ({}, cost {}", saved.name, saved.kind.keyword(), plan.cost),
        Err(_) => format!("{} ({}, BROKEN: no longer valid", saved.name, saved.kind.keyword()),
    };
    if !saved.variables.is_empty() {
        let _ = write!(line, ", {}", saved.variables.join(", "));
    }
    if !through.is_empty() {
        let _ = write!(line, "; glossary: {}", through.join("; "));
    }
    line.push(')');
    if let Some(description) = &saved.description {
        let mut short = excerpt(description, DESCRIPTION_CHARS);
        if short.len() < description.len() {
            short.push('…');
        }
        let _ = write!(line, "  \"{short}\"");
    }
    line
}

/// How much of a description [`summary`] shows.
const DESCRIPTION_CHARS: usize = 100;

/// The filters of a `list_operations` call, checked against the schema.
#[derive(Debug)]
pub(crate) struct Filters<'call> {
    query: &'call str,
    selectors: Vec<library::Selector>,
    uses: &'call [String],
    kind: Option<Kind>,
}

impl<'call> Filters<'call> {
    /// Reads the `query`, `uses` and `kind` of a call, and checks `uses` against `schema`.
    ///
    /// # Errors
    ///
    /// Returns what to write instead when `kind` is not a kind or a `uses` is not a type or a chain of fields.
    pub(crate) fn new(
        query: Option<&'call str>,
        uses: &'call [String],
        kind: Option<&str>,
        schema: &Loaded,
    ) -> Result<Self, String> {
        let kind = match kind.map(str::trim) {
            None | Some("") => None,
            Some("query") => Some(Kind::Query),
            Some("mutation") => Some(Kind::Mutation),
            Some(other) => return Err(format!("`kind` is `query` or `mutation`, not `{other}`.")),
        };
        let selectors = uses
            .iter()
            .map(|text| library::Selector::parse(text, schema))
            .collect::<Result<_, String>>()?;
        Ok(Self {
            query: query.unwrap_or_default().trim(),
            selectors,
            uses,
            kind,
        })
    }

    /// Whether no filter was given.
    const fn are_empty(&self) -> bool {
        self.query.is_empty() && self.uses.is_empty() && self.kind.is_none()
    }

    /// The filters in words, such as `"order", using `Order.payment`, mutations only`.
    fn described(&self) -> String {
        let mut parts = Vec::new();
        if !self.query.is_empty() {
            parts.push(format!("\"{}\"", self.query));
        }
        if !self.uses.is_empty() {
            parts.push(format!("using {}", backticked(self.uses.iter().map(String::as_str))));
        }
        if let Some(kind) = self.kind {
            parts.push(format!("{}s only", kind.keyword()));
        }
        parts.join(", ")
    }

    /// The operations of the `endpoint` that pass every filter, each with the glossary terms it was found through.
    fn apply<'library>(&self, endpoint: &'library Endpoint, schema: &Loaded) -> Vec<(&'library Saved, Vec<String>)> {
        endpoint
            .operations
            .matching_with(self.query, &endpoint.glossary, schema)
            .into_iter()
            .filter(|(saved, _)| self.kind.is_none_or(|kind| saved.kind == kind))
            .filter(|(saved, _)| saved.uses(schema, &self.selectors))
            .collect()
    }
}

/// The result of `list_operations` for the operations of `endpoint` that pass `filters`, followed by the files left
/// out of the library: in full with no filter, and as a count with one.
pub(crate) fn list(schema: &Loaded, name: &str, endpoint: &Endpoint, filters: &Filters<'_>) -> String {
    let mut out = matching_operations(schema, name, endpoint, filters);
    let library = &endpoint.operations;
    let operations = library.operation_files_left_out();
    let fragments = library.fragment_files_left_out();
    if operations + fragments == 0 {
        return out;
    }
    if filters.are_empty() {
        out.push_str(
            "\nLeft out because something is wrong with them, so `execute` cannot run them. Tell the user, who can \
             fix these files:\n",
        );
        for left_out in library.rejected.iter().chain(library.left_out_fragments()) {
            let _ = writeln!(out, "- {}", left_out.problem);
        }
        return out;
    }
    let plural = |count: usize, what: &str| format!("{count} {what} file{}", if count == 1 { "" } else { "s" });
    let counted: Vec<String> = [(operations, "operation"), (fragments, "shared fragment")]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, what)| plural(count, what))
        .collect();
    let _ = writeln!(
        out,
        "\n{} {} left out, because something is wrong with them. `list_operations` with no filter lists them.",
        counted.join(" and "),
        if operations + fragments == 1 { "was" } else { "were" }
    );
    out
}

/// The part of [`list`] about the operations that pass `filters`.
fn matching_operations(schema: &Loaded, name: &str, endpoint: &Endpoint, filters: &Filters<'_>) -> String {
    let library = &endpoint.operations;
    let total = library.operations.len();
    let matching = filters.apply(endpoint, schema);
    let mut out = if filters.are_empty() {
        let source = library.source.as_ref().map(|source| source.display().to_string());
        format!(
            "{total} saved operation{} for `{name}`, from {}.",
            if total == 1 { "" } else { "s" },
            source.unwrap_or_default()
        )
    } else if matching.is_empty() {
        let mut out = format!("No saved operation for `{name}` matches {}.", filters.described());
        if !filters.uses.is_empty() {
            out.push_str(" An operation marked broken never matches `uses`.");
        }
        if total <= NAMES_OFFERED {
            let names = library.operations.iter().map(|saved| saved.name.as_str());
            let _ = write!(out, " Its saved operations: {}.", backticked(names));
        } else {
            let _ = write!(out, " It has {total}: loosen a filter.");
        }
        out.push('\n');
        return out;
    } else {
        format!(
            "{} of {total} saved operations for `{name}` match {}.",
            matching.len(),
            filters.described()
        )
    };
    out.push_str(" Run one with `execute` and `operation_name`, passing the variables it declares.\n");
    if matching.len() <= FULL_LISTING {
        for (saved, through) in matching {
            out.push('\n');
            one(&mut out, schema, name, endpoint, saved, &through);
        }
        return out;
    }
    let _ = writeln!(
        out,
        "More than {FULL_LISTING} match, so each is on one line, without its text. Narrow the list with `query`, \
         `uses` or `kind`, or pass `name` to see one in full.\n"
    );
    for (saved, through) in matching {
        let _ = writeln!(out, "{}", summary(schema, name, endpoint, saved, &through));
    }
    out
}

/// Writes one saved operation for `list_operations`, with the glossary terms it was found `through`.
pub(crate) fn one(
    out: &mut String,
    schema: &Loaded,
    name: &str,
    endpoint: &Endpoint,
    saved: &Saved,
    through: &[String],
) {
    let _ = writeln!(out, "{} ({})", saved.name, saved.kind.keyword());
    if !through.is_empty() {
        let _ = writeln!(out, "  glossary: {}", through.join("; "));
    }
    if let Some(description) = &saved.description {
        let _ = writeln!(out, "  description: {description}");
    }
    let variables = if saved.variables.is_empty() {
        "none".to_owned()
    } else {
        saved.variables.join(", ")
    };
    let _ = writeln!(out, "  variables: {variables}");
    match saved.plan(schema, name, endpoint) {
        Ok(plan) => {
            let note = if plan.assumptions.is_empty() {
                ""
            } else {
                ", with sizes assumed: `estimate_cost` with `operation_name` and your variables gives a closer figure"
            };
            let _ = writeln!(out, "  estimated cost: {}{note}", plan.cost);
            if saved.kind == Kind::Mutation && endpoint.mutations == Mutations::Deny {
                out.push_str("  `execute` refuses it here: the user's config denies mutations for this endpoint.\n");
            }
            for line in plan.text.lines() {
                let _ = writeln!(out, "  {line}");
            }
        }
        Err(error) => {
            let _ = writeln!(
                out,
                "  BROKEN: it no longer validates against the schema, so `execute` will not run it. Tell the user, \
                 who can fix {}{}.",
                saved.file.display(),
                saved.with_fragments()
            );
            for problem in error.problems() {
                let _ = writeln!(out, "  - {problem}");
            }
            if let Some(hint) = error.directive_hint() {
                let _ = writeln!(out, "  {hint}");
            }
        }
    }
}
