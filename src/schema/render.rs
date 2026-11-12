//! Compact SDL: one line per field, one-line descriptions, and nothing an agent does not need.

use std::fmt::Write as _;

use apollo_compiler::Name;
use apollo_compiler::ast::{DirectiveList, FieldDefinition, InputValueDefinition};
use apollo_compiler::schema::{ExtendedType, Implementers};

/// Writes the definition of `ty`. For an interface, `implementers` adds a line naming the types that implement it.
pub(super) fn type_definition(out: &mut String, ty: &ExtendedType, implementers: Option<&Implementers>) {
    description(out, "", ty.description().map(|text| &**text));
    match ty {
        ExtendedType::Scalar(scalar) => {
            let _ = writeln!(out, "scalar {}", scalar.name);
        }
        ExtendedType::Object(object) => {
            let _ = writeln!(
                out,
                "type {}{} {{",
                object.name,
                implements(object.implements_interfaces.iter())
            );
            for field in object.fields.values() {
                field_line(out, field, false);
            }
            out.push_str("}\n");
        }
        ExtendedType::Interface(interface) => {
            let _ = writeln!(
                out,
                "interface {}{} {{",
                interface.name,
                implements(interface.implements_interfaces.iter())
            );
            for field in interface.fields.values() {
                field_line(out, field, false);
            }
            out.push_str("}\n");
            if let Some(implementers) = implementers {
                let names: Vec<&str> = implementers
                    .objects
                    .iter()
                    .chain(&implementers.interfaces)
                    .map(Name::as_str)
                    .collect();
                if !names.is_empty() {
                    let _ = writeln!(out, "# implemented by: {}", names.join(", "));
                }
            }
        }
        ExtendedType::Union(union) => {
            let members: Vec<&str> = union.members.iter().map(|member| member.name.as_str()).collect();
            let _ = writeln!(out, "union {} = {}", union.name, members.join(" | "));
        }
        ExtendedType::Enum(enumeration) => {
            let _ = writeln!(out, "enum {} {{", enumeration.name);
            for value in enumeration.values.values() {
                description(out, "  ", value.description.as_deref());
                let _ = writeln!(out, "  {}{}", value.value, deprecation(&value.directives));
            }
            out.push_str("}\n");
        }
        ExtendedType::InputObject(input) => {
            let _ = writeln!(out, "input {} {{", input.name);
            for field in input.fields.values() {
                description(out, "  ", field.description.as_deref());
                let _ = writeln!(out, "  {}{}", input_value(field), deprecation(&field.directives));
            }
            out.push_str("}\n");
        }
    }
}

/// Writes one field of an object or interface, indented as a member of its type.
///
/// With `expand_arguments`, arguments that have descriptions are written one per line with those descriptions.
pub(super) fn field_line(out: &mut String, field: &FieldDefinition, expand_arguments: bool) {
    description(out, "  ", field.description.as_deref());
    let _ = write!(out, "  {}", field.name);
    let documented = field.arguments.iter().any(|argument| argument.description.is_some());
    if expand_arguments && documented {
        out.push_str("(\n");
        for argument in &field.arguments {
            description(out, "    ", argument.description.as_deref());
            let _ = writeln!(
                out,
                "    {}{}",
                input_value(argument),
                deprecation(&argument.directives)
            );
        }
        out.push_str("  )");
    } else if !field.arguments.is_empty() {
        let arguments: Vec<String> = field.arguments.iter().map(|argument| input_value(argument)).collect();
        let _ = write!(out, "({})", arguments.join(", "));
    }
    let _ = writeln!(out, ": {}{}", field.ty, deprecation(&field.directives));
}

/// The first line of a type's definition, without its members: `type User implements Node`, `enum Role`.
pub(super) fn type_heading(ty: &ExtendedType) -> String {
    match ty {
        ExtendedType::Scalar(scalar) => format!("scalar {}", scalar.name),
        ExtendedType::Object(object) => {
            format!(
                "type {}{}",
                object.name,
                implements(object.implements_interfaces.iter())
            )
        }
        ExtendedType::Interface(interface) => {
            format!(
                "interface {}{}",
                interface.name,
                implements(interface.implements_interfaces.iter())
            )
        }
        ExtendedType::Union(union) => {
            let members: Vec<&str> = union.members.iter().map(|member| member.name.as_str()).collect();
            format!("union {} = {}", union.name, members.join(" | "))
        }
        ExtendedType::Enum(enumeration) => format!("enum {}", enumeration.name),
        ExtendedType::InputObject(input) => format!("input {}", input.name),
    }
}

/// `name(arguments): Type` on one line.
pub(super) fn field_signature(field: &FieldDefinition) -> String {
    let mut out = field.name.to_string();
    if !field.arguments.is_empty() {
        let arguments: Vec<String> = field.arguments.iter().map(|argument| input_value(argument)).collect();
        let _ = write!(out, "({})", arguments.join(", "));
    }
    let _ = write!(out, ": {}", field.ty);
    out
}

/// Writes one input field of an input object, indented as a member of its type.
pub(super) fn input_field_line(out: &mut String, field: &InputValueDefinition) {
    description(out, "  ", field.description.as_deref());
    let _ = writeln!(out, "  {}{}", input_value(field), deprecation(&field.directives));
}

/// `name: Type`, with ` = default` when there is one.
pub(super) fn input_value(value: &InputValueDefinition) -> String {
    value.default_value.as_ref().map_or_else(
        || format!("{}: {}", value.name, value.ty),
        |default| format!("{}: {} = {default}", value.name, value.ty),
    )
}

/// ` implements A & B`, or nothing.
fn implements<'a>(interfaces: impl Iterator<Item = &'a apollo_compiler::schema::ComponentName>) -> String {
    let names: Vec<&str> = interfaces.map(|interface| interface.name.as_str()).collect();
    if names.is_empty() {
        String::new()
    } else {
        format!(" implements {}", names.join(" & "))
    }
}

/// ` @deprecated(reason: "...")`, ` @deprecated`, or nothing.
fn deprecation(directives: &DirectiveList) -> String {
    let Some(directive) = directives.get("deprecated") else {
        return String::new();
    };
    directive
        .specified_argument_by_name("reason")
        .and_then(|reason| reason.as_str())
        .map_or_else(
            || " @deprecated".to_owned(),
            |reason| format!(" @deprecated(reason: {})", quoted(reason)),
        )
}

/// Writes a description as a one-line string on its own line, or nothing when there is none.
fn description(out: &mut String, indent: &str, text: Option<&str>) {
    if let Some(text) = text.filter(|text| !text.trim().is_empty()) {
        let _ = writeln!(out, "{indent}{}", quoted(text));
    }
}

/// `text` as a one-line GraphQL string: whitespace runs become single spaces, quotes and backslashes are escaped.
pub(super) fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for (index, word) in text.split_whitespace().enumerate() {
        if index > 0 {
            out.push(' ');
        }
        for character in word.chars() {
            if matches!(character, '"' | '\\') {
                out.push('\\');
            }
            out.push(character);
        }
    }
    out.push('"');
    out
}
