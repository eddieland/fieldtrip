//! Slice 22: header names ignore case, and the headers fieldtrip sets itself cannot be configured.

use crate::support::Launch;

/// Starts the server with `headers` as the body of the endpoint's headers table, expecting it to refuse.
fn refused(headers: &str) -> String {
    Launch::new(&format!(
        "[endpoints.store]\nurl = \"http://localhost:9/graphql\"\n\n[endpoints.store.headers]\n{headers}"
    ))
    .fails_to_start()
}

#[test]
fn two_names_that_differ_only_in_case_are_refused() {
    let stderr = refused("Authorization = \"first-secret-value\"\nauthorization = \"second-secret-value\"\n");

    assert!(
        stderr.contains("headers `Authorization` and `authorization` are the same header"),
        "names both: {stderr}"
    );
    assert!(!stderr.contains("secret-value"), "no value is quoted: {stderr}");
}

#[test]
fn headers_fieldtrip_sets_are_refused() {
    for name in [
        "Content-Type",
        "accept",
        "Host",
        "Content-Length",
        "Transfer-Encoding",
        "Connection",
    ] {
        let stderr = refused(&format!("{name} = \"x\"\n"));

        assert!(
            stderr.contains(&format!("header `{name}` is set by fieldtrip itself")),
            "{name}: {stderr}"
        );
    }
}
