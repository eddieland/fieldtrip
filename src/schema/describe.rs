//! `describe`: the definition of one type or one field.

use std::fmt::Write as _;

use apollo_compiler::Name;
use apollo_compiler::ast::Type;
use apollo_compiler::schema::ExtendedType;

use super::{Loaded, render};
use crate::suggest::{backticked, nearest};

/// How many near matches to offer when a name is unknown.
const SUGGESTIONS: usize = 5;

/// Returns compact SDL for `name`, which is a type name or `Type.field`.
pub(super) fn describe(loaded: &Loaded, name: &str) -> Result<String, String> {
    let name = name.trim();
    match name.split_once('.') {
        None => {
            let ty = lookup_type(loaded, name)?;
            let mut out = String::new();
            render::type_definition(&mut out, ty, loaded.implementers.get(name));
            Ok(out)
        }
        Some((type_name, field_name)) => describe_field(loaded, type_name.trim(), field_name.trim()),
    }
}

/// Finds a type by name, or explains what is close to it.
fn lookup_type<'schema>(loaded: &'schema Loaded, name: &str) -> Result<&'schema ExtendedType, String> {
    if let Some(ty) = loaded.schema.types.get(name) {
        return Ok(ty);
    }
    let mut message = format!("There is no type named `{name}`.");
    let fields = root_fields_named(loaded, name);
    if !fields.is_empty() {
        let _ = write!(
            message,
            " It is a field. Describe it as {}.",
            backticked(fields.iter().map(String::as_str))
        );
    }
    let types = nearest(name, type_names(loaded), SUGGESTIONS);
    if !types.is_empty() {
        let _ = write!(message, " Nearest types: {}.", backticked(types));
    }
    Err(message)
}

/// Describes one field: the field inside its parent type, then the types needed to call it and read its result.
fn describe_field(loaded: &Loaded, type_name: &str, field_name: &str) -> Result<String, String> {
    let ty = lookup_type(loaded, type_name)?;
    let mut out = String::new();
    let mut related: Vec<&Type> = Vec::new();
    match ty {
        ExtendedType::Object(object) => {
            let field = object
                .fields
                .get(field_name)
                .ok_or_else(|| no_such_field(type_name, field_name, object.fields.keys().map(Name::as_str)))?;
            let _ = writeln!(out, "type {type_name} {{");
            render::field_line(&mut out, field, true);
            related.extend(field.arguments.iter().map(|argument| &*argument.ty));
            related.push(&field.ty);
        }
        ExtendedType::Interface(interface) => {
            let field = interface
                .fields
                .get(field_name)
                .ok_or_else(|| no_such_field(type_name, field_name, interface.fields.keys().map(Name::as_str)))?;
            let _ = writeln!(out, "interface {type_name} {{");
            render::field_line(&mut out, field, true);
            related.extend(field.arguments.iter().map(|argument| &*argument.ty));
            related.push(&field.ty);
        }
        ExtendedType::InputObject(input) => {
            let field = input
                .fields
                .get(field_name)
                .ok_or_else(|| no_such_field(type_name, field_name, input.fields.keys().map(Name::as_str)))?;
            let _ = writeln!(out, "input {type_name} {{");
            render::input_field_line(&mut out, field);
            related.push(&field.ty);
        }
        ExtendedType::Scalar(_) | ExtendedType::Union(_) | ExtendedType::Enum(_) => {
            return Err(format!("`{type_name}` has no fields. Describe it as `{type_name}`."));
        }
    }
    out.push_str("}\n");

    let mut seen: Vec<&str> = Vec::new();
    for ty in related {
        let name = ty.inner_named_type().as_str();
        let Some(definition) = loaded.schema.types.get(name) else {
            continue;
        };
        if definition.is_built_in() || seen.contains(&name) {
            continue;
        }
        seen.push(name);
        out.push('\n');
        render::type_definition(&mut out, definition, loaded.implementers.get(name));
    }
    Ok(out)
}

/// The error for a field that its type does not have.
fn no_such_field<'a>(type_name: &str, field_name: &str, fields: impl Iterator<Item = &'a str>) -> String {
    let fields: Vec<&str> = fields.collect();
    let close = nearest(field_name, fields.iter().copied(), SUGGESTIONS);
    if close.is_empty() {
        format!("`{type_name}` has no field `{field_name}`. Describe `{type_name}` to see its fields.")
    } else {
        format!(
            "`{type_name}` has no field `{field_name}`. Nearest fields: {}.",
            backticked(close)
        )
    }
}

/// Every type name a caller could mean, leaving out the introspection types.
fn type_names(loaded: &Loaded) -> impl Iterator<Item = &str> {
    loaded
        .schema
        .types
        .keys()
        .map(Name::as_str)
        .filter(|name| !name.starts_with("__"))
}

/// `Query.name`, `Mutation.name` and so on, for each root type that has a field called `name`.
fn root_fields_named(loaded: &Loaded, name: &str) -> Vec<String> {
    let definition = &loaded.schema.schema_definition;
    [&definition.query, &definition.mutation, &definition.subscription]
        .into_iter()
        .flatten()
        .filter(|root| {
            loaded
                .schema
                .get_object(root.name.as_str())
                .is_some_and(|object| object.fields.contains_key(name))
        })
        .map(|root| format!("{}.{name}", root.name))
        .collect()
}
