//! Render `gears/qa-platform/docs/openapi.json` from the gear code.
//!
//! ## Why this exists
//!
//! `docs/openapi.json` and the UI's `src/api/generated/openapi.d.ts` are two
//! descriptions of one API, and until this crate landed neither was generated:
//! the json was hand-edited whenever a route changed, and the `.d.ts` came from
//! `npm run gen:api` pointed at a **live** gateway on `localhost:8087`. Both
//! drifted, in both directions at once — the json carried
//! `/qa/v1/schedules/{id}/ticks`, which the `.d.ts` had never heard of, while
//! the `.d.ts` carried five system gears' paths (`authz-resolver`, `credstore`,
//! `gear-orchestrator`, `oagw`, `types-registry`) that the json does not
//! describe, because whoever last ran `gen:api` had a fuller stack up than
//! whoever last edited the json.
//!
//! The Rust handlers and DTOs are the source of truth. This binary reads them
//! and writes the json; `npm run gen:api` then reads the json and writes the
//! `.d.ts`; `make qa-openapi-check` fails when either committed artefact
//! disagrees with what the code says.
//!
//! ## Why it needs no running stack
//!
//! An `OpenAPI` document is built from route *registrations*, not from a
//! served router. Each gear splits its registration in two — a
//! `register_operations(router, openapi)` that declares the operations and
//! binds nothing, and a `register_routes(..., service)` that adds the
//! `axum::Extension` the handlers read at request time. Only the first half
//! contributes to the document, and it needs no database, no policy decision
//! point, no Keycloak and no gateway. So this binary calls the same four
//! functions `RestApiCapability` calls, into the same `OpenApiRegistryImpl`,
//! and asks for the same `build_openapi`.
//!
//! What it therefore does **not** reproduce is anything the gateway adds at
//! serve time that is not a qa-platform route: system gears' paths, and the
//! `servers` entry a non-empty `prefix_path` would produce. Neither belongs in
//! this gear's contract, and the absence of both is the point rather than a
//! gap — see [`document`] for the `info` block, which is the one piece that
//! comes from configuration rather than from code.
//!
//! ## Determinism
//!
//! `OpenApiRegistryImpl` keeps its operations in a `DashMap`, so `paths`
//! arrives in whatever order the iterator felt like. Every object key is
//! therefore sorted recursively before writing, exactly as
//! `tools/scripts/sort_openapi_json.py` does for `docs/api/api.json` — done
//! here in Rust so the check has no Python prerequisite. Arrays are left
//! alone: `parameters`, `required` and `enum` come out of the builders in
//! declaration order, which is already stable and is meaningful for the reader.
//!
//! ## Usage
//!
//! ```text
//! cargo run -p qa-platform-openapi             # write docs/openapi.json
//! cargo run -p qa-platform-openapi -- --check  # fail if it is out of date
//! ```

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use axum::Router;
use serde_json::{Map, Value};
use toolkit::api::{OpenApiInfo, OpenApiRegistry, OpenApiRegistryImpl};

/// The document's title, as published.
///
/// The `info` block is the one part of the document that does not come from
/// the gear code: at serve time the gateway fills it from
/// `api-gateway.config.openapi` in whichever stack config it was started with.
/// Pinning it here rather than parsing a yaml keeps the committed contract
/// from changing because someone edited a development server's banner, and
/// keeps the generator free of a config-file dependency it would otherwise
/// need only for three strings.
const TITLE: &str = "QA Platform";
/// The document's version, matching every qa-platform crate's `0.1.0`.
const VERSION: &str = "0.1.0";
/// The document's description.
const DESCRIPTION: &str = "QA Platform development server";

/// Where the rendered document is committed, resolved at compile time so the
/// binary works from any working directory.
fn output_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/openapi.json")
}

/// The `OpenAPI` document for the `/qa/v1` surface, as `serde_json`.
///
/// The four `register_operations` calls are the same ones each gear's
/// `register_rest` makes, in the order the gateway would make them; order does
/// not survive [`sorted`] anyway, but keeping it matching makes a duplicate
/// `(method, path)` registration fail here the way it fails at startup.
fn document() -> Result<Value> {
    let openapi = OpenApiRegistryImpl::new();
    let registry: &dyn OpenApiRegistry = &openapi;

    let mut router = Router::new();
    router = qa_environments::api::rest::routes::register_operations(router, registry);
    router = qa_catalog::api::rest::routes::register_operations(router, registry);
    router = qa_runs::api::rest::register_operations(router, registry);
    let _router = qa_insights::api::rest::routes::register_operations(router, registry);

    let info = OpenApiInfo {
        title: TITLE.to_owned(),
        version: VERSION.to_owned(),
        description: Some(DESCRIPTION.to_owned()),
        servers: vec![],
    };
    let doc = openapi
        .build_openapi(&info)
        .context("building the OpenAPI document from the registered operations")?;

    let value = serde_json::to_value(&doc).context("serializing the OpenAPI document")?;
    Ok(sorted(value))
}

/// Every object key sorted, recursively. Arrays keep their order.
fn sorted(value: Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            let mut out = Map::new();
            let mut object = object;
            for key in keys {
                if let Some(entry) = object.remove(&key) {
                    out.insert(key, sorted(entry));
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
        other => other,
    }
}

/// The document as it is written to disk: two-space indent, trailing newline.
fn rendered() -> Result<String> {
    let mut text = serde_json::to_string_pretty(&document()?)
        .context("pretty-printing the OpenAPI document")?;
    text.push('\n');
    Ok(text)
}

fn main() -> Result<()> {
    let check = std::env::args().skip(1).any(|arg| arg == "--check");
    let path = output_path();
    let text = rendered()?;

    if check {
        let committed = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        if committed == text {
            println!("{} is up to date", path.display());
            return Ok(());
        }
        bail!(
            "{} does not match the document generated from the gear code.\n\
             Run `make qa-openapi` and commit the result (and `make ui-contract`\n\
             for the UI types derived from it).",
            path.display()
        );
    }

    std::fs::write(&path, &text).with_context(|| format!("writing {}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{document, sorted};
    use serde_json::json;

    /// **The generated document describes this gear and nothing else.**
    ///
    /// The `.d.ts` drifted precisely by acquiring five *other* gears' paths
    /// from whatever stack happened to be up when `gen:api` last ran. A
    /// generator that reads route registrations cannot do that — this pins it,
    /// so the property survives someone adding a convenience dependency here.
    #[test]
    fn every_path_is_under_the_gear_prefix() {
        let doc = document().expect("the document must build");
        let paths = doc["paths"].as_object().expect("paths is an object");
        assert!(!paths.is_empty(), "the document must declare paths");
        for path in paths.keys() {
            assert!(
                path.starts_with("/qa/v1/"),
                "{path} is not a qa-platform route; this generator registers \
                 only the four qa gears' operations"
            );
        }
    }

    /// Sorting is recursive, and leaves arrays alone.
    #[test]
    fn keys_sort_recursively_and_arrays_do_not() {
        let input = json!({ "b": 1, "a": { "d": [3, 1, 2], "c": 0 } });
        let expected = r#"{"a":{"c":0,"d":[3,1,2]},"b":1}"#;
        assert_eq!(serde_json::to_string(&sorted(input)).unwrap(), expected);
    }
}
