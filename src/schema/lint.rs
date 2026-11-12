//! Checking a cost map against the schema: which of its rules can never match.
//!
//! A cost map is written by hand, or by an agent reading server code. A misspelled type or field makes a rule
//! that silently does nothing, so `introspect` reports those.

use std::fmt::Write as _;

use apollo_compiler::Name;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::schema::ExtendedType;

use super::Loaded;
use crate::costs::{CostMap, Shape};
use crate::suggest::backticked;

/// What `introspect` says about the endpoint's cost map. Empty when the endpoint has none.
pub(super) fn report(loaded: &Loaded, costs: &CostMap) -> String {
    let Some(source) = &costs.source else {
        return String::new();
    };
    let count = costs.len();
    let plural = if count == 1 { "" } else { "s" };
    let mut out = format!("\nCost map: {count} rule{plural} from {}.", source.display());

    let mut unmatched = Vec::new();
    let mut wrong_argument = Vec::new();
    for shape in costs.shapes() {
        let fields = reached(loaded, &shape);
        if fields.is_empty() {
            unmatched.push(shape.text);
        } else if let Some(argument) = shape.argument
            && !fields.iter().any(|field| field.argument_by_name(argument).is_some())
        {
            wrong_argument.push((shape.text, argument));
        }
    }
    if unmatched.is_empty() && wrong_argument.is_empty() {
        out.push_str(" Every rule matches something in this schema.\n");
        return out;
    }
    out.push('\n');
    if !unmatched.is_empty() {
        let _ = writeln!(
            out,
            "These rules match nothing in this schema: {}.",
            backticked(unmatched)
        );
    }
    for (rule, argument) in wrong_argument {
        let _ = writeln!(
            out,
            "`{rule}` names the argument `{argument}`, which no field it matches has."
        );
    }
    out
}

/// The fields a pattern can land on in this schema. Empty when nothing in the schema fits it.
fn reached<'schema>(loaded: &'schema Loaded, shape: &Shape<'_>) -> Vec<&'schema FieldDefinition> {
    let all_types = || loaded.schema.types.keys().map(Name::as_str).collect();
    let only = |anchor: &str| {
        loaded
            .schema
            .types
            .get_key_value(anchor)
            .map(|(name, _)| name.as_str())
            .into_iter()
            .collect()
    };
    let mut types: Vec<&'schema str> = shape.anchor.map_or_else(all_types, only);
    let mut landed: Vec<&'schema FieldDefinition> = Vec::new();
    for (index, part) in shape.fields.iter().enumerate() {
        landed = types
            .iter()
            .flat_map(|ty| fields_of(loaded, ty))
            .filter(|field| part.is_none_or(|name| field.name == name))
            .collect();
        if landed.is_empty() {
            return landed;
        }
        if index + 1 < shape.fields.len() {
            types = landed
                .iter()
                .flat_map(|field| selectable_under(loaded, field.ty.inner_named_type()))
                .collect();
            types.sort_unstable();
            types.dedup();
        }
    }
    landed
}

/// The fields of the object or interface called `ty`. Nothing for any other kind of type.
fn fields_of<'schema>(loaded: &'schema Loaded, ty: &str) -> Vec<&'schema FieldDefinition> {
    match loaded.schema.types.get(ty) {
        Some(ExtendedType::Object(object)) => object.fields.values().map(|field| &***field).collect(),
        Some(ExtendedType::Interface(interface)) => interface.fields.values().map(|field| &***field).collect(),
        _ => Vec::new(),
    }
}

/// The types whose fields can be selected under a field that returns `ty`: the type itself, and the types a
/// fragment can narrow or widen it to.
fn selectable_under<'schema>(loaded: &'schema Loaded, ty: &'schema Name) -> Vec<&'schema str> {
    let mut types = vec![ty.as_str()];
    match loaded.schema.types.get(ty) {
        Some(ExtendedType::Union(union)) => types.extend(union.members.iter().map(|member| member.name.as_str())),
        Some(ExtendedType::Interface(_)) => {
            if let Some(implementers) = loaded.implementers.get(ty) {
                types.extend(
                    implementers
                        .objects
                        .iter()
                        .chain(&implementers.interfaces)
                        .map(Name::as_str),
                );
            }
        }
        Some(ExtendedType::Object(object)) => {
            types.extend(
                object
                    .implements_interfaces
                    .iter()
                    .map(|interface| interface.name.as_str()),
            );
        }
        _ => {}
    }
    types
}
