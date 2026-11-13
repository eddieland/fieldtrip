//! Slice 30: introspection reads up to `max_schema_bytes`, 32 MiB unless the config says otherwise, apart from what
//! `execute` reads. A schema larger than the limit is an error that names the limit, not one that blames the API.

use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;

use crate::support::{FixtureApi, Launch, MockServer};

/// The fixture schema with enough described types added that its introspection response is well over 1 MiB.
fn large_sdl() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/schema.graphql");
    let mut sdl = std::fs::read_to_string(path).expect("read the fixture schema");
    let words = "A field with a long description, as large production APIs write them. ".repeat(7);
    for ty in 0..100 {
        let _ = writeln!(sdl, "\n\"Filler type {ty}.\"\ntype Filler{ty} {{");
        for field in 0..20 {
            let _ = writeln!(sdl, "  \"{words}\"\n  field{field}(first: Int = 10): [String!]!");
        }
        sdl.push_str("}\n");
    }
    sdl
}

/// A mock that serves `sdl` by introspection and records the size of the largest body it sent.
fn serving(sdl: &str) -> (MockServer, Arc<AtomicUsize>) {
    let api = FixtureApi::serving(sdl);
    let largest = Arc::new(AtomicUsize::new(0));
    let sent = Arc::clone(&largest);
    let mock = MockServer::start(move |request| {
        let response = api.respond(request);
        sent.fetch_max(response.body.len(), Ordering::SeqCst);
        response
    });
    (mock, largest)
}

#[test]
fn a_schema_larger_than_a_mebibyte_loads_by_introspection() {
    let sdl = large_sdl();
    let (mock, largest) = serving(&sdl);
    let config = format!(
        "[endpoints.live]\nurl = \"{url}\"\n\n[endpoints.file]\nurl = \"{url}\"\nschema = \"large.graphql\"\n",
        url = mock.url()
    );
    let mut mcp = Launch::new(&config).file("large.graphql", &sdl).start();

    let live = mcp.call("introspect", &json!({ "endpoint": "live" }));
    let file = mcp.call("introspect", &json!({ "endpoint": "file" }));

    assert!(!live.is_error, "the large schema loads: {}", live.text);
    assert!(
        largest.load(Ordering::SeqCst) > 1024 * 1024,
        "the introspection response should be over 1 MiB for this test to mean anything"
    );
    let overview = |text: &str| text.lines().skip(1).collect::<Vec<_>>().join("\n");
    assert_eq!(
        overview(&live.text),
        overview(&file.text),
        "introspection gives the same counts and entry points as the SDL file"
    );
    let described = mcp.call("describe", &json!({ "endpoint": "live", "name": "Filler99.field19" }));
    assert!(!described.is_error, "the last type made it: {}", described.text);
}

#[test]
fn a_schema_over_the_limit_is_an_error_that_names_the_limit() {
    let (mock, _) = serving(&large_sdl());
    let config = format!(
        "[endpoints.live]\nurl = \"{}\"\nmax_schema_bytes = 1048576\n",
        mock.url()
    );
    let mut mcp = Launch::new(&config).start();

    let reply = mcp.call("introspect", &json!({ "endpoint": "live" }));

    assert!(reply.is_error, "a schema that is cut off cannot load: {}", reply.text);
    for phrase in ["larger than 1 MiB", "`max_schema_bytes`", "set `schema`", "HTTP 200"] {
        assert!(reply.text.contains(phrase), "the error says {phrase:?}: {}", reply.text);
    }
    assert!(
        !reply.text.contains("turned off"),
        "the API did answer, so the error must not blame introspection being off: {}",
        reply.text
    );
    assert!(
        !reply.text.contains("__schema"),
        "no part of the body is quoted: {}",
        reply.text
    );
    assert_eq!(
        mock.requests().len(),
        1,
        "the fallback query would be no shorter, so it is not sent"
    );
}

#[test]
fn a_template_sets_the_limit_and_check_shows_it() {
    let (mock, _) = serving(&large_sdl());
    let config = format!(
        "[templates.small]\nmax_schema_bytes = 4096\n\n\
         [endpoints.capped]\ntemplate = \"small\"\nurl = \"{url}\"\n\n\
         [endpoints.roomy]\ntemplate = \"small\"\nurl = \"{url}\"\nmax_schema_bytes = 67108864\n\n\
         [endpoints.plain]\nurl = \"{url}\"\n",
        url = mock.url()
    );

    let mut mcp = Launch::new(&config).start();
    let capped = mcp.call("introspect", &json!({ "endpoint": "capped" }));
    let roomy = mcp.call("introspect", &json!({ "endpoint": "roomy" }));
    let checked = Launch::new(&config).check();

    assert!(capped.is_error, "the template's limit applies: {}", capped.text);
    assert!(capped.text.contains("larger than 4 KiB"), "{}", capped.text);
    assert!(!roomy.is_error, "the endpoint's own limit wins: {}", roomy.text);
    assert!(checked.passed, "{}", checked.stdout);
    for (endpoint, limit) in [("capped", "4 KiB"), ("roomy", "64 MiB"), ("plain", "32 MiB")] {
        let expected = format!("\n{endpoint}\n");
        let section = checked.stdout.split(&expected).nth(1).unwrap_or_default();
        assert!(
            section.contains(&format!("  max schema bytes: {limit}\n")),
            "check shows the limit for `{endpoint}`: {}",
            checked.stdout
        );
    }
}

#[test]
fn a_limit_of_zero_is_refused_at_startup() {
    let config = "[endpoints.live]\nurl = \"http://localhost:9/graphql\"\nmax_schema_bytes = 0\n";

    let stderr = Launch::new(config).fails_to_start();

    assert!(stderr.contains("`max_schema_bytes`"), "names the setting: {stderr}");
}
