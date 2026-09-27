//! Postman import tests against checked-in fixture collections.

use std::path::PathBuf;

use swarmo_core::model::*;
use swarmo_core::postman;
use swarmo_core::WorkspaceStore;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn workspace() -> (tempfile::TempDir, WorkspaceStore) {
    let tmp = tempfile::TempDir::new().unwrap();
    let store = WorkspaceStore::create(tmp.path().join("ws"), "import test").unwrap();
    (tmp, store)
}

/// Find a request anywhere in the tree by display name.
fn find(store: &WorkspaceStore, name: &str) -> RequestDef {
    store.get_request(&find_ref(store, name)).unwrap()
}

/// The ref of a request anywhere in the tree, by display name.
fn find_ref(store: &WorkspaceStore, name: &str) -> String {
    fn walk(nodes: &[TreeNode], name: &str, out: &mut Option<String>) {
        for n in nodes {
            if n.kind == NodeKind::Request && n.name == name {
                *out = Some(n.node_ref.clone());
            }
            walk(&n.children, name, out);
        }
    }
    let tree = store.tree().unwrap();
    let mut found = None;
    walk(&tree, name, &mut found);
    found.unwrap_or_else(|| panic!("no request named {name}"))
}

// ---------------------------------------------------------------------------

#[test]
fn imports_structure_auth_and_scripts() {
    let (_t, store) = workspace();
    let report = postman::import_collection(&store, &fixture("basic.postman_collection.json"))
        .expect("import failed");

    assert_eq!(report.collection_name, "Orders API");
    assert_eq!(report.requests_imported, 3);
    assert_eq!(report.folders_imported, 1);
    assert_eq!(
        report.environment_created.as_deref(),
        Some("Orders API-imported")
    );
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);

    // Structure: collection > (List orders, Create order, Admin > Delete order)
    let tree = store.tree().unwrap();
    assert_eq!(tree.len(), 1);
    assert_eq!(tree[0].name, "Orders API");
    let admin = tree[0]
        .children
        .iter()
        .find(|c| c.name == "Admin")
        .expect("Admin folder missing");
    assert_eq!(admin.kind, NodeKind::Folder);
    assert_eq!(admin.children[0].name, "Delete order");

    // Collection-level bearer auth and pre-request script.
    let container = store.get_container(&report.collection_ref).unwrap();
    match container.auth {
        Auth::Bearer { ref token } => assert_eq!(token, "{{apiKey}}"),
        ref other => panic!("expected bearer auth, got {other:?}"),
    }
    assert!(container.scripts.pre_request.contains("pm.environment.set"));

    // Query params, including the disabled one.
    let list = find(&store, "List orders");
    assert_eq!(list.method, "GET");
    assert_eq!(
        list.url, "{{baseUrl}}/orders",
        "the query must move to params"
    );
    assert_eq!(list.params.len(), 3);
    assert!(list.params.iter().any(|p| p.key == "limit" && p.enabled));
    assert!(list.params.iter().any(|p| p.key == "cursor" && !p.enabled));
    assert!(list.scripts.post_response.contains("pm.test"));

    // JSON body detection.
    let create = find(&store, "Create order");
    match create.body {
        Body::Json { ref text } => assert!(text.contains("\"sku\"")),
        ref other => panic!("expected a JSON body, got {other:?}"),
    }

    // Request-level basic auth overrides the collection's bearer.
    let del = find(&store, "Delete order");
    match del.auth {
        Auth::Basic {
            ref username,
            ref password,
        } => {
            assert_eq!(username, "admin");
            assert_eq!(password, "hunter2");
        }
        ref other => panic!("expected basic auth, got {other:?}"),
    }
}

#[test]
fn collection_variables_become_an_environment() {
    let (_t, store) = workspace();
    postman::import_collection(&store, &fixture("basic.postman_collection.json")).unwrap();

    let env = store.get_environment("Orders API-imported").unwrap();
    assert!(env
        .variables
        .iter()
        .any(|v| v.key == "baseUrl" && v.value == "https://api.example.com"));
    let key = env.variables.iter().find(|v| v.key == "apiKey").unwrap();
    assert!(key.secret, "a Postman secret variable must stay secret");
}

#[test]
fn imports_every_body_type() {
    let (_t, store) = workspace();
    let report =
        postman::import_collection(&store, &fixture("bodies.postman_collection.json")).unwrap();
    assert_eq!(report.requests_imported, 7);

    match find(&store, "Urlencoded").body {
        Body::Form { ref fields } => {
            assert_eq!(fields.len(), 2);
            assert!(fields.iter().any(|f| f.key == "grant_type" && f.enabled));
            assert!(fields.iter().any(|f| f.key == "scope" && !f.enabled));
        }
        ref other => panic!("expected a form body, got {other:?}"),
    }

    match find(&store, "Form data").body {
        Body::Multipart { ref parts } => {
            assert_eq!(parts.len(), 2);
            let file = parts.iter().find(|p| p.key == "file").unwrap();
            assert_eq!(file.kind, MultipartKind::File);
            assert_eq!(file.value, "/tmp/photo.png");
        }
        ref other => panic!("expected a multipart body, got {other:?}"),
    }

    match find(&store, "GraphQL").body {
        Body::Graphql {
            ref query,
            ref variables,
        } => {
            assert!(query.contains("query Me"));
            assert!(variables.contains("limit"));
        }
        ref other => panic!("expected a GraphQL body, got {other:?}"),
    }

    match find(&store, "Plain text").body {
        Body::Text {
            ref text,
            ref content_type,
        } => {
            assert_eq!(text, "a,b\n1,2");
            assert_eq!(content_type.as_deref(), Some("text/csv"));
        }
        ref other => panic!("expected a text body, got {other:?}"),
    }

    match find(&store, "Binary file").body {
        Body::Binary { ref path } => assert_eq!(path, "/tmp/blob.bin"),
        ref other => panic!("expected a binary body, got {other:?}"),
    }

    match find(&store, "API key auth").auth {
        Auth::ApiKeyHeader {
            ref header_name,
            ref value,
        } => {
            assert_eq!(header_name, "X-Api-Key");
            assert_eq!(value, "secret");
        }
        ref other => panic!("expected API-key auth, got {other:?}"),
    }

    // A request that is just a URL string.
    let ping = find(&store, "Bare url string");
    assert_eq!(ping.url, "https://api.example.com/ping");
}

#[test]
fn unmappable_features_warn_rather_than_disappear() {
    let (_t, store) = workspace();
    let report =
        postman::import_collection(&store, &fixture("edge.postman_collection.json")).unwrap();

    // Everything was still imported.
    assert_eq!(report.requests_imported, 8);
    assert!(!report.warnings.is_empty());

    let joined = report.warnings.join("\n");
    assert!(joined.contains("oauth2"), "{joined}");
    assert!(joined.contains("pm.cookies"), "{joined}");
    assert!(joined.contains("query string"), "{joined}");

    // The unsupported script is kept verbatim, with a warning comment.
    let script = find(&store, "Unsupported script");
    assert!(script
        .scripts
        .post_response
        .contains("SWARMO-IMPORT-WARNING"));
    assert!(script.scripts.post_response.contains("pm.cookies.get"));

    // Unsupported auth degrades to none rather than silently inheriting.
    assert!(matches!(find(&store, "Unsupported auth").auth, Auth::None));
}

#[test]
fn filenames_are_made_safe_and_collisions_resolved() {
    let (_t, store) = workspace();
    let report =
        postman::import_collection(&store, &fixture("edge.postman_collection.json")).unwrap();
    let dir = store.path_of(&report.collection_ref).unwrap();

    let names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();

    // Windows reserved device name gets a prefix.
    assert!(names.iter().any(|n| n == "_CON.req.json"), "{names:?}");
    // Illegal characters are replaced.
    assert!(
        names.iter().any(|n| n == "Has_Illegal_Chars_.req.json"),
        "{names:?}"
    );
    // Duplicate display names get distinct files.
    assert!(names.iter().any(|n| n == "Duplicate.req.json"), "{names:?}");
    assert!(
        names.iter().any(|n| n == "Duplicate 2.req.json"),
        "{names:?}"
    );

    // The display names inside the files are untouched.
    assert_eq!(find(&store, "CON").name, "CON");
    assert_eq!(
        find(&store, "Has/Illegal:Chars?").name,
        "Has/Illegal:Chars?"
    );
}

#[test]
fn deeply_nested_folders_survive() {
    let (_t, store) = workspace();
    postman::import_collection(&store, &fixture("edge.postman_collection.json")).unwrap();
    let leaf = find(&store, "Leaf");
    assert_eq!(leaf.url, "https://x.test/leaf");

    let tree = store.tree().unwrap();
    let nested = tree[0]
        .children
        .iter()
        .find(|c| c.name == "Nested")
        .unwrap();
    assert_eq!(nested.children[0].name, "Deeper");
    assert_eq!(nested.children[0].children[0].name, "Leaf");
}

#[test]
fn imports_an_environment_export() {
    let (_t, store) = workspace();
    let name = postman::import_environment(&store, &fixture("prod.postman_environment.json"))
        .expect("environment import failed");
    assert_eq!(name, "Production");

    let env = store.get_environment_with_secrets("Production").unwrap();
    assert_eq!(env.variables.len(), 3);
    assert_eq!(
        env.variables
            .iter()
            .find(|v| v.key == "baseUrl")
            .unwrap()
            .value,
        "https://api.example.com"
    );

    let key = env.variables.iter().find(|v| v.key == "apiKey").unwrap();
    assert!(key.secret);
    assert_eq!(key.value, "live_key_123");

    // The committed file must not contain the secret.
    let on_disk = store.get_environment("Production").unwrap();
    assert_eq!(
        on_disk
            .variables
            .iter()
            .find(|v| v.key == "apiKey")
            .unwrap()
            .value,
        ""
    );

    assert!(
        !env.variables
            .iter()
            .find(|v| v.key == "unused")
            .unwrap()
            .enabled
    );
}

#[test]
fn rejects_files_that_are_not_postman_exports() {
    let (_t, store) = workspace();
    let tmp = tempfile::TempDir::new().unwrap();

    let not_postman = tmp.path().join("random.json");
    std::fs::write(&not_postman, r#"{"hello":"world"}"#).unwrap();
    let err = postman::import_collection(&store, &not_postman).unwrap_err();
    assert!(
        err.to_string().contains("not a Postman collection"),
        "{err}"
    );

    let broken = tmp.path().join("broken.json");
    std::fs::write(&broken, "{ not json").unwrap();
    assert!(postman::import_collection(&store, &broken).is_err());
}

#[test]
fn imported_requests_resolve_and_are_sendable() {
    let (_t, store) = workspace();
    let report =
        postman::import_collection(&store, &fixture("basic.postman_collection.json")).unwrap();

    // Point the imported environment at a concrete host and resolve a request.
    let mut env = store
        .get_environment_with_secrets(report.environment_created.as_ref().unwrap())
        .unwrap();
    for v in env.variables.iter_mut() {
        if v.key == "apiKey" {
            v.value = "tok".into();
        }
    }
    store.save_environment(&env).unwrap();

    let tree = store.tree().unwrap();
    let list_ref = tree[0]
        .children
        .iter()
        .find(|c| c.name == "List orders")
        .unwrap()
        .node_ref
        .clone();

    let merged = store.merged_request(&list_ref).unwrap();
    let scope = store.var_scope(Some(&env.name)).unwrap();
    let resolved = swarmo_core::finalize(&merged, &scope);

    assert_eq!(
        resolved.url,
        "https://api.example.com/orders?limit=10&status=open"
    );
    // Bearer auth was inherited from the collection and interpolated.
    assert_eq!(
        resolved
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
            .unwrap()
            .1,
        "Bearer tok"
    );
    assert!(resolved.unresolved.is_empty(), "{:?}", resolved.unresolved);
}

/// Resolve a request by name exactly as it would be sent, with no environment.
fn resolve(store: &WorkspaceStore, name: &str) -> swarmo_core::ResolvedRequest {
    let merged = store.merged_request(&find_ref(store, name)).unwrap();
    swarmo_core::finalize(&merged, &store.var_scope(None).unwrap())
}

#[test]
fn encoded_query_values_are_not_encoded_twice() {
    let (_t, store) = workspace();
    postman::import_collection(&store, &fixture("fidelity.postman_collection.json")).unwrap();

    let list = find(&store, "Encoded query");
    assert_eq!(list.params[0].value, "hello world");
    assert_eq!(list.params[1].value, "a@b.com");
    assert_eq!(list.params[2].value, "a b", "+ is a space in a query");

    // What goes out is encoded once, not `%2520`.
    assert_eq!(
        resolve(&store, "Encoded query").url,
        "https://x.test/search?q=hello%20world&email=a%40b.com&t=a%20b"
    );
}

#[test]
fn path_variables_are_substituted() {
    let (_t, store) = workspace();
    let report =
        postman::import_collection(&store, &fixture("fidelity.postman_collection.json")).unwrap();

    // A valued variable goes in directly; an empty one becomes a variable
    // reference, so sending it is flagged instead of `:postId` going out.
    let req = find(&store, "Path variables");
    assert_eq!(req.url, "https://x.test/users/42/posts/{{postId}}");
    let resolved = resolve(&store, "Path variables");
    assert!(
        resolved.unresolved.iter().any(|u| u == "postId"),
        "{:?}",
        resolved.unresolved
    );
    let joined = report.warnings.join(
        "
",
    );
    assert!(joined.contains(":postId"), "{joined}");
    assert!(!joined.contains(":userId"), "{joined}");
}

#[test]
fn null_or_typeless_auth_inherits() {
    let (_t, store) = workspace();
    postman::import_collection(&store, &fixture("fidelity.postman_collection.json")).unwrap();

    for name in ["Null auth", "Typeless auth"] {
        assert!(matches!(find(&store, name).auth, Auth::Inherit), "{name}");
        // And the collection's bearer token actually reaches the request.
        let resolved = resolve(&store, name);
        let auth = resolved
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
            .map(|(_, v)| v.as_str());
        assert_eq!(auth, Some("Bearer tok"), "{name}");
    }
}

#[test]
fn a_raw_body_language_sets_the_content_type() {
    let (_t, store) = workspace();
    postman::import_collection(&store, &fixture("fidelity.postman_collection.json")).unwrap();

    match find(&store, "XML body").body {
        Body::Text {
            ref content_type, ..
        } => assert_eq!(content_type.as_deref(), Some("application/xml")),
        ref other => panic!("expected a text body, got {other:?}"),
    }
    // An explicit header still wins over the language picker.
    match find(&store, "HTML body with header").body {
        Body::Text {
            ref content_type, ..
        } => assert_eq!(content_type.as_deref(), Some("application/xhtml+xml")),
        ref other => panic!("expected a text body, got {other:?}"),
    }
}
