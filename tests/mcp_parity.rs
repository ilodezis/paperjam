//! REST and MCP offer the same capabilities: every REST route has a tool and
//! every tool has a REST route, except for an explicit list of exceptions.

mod common;

use std::collections::BTreeSet;

use axum::http::Method;
use common::{TestEnv, stderr, stdout};
use paperjam::mcp::tools::REST_ROUTES;
use reqwest::{Client, StatusCode};
use serde_json::Value;

const PASSWORD: &str = "correct horse battery";

/// REST routes that deliberately have no tool, and why.
const EXCEPTIONS: &[(&str, &str)] = &[
    // Authentication and transport belong to the HTTP surface.
    ("GET", "/setup"),
    ("POST", "/setup"),
    ("POST", "/auth/login"),
    ("POST", "/auth/logout"),
    ("GET", "/auth/session"),
    ("PATCH", "/auth/session"),
    ("POST", "/auth/password"),
    ("POST", "/auth/passkey/challenge"),
    ("POST", "/auth/passkey"),
    ("GET", "/openapi.json"),
    // Links carry their own authority; the tools mint them.
    ("GET", "/dl/{token}"),
    ("PUT", "/ul/{token}"),
    // Inbound email webhook is authenticated by its own write-only secret.
    ("POST", "/ingest/email"),
    ("GET", "/shared/{token}/pages/{file}"),
    ("GET", "/shared/{token}/download"),
    // Live share links are reviewed and revoked by people in the web UI.
    ("GET", "/documents/{id}/shares"),
    ("DELETE", "/shares/{id}"),
    // Agents can never purge or empty the trash.
    ("DELETE", "/documents/{id}"),
    ("DELETE", "/trash"),
    // Deleting vocabulary unlinks documents; agents merge or rename instead.
    ("DELETE", "/people/{id}"),
    ("DELETE", "/correspondents/{id}"),
    // Administration is for people in the web UI.
    ("GET", "/users"),
    ("POST", "/users"),
    ("PATCH", "/users/{id}"),
    ("DELETE", "/users/{id}"),
    ("GET", "/calendar-feed"),
    ("POST", "/calendar-feed"),
    ("DELETE", "/calendar-feed"),
    ("GET", "/tokens"),
    ("POST", "/tokens"),
    ("DELETE", "/tokens/{id}"),
    ("GET", "/connections"),
    ("DELETE", "/connections/{id}"),
    ("GET", "/settings/email"),
    ("PATCH", "/settings/email"),
    ("POST", "/settings/email/secret"),
    ("DELETE", "/settings/email/secret"),
    // Passkeys are a web sign-in: managed from a cookie session only, like purge.
    ("GET", "/passkeys"),
    ("POST", "/passkeys"),
    ("POST", "/passkeys/challenge"),
    ("DELETE", "/passkeys/{id}"),
    // A UI-only derivative; models look at `get_page_image`.
    ("GET", "/documents/{id}/thumbnail.webp"),
    // Event audit history is read-only metadata for people in the web UI.
    ("GET", "/documents/{id}/events"),
];

fn contract_tool_names() -> BTreeSet<String> {
    let contract: Value = serde_json::from_str(include_str!("../src/mcp/tools.json")).unwrap();
    contract["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn every_rest_route_has_a_tool_and_vice_versa() {
    let rest: BTreeSet<(Method, String)> = paperjam::http::api::route_list().into_iter().collect();
    let excepted: BTreeSet<(Method, String)> = EXCEPTIONS
        .iter()
        .map(|(m, p)| (m.parse().unwrap(), (*p).to_string()))
        .collect();
    let mapped: BTreeSet<(Method, String)> = REST_ROUTES
        .iter()
        .flat_map(|t| t.rest.iter().map(|(m, p)| (m.clone(), (*p).to_string())))
        .collect();

    // Every route a tool claims, and every exception, exists.
    for (method, path) in mapped.iter().chain(excepted.iter()) {
        assert!(
            rest.contains(&(method.clone(), path.clone())),
            "{method} {path} is listed but the REST API has no such route"
        );
    }
    // No route is both mapped and excepted.
    let both: Vec<_> = mapped.intersection(&excepted).collect();
    assert!(
        both.is_empty(),
        "mapped to a tool and also on the exception list: {both:?}"
    );
    // Every REST route is covered one way or the other.
    for (method, path) in &rest {
        assert!(
            mapped.contains(&(method.clone(), path.clone()))
                || excepted.contains(&(method.clone(), path.clone())),
            "REST route {method} {path} has no MCP tool and is not on the exception list"
        );
    }

    // Every tool names at least one route, and the table has no strangers.
    let tools = contract_tool_names();
    let tabled: BTreeSet<String> = REST_ROUTES.iter().map(|t| t.tool.to_string()).collect();
    assert_eq!(
        tools, tabled,
        "REST_ROUTES and tools.json list different tools"
    );
    for entry in REST_ROUTES {
        assert!(!entry.rest.is_empty(), "{} has no REST route", entry.tool);
    }
}

#[test]
fn no_purge_capability_is_mapped_to_a_tool() {
    for entry in REST_ROUTES {
        for (method, path) in entry.rest {
            assert!(
                !(*method == Method::DELETE && (*path == "/trash" || *path == "/documents/{id}")),
                "{} maps to a purge route",
                entry.tool
            );
        }
    }
}

/// The two routes added for parity do what their tools do.
#[tokio::test]
async fn rest_link_endpoints_mirror_the_tools() {
    let env = TestEnv::new();
    let out = env.run_with_password(&["user", "add", "--admin", "alice"], PASSWORD);
    assert!(out.status.success(), "user add failed: {}", stderr(&out));
    let out = env.run(&["token", "create", "--user", "alice", "parity"]);
    let token = stdout(&out).trim().to_string();
    let server = env.serve();
    let http = Client::new();

    let anonymous = http
        .post(server.url("/api/v1/upload-links"))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);

    let link: Value = http
        .post(server.url("/api/v1/upload-links"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let url = link["url"].as_str().expect("url");
    assert!(link["expires_at"].is_string() && link["curl_example"].is_string());

    // The link uploads one file; the document can then be linked for download.
    let uploaded: Value = http
        .put(format!("{url}?filename=note.txt"))
        .body("a plain text note for the parity test")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = uploaded["id"].as_str().expect("document id");

    let download: Value = http
        .post(server.url(&format!("/api/v1/documents/{id}/download-link")))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(download["filename"], "note.txt");
    let fetched = http
        .get(download["url"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(fetched.status(), StatusCode::OK);
    assert_eq!(
        fetched.text().await.unwrap(),
        "a plain text note for the parity test"
    );
}
