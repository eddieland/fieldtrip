//! What refers to a type: the unions it belongs to and the fields, arguments and input fields that use it.

use std::fmt::Write as _;

use apollo_compiler::Name;
use apollo_compiler::ast::{FieldDefinition, Type};
use apollo_compiler::collections::IndexMap;
use apollo_compiler::schema::{Component, ExtendedType};

use super::Loaded;

/// The fields of an object or an interface.
type Fields = IndexMap<Name, Component<FieldDefinition>>;

/// How many uses `describe` lists before counting the rest.
const MAX_USES: usize = 30;

/// Writes the lines that say what uses the type called `name`: `# member of:` for the unions it belongs to, then
/// `# used by:` for every field that returns it, argument that takes it and input field that holds it. Root types and
/// built-in types are skipped: nothing useful refers to `Query`, and everything refers to `String`.
pub(super) fn used_by(out: &mut String, loaded: &Loaded, name: &str) {
    let Some(ty) = loaded.schema.types.get(name) else {
        return;
    };
    if ty.is_built_in() || loaded.is_root(name) {
        return;
    }

    let unions: Vec<&str> = ordered_types(loaded)
        .filter_map(|(union_name, ty)| match ty {
            ExtendedType::Union(union) if union.members.contains(name) => Some(union_name),
            _ => None,
        })
        .collect();
    if !unions.is_empty() {
        let _ = writeln!(out, "# member of: {}", unions.join(", "));
    }

    let uses = uses(loaded, name);
    if uses.is_empty() {
        out.push_str("# used by: no field, argument or input field\n");
        return;
    }
    let shown = uses.len().min(MAX_USES);
    let _ = write!(out, "# used by: {}", uses.get(..shown).unwrap_or_default().join(", "));
    if uses.len() > shown {
        let _ = write!(out, ", and {} more", uses.len() - shown);
    }
    out.push('\n');
}

/// Every field, argument and input field whose type, lists and `!` aside, is `name`: `Type.field`,
/// `Type.field(argument:)` and `Input.field`, root types first.
fn uses(loaded: &Loaded, name: &str) -> Vec<String> {
    let mut uses = Vec::new();
    for (type_name, ty) in ordered_types(loaded) {
        match ty {
            ExtendedType::Object(object) => field_uses(&mut uses, type_name, &object.fields, name),
            ExtendedType::Interface(interface) => field_uses(&mut uses, type_name, &interface.fields, name),
            ExtendedType::InputObject(input) => {
                for field in input.fields.values() {
                    if names(&field.ty, name) {
                        uses.push(format!("{type_name}.{}", field.name));
                    }
                }
            }
            ExtendedType::Scalar(_) | ExtendedType::Union(_) | ExtendedType::Enum(_) => {}
        }
    }
    uses
}

/// Adds each of `fields` of `type_name` that returns `name`, and each of their arguments that takes it.
fn field_uses(uses: &mut Vec<String>, type_name: &str, fields: &Fields, name: &str) {
    for field in fields.values() {
        if names(&field.ty, name) {
            uses.push(format!("{type_name}.{}", field.name));
        }
        for argument in &field.arguments {
            if names(&argument.ty, name) {
                uses.push(format!("{type_name}.{}({}:)", field.name, argument.name));
            }
        }
    }
}

/// Whether `ty`, with its lists and `!` taken off, is the type called `name`.
fn names(ty: &Type, name: &str) -> bool {
    ty.inner_named_type().as_str() == name
}

/// The types that are not built in: the root types in the order query, mutation, subscription, then the rest by name.
/// The order of the schema is not used, because an introspected schema and a schema file can list types differently.
pub(super) fn ordered_types(loaded: &Loaded) -> impl Iterator<Item = (&str, &ExtendedType)> {
    let roots = loaded.root_names();
    let mut rest: Vec<(&str, &ExtendedType)> = loaded
        .schema
        .types
        .iter()
        .filter(|(name, ty)| !ty.is_built_in() && !roots.contains(&name.as_str()))
        .map(|(name, ty)| (name.as_str(), ty))
        .collect();
    rest.sort_unstable_by_key(|(name, _)| *name);
    roots
        .into_iter()
        .filter_map(|root| loaded.schema.types.get_key_value(root))
        .map(|(name, ty): (&Name, &ExtendedType)| (name.as_str(), ty))
        .chain(rest)
}
