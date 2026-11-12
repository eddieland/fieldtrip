//! `search_schema`: find types, fields, arguments and enum values by name or description.

use std::fmt::Write as _;

use apollo_compiler::Name;
use apollo_compiler::ast::{DirectiveList, EnumValueDefinition, FieldDefinition, InputValueDefinition};
use apollo_compiler::schema::ExtendedType;

use super::{Loaded, render};
use crate::suggest::{backticked, nearest};

/// How many matches a search returns.
const MAX_RESULTS: usize = 20;
/// How many characters of a description a result line shows.
const MAX_DESCRIPTION: usize = 100;
/// How many near names to offer when nothing matches.
const SUGGESTIONS: usize = 5;

/// One searchable thing in a schema.
enum Item<'schema> {
    /// A named type.
    Type(&'schema ExtendedType),
    /// A field of an object or interface. `root` is set for the fields of the query and mutation types.
    Field {
        parent: &'schema Name,
        field: &'schema FieldDefinition,
        root: bool,
    },
    /// An argument of a field.
    Argument {
        parent: &'schema Name,
        field: &'schema FieldDefinition,
        argument: &'schema InputValueDefinition,
    },
    /// A field of an input object.
    InputField {
        parent: &'schema Name,
        field: &'schema InputValueDefinition,
    },
    /// A value of an enum.
    EnumValue {
        parent: &'schema Name,
        value: &'schema EnumValueDefinition,
    },
}

impl Item<'_> {
    /// The item's own name.
    fn name(&self) -> &str {
        match self {
            Self::Type(ty) => ty.name().as_str(),
            Self::Field { field, .. } => field.name.as_str(),
            Self::Argument { argument, .. } => argument.name.as_str(),
            Self::InputField { field, .. } => field.name.as_str(),
            Self::EnumValue { value, .. } => value.value.as_str(),
        }
    }

    /// The names of the things that contain the item: its type, and for an argument its field too.
    fn parents(&self) -> [Option<&str>; 2] {
        match self {
            Self::Type(_) => [None, None],
            Self::Field { parent, .. } | Self::InputField { parent, .. } | Self::EnumValue { parent, .. } => {
                [Some(parent.as_str()), None]
            }
            Self::Argument { parent, field, .. } => [Some(parent.as_str()), Some(field.name.as_str())],
        }
    }

    /// The item's description, if it has one.
    fn description(&self) -> Option<&str> {
        match self {
            Self::Type(ty) => ty.description().map(|text| &**text),
            Self::Field { field, .. } => field.description.as_deref(),
            Self::Argument { argument, .. } => argument.description.as_deref(),
            Self::InputField { field, .. } => field.description.as_deref(),
            Self::EnumValue { value, .. } => value.description.as_deref(),
        }
    }

    /// Whether the item is a query or a mutation, which is what a caller is most often looking for.
    const fn is_operation(&self) -> bool {
        matches!(self, Self::Field { root: true, .. })
    }

    /// Orders kinds of item when their scores tie: operations, then types, then members.
    const fn kind_rank(&self) -> u8 {
        match self {
            Self::Field { root: true, .. } => 0,
            Self::Type(_) => 1,
            Self::Field { .. } | Self::InputField { .. } => 2,
            Self::Argument { .. } | Self::EnumValue { .. } => 3,
        }
    }

    /// The item as a one-line signature that starts with a name `describe` accepts.
    fn line(&self) -> String {
        let mut line = match self {
            Self::Type(ty) => render::type_heading(ty),
            Self::Field { parent, field, .. } => {
                format!(
                    "{parent}.{}{}",
                    render::field_signature(field),
                    deprecated(&field.directives)
                )
            }
            Self::Argument {
                parent,
                field,
                argument,
            } => {
                format!(
                    "{parent}.{}({}){}",
                    field.name,
                    render::input_value(argument),
                    deprecated(&argument.directives)
                )
            }
            Self::InputField { parent, field } => {
                format!(
                    "{parent}.{}{}",
                    render::input_value(field),
                    deprecated(&field.directives)
                )
            }
            Self::EnumValue { parent, value } => format!("{parent}.{}{}", value.value, deprecated(&value.directives)),
        };
        if let Some(description) = self.description().filter(|text| !text.trim().is_empty()) {
            let _ = write!(line, "  {}", render::quoted(&shortened(description)));
        }
        line
    }

    /// How well the item matches `terms`, or `None` when some term does not match at all.
    ///
    /// Every term has to match the item's name, its description, or the name of something that contains it. At
    /// least one term has to match the item itself, so that `user` alone does not return every field of `User`.
    fn score(&self, terms: &[String]) -> Option<u32> {
        let name = self.name().to_lowercase();
        let description = self.description().map(str::to_lowercase);
        let parents = self.parents().map(|parent| parent.map(str::to_lowercase));
        let mut total = 0;
        let mut matched_itself = false;
        for term in terms {
            let own = if name == *term {
                100
            } else if name.starts_with(term.as_str()) {
                60
            } else if name.contains(term.as_str()) {
                40
            } else if description.as_ref().is_some_and(|text| text.contains(term.as_str())) {
                15
            } else {
                0
            };
            if own > 0 {
                matched_itself = true;
                total += own;
            } else if parents.iter().flatten().any(|parent| parent.contains(term.as_str())) {
                total += 5;
            } else {
                return None;
            }
        }
        if !matched_itself {
            return None;
        }
        Some(if self.is_operation() { total + 30 } else { total })
    }
}

/// Searches `loaded` for `query` and returns the best matches as one-line signatures.
///
/// # Errors
///
/// Returns a message when `query` has no words in it.
pub(super) fn search(loaded: &Loaded, endpoint: &str, query: &str) -> Result<String, String> {
    let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if terms.is_empty() {
        return Err("Give `query` at least one word to search for, such as part of a field or type name.".to_owned());
    }
    let shown = terms.join(" ");

    let items = items(loaded);
    let mut matches: Vec<(u32, &Item<'_>)> = items
        .iter()
        .filter_map(|item| item.score(&terms).map(|score| (score, item)))
        .collect();
    if matches.is_empty() {
        let names = items.iter().filter(|item| item.kind_rank() <= 1).map(Item::name);
        let near = nearest(&shown, names, SUGGESTIONS);
        let mut out = format!("No matches for \"{shown}\" in `{endpoint}`.");
        if !near.is_empty() {
            let _ = write!(out, " Nearest names: {}.", backticked(near));
        }
        out.push('\n');
        return Ok(out);
    }
    matches.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then_with(|| left.kind_rank().cmp(&right.kind_rank()))
            .then_with(|| left.name().len().cmp(&right.name().len()))
    });

    let count = matches.len();
    let plural = if count == 1 { "match" } else { "matches" };
    let mut out = format!("{count} {plural} for \"{shown}\" in `{endpoint}`, best first:\n");
    for (_, item) in matches.iter().take(MAX_RESULTS) {
        let _ = writeln!(out, "{}", item.line());
    }
    if count > MAX_RESULTS {
        let _ = writeln!(
            out,
            "Showing the best {MAX_RESULTS} of {count}. {} were left out. Add a word to narrow the search.",
            count - MAX_RESULTS
        );
    }
    Ok(out)
}

/// Everything searchable in the schema, in schema order.
fn items(loaded: &Loaded) -> Vec<Item<'_>> {
    let definition = &loaded.schema.schema_definition;
    let operation_roots: Vec<&str> = [&definition.query, &definition.mutation]
        .into_iter()
        .flatten()
        .map(|root| root.name.as_str())
        .collect();

    let mut items = Vec::new();
    for (name, ty) in loaded.schema.types.iter().filter(|(_, ty)| !ty.is_built_in()) {
        items.push(Item::Type(ty));
        let root = operation_roots.contains(&name.as_str());
        match ty {
            ExtendedType::Object(object) => {
                fields(&mut items, name, object.fields.values().map(|field| &***field), root);
            }
            ExtendedType::Interface(interface) => {
                fields(
                    &mut items,
                    name,
                    interface.fields.values().map(|field| &***field),
                    false,
                );
            }
            ExtendedType::InputObject(input) => {
                items.extend(
                    input
                        .fields
                        .values()
                        .map(|field| Item::InputField { parent: name, field }),
                );
            }
            ExtendedType::Enum(enumeration) => {
                items.extend(
                    enumeration
                        .values
                        .values()
                        .map(|value| Item::EnumValue { parent: name, value }),
                );
            }
            ExtendedType::Scalar(_) | ExtendedType::Union(_) => {}
        }
    }
    items
}

/// Adds each field of a type, and each argument of each field.
fn fields<'schema>(
    items: &mut Vec<Item<'schema>>,
    parent: &'schema Name,
    fields: impl Iterator<Item = &'schema FieldDefinition>,
    root: bool,
) {
    for field in fields {
        items.push(Item::Field { parent, field, root });
        items.extend(field.arguments.iter().map(|argument| Item::Argument {
            parent,
            field,
            argument,
        }));
    }
}

/// ` @deprecated`, or nothing.
fn deprecated(directives: &DirectiveList) -> &'static str {
    if directives.get("deprecated").is_some() {
        " @deprecated"
    } else {
        ""
    }
}

/// `text` on one line, cut to [`MAX_DESCRIPTION`] characters.
fn shortened(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= MAX_DESCRIPTION {
        return flat;
    }
    let mut cut: String = flat.chars().take(MAX_DESCRIPTION).collect();
    cut.push('…');
    cut
}
