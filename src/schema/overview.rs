//! `introspect`: a schema at a glance.

use std::fmt::Write as _;

use apollo_compiler::schema::{ComponentName, ExtendedType};

use super::Loaded;

/// How many root field names to list per operation type before cutting off.
const MAX_ROOT_FIELDS: usize = 300;

/// Returns the schema's description, type counts and root field names.
pub(super) fn overview(loaded: &Loaded) -> String {
    let schema = &loaded.schema;
    let mut out = String::new();
    if let Some(description) = schema
        .schema_definition
        .description
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        let _ = writeln!(out, "{}", super::render::quoted(description));
    }

    let mut counts = [0_usize; 6];
    for ty in schema.types.values().filter(|ty| !ty.is_built_in()) {
        let slot = match ty {
            ExtendedType::Object(_) => 0,
            ExtendedType::Interface(_) => 1,
            ExtendedType::Union(_) => 2,
            ExtendedType::Enum(_) => 3,
            ExtendedType::InputObject(_) => 4,
            ExtendedType::Scalar(_) => 5,
        };
        if let Some(count) = counts.get_mut(slot) {
            *count += 1;
        }
    }
    let kinds = ["object", "interface", "union", "enum", "input object", "scalar"];
    let parts: Vec<String> = counts
        .iter()
        .zip(kinds)
        .filter(|(count, _)| **count > 0)
        .map(|(count, kind)| format!("{count} {kind}{}", plural(*count)))
        .collect();
    let total: usize = counts.iter().sum();
    let _ = writeln!(out, "\n{total} type{}: {}\n", plural(total), parts.join(", "));

    let definition = &schema.schema_definition;
    root(&mut out, loaded, "Query", definition.query.as_ref());
    root(&mut out, loaded, "Mutation", definition.mutation.as_ref());
    if definition.subscription.is_some() {
        root(&mut out, loaded, "Subscription", definition.subscription.as_ref());
        out.push_str("Subscriptions can be described but not executed.\n");
    }
    out
}

/// Writes one line naming the fields of a root operation type, when the schema has that type.
fn root(out: &mut String, loaded: &Loaded, label: &str, root: Option<&ComponentName>) {
    let Some(object) = root.and_then(|root| loaded.schema.get_object(root.name.as_str())) else {
        return;
    };
    let count = object.fields.len();
    let names: Vec<String> = object
        .fields
        .values()
        .take(MAX_ROOT_FIELDS)
        .map(|field| {
            if field.directives.get("deprecated").is_some() {
                format!("{} (deprecated)", field.name)
            } else {
                field.name.to_string()
            }
        })
        .collect();
    let _ = write!(out, "{label} ({count} field{}): {}", plural(count), names.join(", "));
    if count > MAX_ROOT_FIELDS {
        let _ = write!(
            out,
            ", and {} more. Use `search_schema` to find them.",
            count - MAX_ROOT_FIELDS
        );
    }
    out.push('\n');
}

/// `"s"` unless `count` is one.
const fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}
