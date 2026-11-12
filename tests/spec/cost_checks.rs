//! Slice 20: a cost map is checked when the server starts, and against the schema when the schema is loaded.
//!
//! A map is often written by hand or by an agent reading server code, so a mistake in it has to be loud: a rule
//! that is not valid stops the server, and a rule that can never match is reported by `introspect`.

use serde_json::json;

use crate::support::{Launch, MockServer};

const CONFIG: &str = "[endpoints.store]\nurl = \"http://localhost:9/graphql\"\nschema = \"schema.graphql\"\n\
                      costs = \"store.costs.toml\"\n";

/// Starts the server with `map` as the cost map, expecting it to refuse, and returns what it wrote to stderr.
fn refused(map: &str) -> String {
    let stderr = Launch::new(CONFIG).file("store.costs.toml", map).fails_to_start();
    assert!(
        stderr.contains("store.costs.toml"),
        "the error names the cost map file: {stderr}"
    );
    assert!(stderr.contains("`store`"), "the error names the endpoint: {stderr}");
    stderr
}

/// Returns what `introspect` says about `map`: the lines from `Cost map:` on.
fn report(map: &str) -> String {
    let mock = MockServer::unreachable();
    let mut mcp = Launch::new(CONFIG).file("store.costs.toml", map).start();
    let reply = mcp.call("introspect", &json!({ "endpoint": "store" }));
    assert!(!reply.is_error, "introspect should succeed: {}", reply.text);
    assert_eq!(
        mock.requests().len(),
        0,
        "the map is checked against the schema file, with no request"
    );
    let start = reply
        .text
        .find("Cost map:")
        .unwrap_or_else(|| panic!("no cost map report in:\n{}", reply.text));
    reply.text.get(start..).unwrap_or_default().to_owned()
}

#[test]
fn a_map_whose_rules_all_match_says_so() {
    let mcp = Launch::new(CONFIG).file("store.costs.toml", "").start();
    let path = mcp.dir().join("store.costs.toml");
    drop(mcp);

    let text = report("\"Query.products\" = { multiplier = 500 }\n\"*.orders\" = { multiplier = \"first\" }\n");

    let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
    assert!(
        text.starts_with("Cost map: 2 rules from "),
        "says how many rules there are: {text}"
    );
    assert!(text.contains(file_name), "says where they came from: {text}");
    assert!(
        text.ends_with("Every rule matches something in this schema.\n"),
        "says all is well: {text}"
    );
}

#[test]
fn rules_that_name_a_type_or_field_the_schema_lacks_are_reported() {
    let text = report(
        "\"Usr.orders\" = { multiplier = 3 }\n\
         \"Query.prodcuts\" = { multiplier = 500 }\n\
         \"Query.viewer.ordrs\" = { cost = 2 }\n\
         \"User.orders\" = { multiplier = 3 }\n",
    );

    assert!(
        text.contains(
            "These rules match nothing in this schema: `Query.prodcuts`, `Query.viewer.ordrs`, `Usr.orders`.\n"
        ),
        "the three misspelled rules are listed, and the good one is not: {text}"
    );
}

#[test]
fn patterns_with_stars_are_checked_too() {
    let text = report(
        "\"*.orders\" = { multiplier = 3 }\n\
         \"Query.*.orders\" = { multiplier = 3 }\n\
         \"*.nope\" = { cost = 1 }\n\
         \"Query.*.nope\" = { cost = 1 }\n",
    );

    assert!(
        text.contains("These rules match nothing in this schema: `*.nope`, `Query.*.nope`.\n"),
        "only the patterns no field can satisfy are listed: {text}"
    );
}

#[test]
fn a_path_through_a_union_or_an_interface_counts_as_matching() {
    // `search` returns a union and `node` returns an interface. Their fields are reached through fragments.
    let text = report(
        "\"Query.search.name\" = { cost = 1 }\n\
         \"Query.node.title\" = { cost = 1 }\n\
         \"Query.search.nope\" = { cost = 1 }\n\
         \"SearchResult.name\" = { cost = 1 }\n",
    );

    assert!(
        text.contains("These rules match nothing in this schema: `Query.search.nope`, `SearchResult.name`.\n"),
        "fields of the union's members are reachable, and a union itself has no fields to select: {text}"
    );
}

#[test]
fn a_multiplier_that_names_an_argument_no_matching_field_has_is_reported() {
    let text = report(
        "\"Product.related\" = { multiplier = \"tke\" }\n\
         \"*.*\" = { multiplier = \"take\" }\n",
    );

    assert!(
        text.contains("`Product.related` names the argument `tke`, which no field it matches has.\n"),
        "the misspelled argument is reported: {text}"
    );
    assert!(
        !text.contains("`*.*` names"),
        "a pattern is fine when some field it matches has the argument: {text}"
    );
}

#[test]
fn a_rule_that_matches_nothing_does_not_get_in_the_way_of_an_estimate() {
    let mut mcp = Launch::new(CONFIG)
        .file("store.costs.toml", "\"Usr.orders\" = { multiplier = 3 }\n")
        .start();

    let reply = mcp.call(
        "estimate_cost",
        &json!({ "endpoint": "store", "operation": "{ viewer { name } }" }),
    );

    assert!(!reply.is_error, "the estimate works: {}", reply.text);
    assert!(
        reply.text.contains("Estimated cost: 1\n"),
        "with the built-in costs: {}",
        reply.text
    );
}

#[test]
fn a_map_that_is_not_toml_is_refused_with_the_line() {
    let stderr = refused("\"Query.products\" = { multiplier = 500 }\n\"User.orders\" = { multiplier = \n");

    assert!(stderr.contains("line 2"), "says where the problem is: {stderr}");
}

#[test]
fn a_rule_with_an_unknown_setting_is_refused() {
    let stderr = refused("\"Query.products\" = { weight = 500 }\n");

    assert!(stderr.contains("weight"), "names the setting: {stderr}");
    assert!(
        stderr.contains("cost") && stderr.contains("multiplier"),
        "says what the settings are: {stderr}"
    );
}

#[test]
fn a_rule_that_sets_nothing_is_refused() {
    let stderr = refused("\"Query.products\" = {}\n");

    assert!(stderr.contains("`Query.products`"), "names the rule: {stderr}");
    assert!(stderr.contains("neither"), "says what is missing: {stderr}");
}

#[test]
fn a_key_that_is_not_a_pattern_is_refused() {
    for key in [
        "orders",
        "User..orders",
        "User.orders()",
        "User.**",
        ".orders",
        "User.",
        "a b.c",
    ] {
        let stderr = refused(&format!("\"{key}\" = {{ cost = 1 }}\n"));

        assert!(stderr.contains(&format!("`{key}`")), "names the rule `{key}`: {stderr}");
        assert!(
            stderr.contains("is not a pattern"),
            "says what is wrong with `{key}`: {stderr}"
        );
        assert!(
            stderr.contains("`User.orders`"),
            "shows what a pattern looks like: {stderr}"
        );
    }
}

#[test]
fn a_multiplier_of_the_wrong_kind_is_refused() {
    for value in ["true", "1.5", "-1", "\"not a name\"", "[1]"] {
        let stderr = refused(&format!("\"Query.products\" = {{ multiplier = {value} }}\n"));

        assert!(
            stderr.contains("`Query.products`"),
            "names the rule for `{value}`: {stderr}"
        );
        assert!(
            stderr.contains("`multiplier`"),
            "names the setting for `{value}`: {stderr}"
        );
    }
}

#[test]
fn a_cost_of_the_wrong_kind_is_refused() {
    for value in ["-1", "1.5", "\"ten\"", "true"] {
        let stderr = refused(&format!("\"Query.products\" = {{ cost = {value} }}\n"));

        assert!(
            stderr.contains("`Query.products`"),
            "names the rule for `{value}`: {stderr}"
        );
        assert!(stderr.contains("`cost`"), "names the setting for `{value}`: {stderr}");
    }
}
