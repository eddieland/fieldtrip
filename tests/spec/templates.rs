//! Slice 24: an endpoint can name a template that holds the settings it shares with other instances of the same
//! service, such as dev and prod.

use serde_json::json;

use crate::support::{FixtureApi, Human, Launch, MockServer};

/// A schema for an older instance: the fixture without `Product.related`.
fn older_schema() -> String {
    let fixture =
        std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/schema.graphql"))
            .expect("read the fixture schema");
    let older = fixture.replace("  related(take: Int = 4): [Product!]!\n", "");
    assert_ne!(older, fixture, "the fixture still has `Product.related`");
    older
}

/// Two instances of the store: `dev` reads the fixture schema, `prod` an older one, and both share `template`.
fn config(dev: &MockServer, prod: &MockServer, template: &str, dev_extra: &str) -> String {
    format!(
        "[templates.store]\n{template}\n\
         [endpoints.store-dev]\ntemplate = \"store\"\nurl = \"{}\"\nschema = \"schema.graphql\"\n{dev_extra}\n\
         [endpoints.store-prod]\ntemplate = \"store\"\nurl = \"{}\"\nschema = \"older.graphql\"\n",
        dev.url(),
        prod.url()
    )
}

const SHARED: &str = "costs = \"store.costs.toml\"\nassumed_list_size = 3\n\n\
                      [templates.store.headers]\n\
                      X-Client = \"fieldtrip-spec\"\n\
                      Authorization = { command = [\"sh\", \"token.sh\", \"count\"], format = \"Bearer {}\" }\n";

const COSTS: &str = "\"Query.products\" = { multiplier = 50 }\n\"Product.related\" = { multiplier = 2 }\n";

#[test]
fn endpoints_take_the_templates_headers_cost_map_and_sizes() {
    let dev = MockServer::answering(&json!({ "data": { "viewer": null } }));
    let prod = MockServer::answering(&json!({ "data": { "viewer": null } }));
    let mut mcp = Launch::new(&config(&dev, &prod, SHARED, "approval = \"client\"\n"))
        .file("older.graphql", &older_schema())
        .file("store.costs.toml", COSTS)
        .human(Human::Accepts)
        .start();

    for endpoint in ["store-dev", "store-prod"] {
        let estimate = mcp.call(
            "estimate_cost",
            &json!({ "endpoint": endpoint, "operation": "{ products { id } viewer { orders { id } } }" }),
        );
        assert!(
            estimate.text.contains("multiplier 50 (rule `Query.products`)"),
            "{endpoint} uses the shared cost map: {}",
            estimate.text
        );
        assert!(
            estimate.text.contains("so 3 items were assumed"),
            "{endpoint} uses the shared assumed_list_size: {}",
            estimate.text
        );
        let sent = mcp.call(
            "execute",
            &json!({ "endpoint": endpoint, "operation": "{ viewer { id } }" }),
        );
        assert!(!sent.is_error, "{endpoint}: {}", sent.text);
    }

    for (name, mock) in [("store-dev", &dev), ("store-prod", &prod)] {
        let requests = mock.requests();
        let request = requests.first().expect("one request");
        assert_eq!(
            request.header("x-client"),
            Some("fieldtrip-spec"),
            "{name} has the literal"
        );
        assert_eq!(
            request.header("authorization"),
            Some("Bearer t0ken-1"),
            "{name} has the command's output, and the command ran once for both endpoints"
        );
    }
}

#[test]
fn approval_is_never_taken_from_the_template() {
    let mock = MockServer::unreachable();
    let mut mcp = Launch::new(&config(&mock, &mock, SHARED, "approval = \"client\"\n"))
        .file("older.graphql", &older_schema())
        .file("store.costs.toml", COSTS)
        .start();

    let listed = mcp.call("list_endpoints", &json!({})).text;

    assert!(
        listed.contains("store-dev\n  url:") && listed.contains("approval: left to the MCP client"),
        "dev sets its own approval: {listed}"
    );
    assert!(
        listed.contains("store-prod\n  url:")
            && listed
                .split("store-prod\n")
                .nth(1)
                .is_some_and(|prod| prod.contains("approval: the user is asked before every operation")),
        "prod sets none, so it asks: {listed}"
    );

    for key in [
        "approval = \"client\"",
        "auto_approve_cost = 5",
        "url = \"http://localhost:9\"",
        "description = \"x\"",
    ] {
        let stderr = Launch::new(&format!(
            "[templates.store]\n{key}\n\n[endpoints.a]\nurl = \"http://localhost:9/graphql\"\ntemplate = \"store\"\n"
        ))
        .fails_to_start();
        let name = key.split(' ').next().unwrap_or_default();
        assert!(
            stderr.contains("template `store`") && stderr.contains(&format!("`{name}` cannot be set in a template")),
            "{key}: {stderr}"
        );
    }
}

#[test]
fn the_endpoints_own_settings_win_and_header_names_ignore_case() {
    let dev = MockServer::answering(&json!({ "data": { "viewer": null } }));
    let prod = MockServer::unreachable();
    let dev_extra = "approval = \"client\"\nassumed_list_size = 7\n\n\
                     [endpoints.store-dev.headers]\nauthorization = \"Bearer dev-only-token\"\n";
    let mut mcp = Launch::new(&config(&dev, &prod, SHARED, dev_extra))
        .file("older.graphql", &older_schema())
        .file("store.costs.toml", COSTS)
        .start();

    let estimate = mcp.call(
        "estimate_cost",
        &json!({ "endpoint": "store-dev", "operation": "{ viewer { orders { id } } }" }),
    );
    assert!(estimate.text.contains("so 7 items were assumed"), "{}", estimate.text);

    let listed = mcp.call("list_endpoints", &json!({})).text;
    assert!(
        listed.contains("headers: X-Client, authorization\n"),
        "the endpoint's `authorization` replaces the template's `Authorization`: {listed}"
    );

    mcp.call(
        "execute",
        &json!({ "endpoint": "store-dev", "operation": "{ viewer { id } }" }),
    );
    let requests = dev.requests();
    let request = requests.first().expect("one request");
    let authorizations: Vec<&str> = request
        .headers
        .iter()
        .filter(|(name, _)| name == "authorization")
        .map(|(_, value)| value.as_str())
        .collect();
    assert_eq!(
        authorizations,
        ["Bearer dev-only-token"],
        "sent once, with the endpoint's value"
    );
}

#[test]
fn a_shared_cost_map_is_checked_against_each_endpoints_schema() {
    let mock = MockServer::unreachable();
    let mut mcp = Launch::new(&config(&mock, &mock, SHARED, ""))
        .file("older.graphql", &older_schema())
        .file("store.costs.toml", COSTS)
        .start();

    let dev = mcp.call("introspect", &json!({ "endpoint": "store-dev" }));
    let prod = mcp.call("introspect", &json!({ "endpoint": "store-prod" }));

    assert!(dev.text.contains("Every rule matches something"), "{}", dev.text);
    assert!(
        prod.text
            .contains("These rules match nothing in this schema: `Product.related`."),
        "prod's schema has no `Product.related`: {}",
        prod.text
    );
}

#[test]
fn a_template_that_is_not_defined_is_refused() {
    let stderr = Launch::new(
        "[templates.shop]\ntimeout = 5\n\n[endpoints.a]\nurl = \"http://localhost:9/graphql\"\ntemplate = \"store\"\n",
    )
    .fails_to_start();

    assert!(
        stderr.contains("endpoint `a`") && stderr.contains("`store`, which is not defined. Templates: `shop`"),
        "names what is defined: {stderr}"
    );
}

#[test]
fn a_problem_in_a_template_names_the_template() {
    let stderr = Launch::new("[templates.store]\ntimeout = 0\n").fails_to_start();

    assert!(stderr.contains("template `store`"), "{stderr}");
    assert!(stderr.contains("`timeout`"), "{stderr}");
}

#[test]
fn an_introspected_endpoint_can_use_a_template() {
    let mock = MockServer::serving(FixtureApi::new());
    let mut mcp = Launch::new(&format!(
        "[templates.store]\nassumed_list_size = 2\n\n[endpoints.live]\nurl = \"{}\"\ntemplate = \"store\"\n",
        mock.url()
    ))
    .start();

    let estimate = mcp.call(
        "estimate_cost",
        &json!({ "endpoint": "live", "operation": "{ products { id } }" }),
    );

    assert!(estimate.text.contains("Estimated cost: 2\n"), "{}", estimate.text);
}
