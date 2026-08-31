//! Conversions between `SeaORM` entity models and SDK contract types.
//!
//! ## Fail-closed decoding
//!
//! Two columns (`custom_plans.files`, `custom_plans.tags`) hold JSON arrays,
//! and two (`custom_plans.timeout_seconds`, `test_bundles.size_bytes`) are `BIGINT`
//! backing an unsigned SDK field. Content that doesn't decode can only mean a
//! corrupt row (manual edit, schema drift, or a bug), never a legitimate
//! value: the writers below always emit well-formed JSON and non-negative
//! integers. Such a row therefore surfaces as `DomainError::Internal` instead
//! of silently reading as an empty list or a clamped number — a plan that
//! quietly loses its file list would launch a run that executes nothing, and a
//! bundle whose size decodes as `0` would look empty to its consumer.

use qa_catalog_sdk::{CustomPlan, CustomPlanEntry, Product, SshKey, TestBundle, TestRepository};
use serde_json::Value;
use uuid::Uuid;

use super::entity::{custom_plan, product, ssh_key, test_bundle, test_repository};
use crate::domain::error::DomainError;

/// Convert a test-repository database entity to a contract model.
#[must_use]
pub fn repo_to_sdk(m: test_repository::Model) -> TestRepository {
    TestRepository {
        id: m.id,
        product_id: m.product_id,
        name: m.name,
        url: m.url,
        default_branch: m.default_branch,
        content_root: m.content_root,
        credential_ref: m.credential_ref,
        last_synced_at: m.last_synced_at,
        sync_error: m.sync_error,
        created_at: m.created_at,
        updated_at: m.updated_at,
    }
}

/// Convert an SSH-key-metadata database entity to a contract model.
#[must_use]
pub fn ssh_key_to_sdk(m: ssh_key::Model) -> SshKey {
    SshKey {
        id: m.id,
        name: m.name,
        credstore_ref: m.credstore_ref,
        fingerprint: m.fingerprint,
        created_at: m.created_at,
    }
}

/// Convert a product database entity to a contract model.
///
/// The `product_key` column is exposed as `Product::key` — the column avoids
/// `key` because it is a `MySQL` reserved word (see the migration module docs).
#[must_use]
pub fn product_to_sdk(m: product::Model) -> Product {
    Product {
        id: m.id,
        name: m.name,
        key: m.product_key,
        description: m.description,
        folder: m.folder,
        created_at: m.created_at,
        updated_at: m.updated_at,
    }
}

/// Convert a custom-plan database entity to a contract model.
///
/// # Errors
///
/// Returns `DomainError::Internal` if `files` or `tags` do not decode, or if
/// `timeout_seconds` is negative — see the module-level fail-closed note.
pub fn custom_plan_to_sdk(m: custom_plan::Model) -> Result<CustomPlan, DomainError> {
    let files = custom_plan_entries_from_json(&m.files, "qa_custom_plans.files", m.id)?;
    let tags = tags_from_json(&m.tags, "qa_custom_plans.tags", m.id)?;
    let timeout_seconds = m
        .timeout_seconds
        .map(|v| u64_from_db(v, "qa_custom_plans.timeout_seconds", m.id))
        .transpose()?;

    Ok(CustomPlan {
        id: m.id,
        name: m.name,
        files,
        tags,
        timeout_seconds,
        created_at: m.created_at,
        updated_at: m.updated_at,
    })
}

/// Convert a test-bundle database entity to a contract model.
///
/// # Errors
///
/// Returns `DomainError::Internal` if `size_bytes` is negative — see the
/// module-level fail-closed note.
pub fn bundle_to_sdk(m: test_bundle::Model) -> Result<TestBundle, DomainError> {
    let size_bytes = u64_from_db(m.size_bytes, "qa_test_bundles.size_bytes", m.id)?;

    Ok(TestBundle {
        id: m.id,
        storage_ref: m.storage_ref,
        checksum_sha256: m.checksum_sha256,
        size_bytes,
        expires_at: m.expires_at,
        created_at: m.created_at,
    })
}

/// The stored shape of one `qa_custom_plans.files` element.
///
/// # This type exists to read a payload it does not write
///
/// `plan_path` was added to [`CustomPlanEntry`] after the column shipped, and
/// **every row written before that holds a two-element array**
/// `["<repo uuid>", "<path>"]` — the serialization of the `(Uuid, String)` pair
/// the SDK model used to carry. A widening that could not read those rows would
/// not degrade gracefully: the module's fail-closed rule turns an undecodable
/// payload into `DomainError::Internal`, so *every stored custom plan* would
/// become unreadable, and every launch targeting one would fail.
///
/// `serde`'s derived `Deserialize` for a struct with named fields implements
/// **both** `visit_map` and `visit_seq`, and in `visit_seq` a field carrying
/// `#[serde(default)]` takes its default when the sequence runs out. So this one
/// type accepts all three payloads that can legitimately be in the column:
///
/// | payload | `plan_path` |
/// |---|---|
/// | `["<uuid>", "a.py"]` — pre-`plan_path` rows | `None` |
/// | `["<uuid>", "a.py", "p.yaml"]` | `Some("p.yaml")` |
/// | `{"repo_id": "<uuid>", "path": "a.py", …}` — what this writes | as given |
///
/// It stays fail-closed on the shapes that can only mean corruption: a bare
/// string, an object missing `path`, and a one-element array all still error,
/// because `#[serde(default)]` is on the *added* field only.
/// `stored_pre_plan_path_rows_still_decode` and
/// `corrupt_json_fails_closed_instead_of_reading_as_empty` pin both halves.
///
/// **`plan_path` must stay `Option` and stay defaulted here even though the write
/// model requires it.** `qa_catalog_sdk::NewCustomPlanEntry::plan_path` is a plain
/// `String` since 2026-08-14 — the API refuses an entry that names no plan — and
/// the temptation is to tidy this struct to match. Doing so turns every row
/// written before the field existed into `DomainError::Internal`, which is the
/// precise failure this type exists to prevent. That mutation is break-tested.
/// `qa_catalog_sdk::CustomPlanEntry` carries the three-way table.
///
/// # One shape it accepts that the list above does not mention
///
/// **An unknown key in the object form decodes `Ok`, silently dropped**: there is
/// no `#[serde(deny_unknown_fields)]`, so `[{"repo_id":…,"path":…,"zzz":1}]`
/// loads. That is forward-compatibility — a newer writer's extra field must not
/// fail-close an older reader — and it is not a regression, since the previous
/// tuple shape had no keys to be unknown. But the module header promises
/// fail-closed decoding, so the exception belongs written down rather than
/// inferred from the absence of an attribute. Note it is object-form only: a
/// **four**-element array still errors (`invalid length 4, expected fewer
/// elements in array`), because the sequence path has a fixed arity.
///
/// # The one directional cost, stated rather than implied
///
/// This is a **one-way** widening. Writes emit the object form, which a binary
/// from before this change cannot decode — so a rollback taken *after* a custom
/// plan has been written or updated leaves that row failing closed. Writing a
/// three-element array instead would not help; both were measured against the old
/// decoder, which was `serde_json::from_value::<Vec<(Uuid, String)>>` — the
/// function, not `from_str`, because the callee is what decides the error:
///
/// | payload | old `from_value` decoder |
/// |---|---|
/// | two-element array | `Ok` |
/// | three-element array | `Err("invalid length 3, expected fewer elements in array")` |
/// | object | `Err("invalid type: map, expected a tuple of size 2")` |
///
/// (An earlier version of this table gave the three-element row as
/// `"trailing characters"`. That is `from_str`'s error on the same input — a real
/// measurement of the wrong function. The conclusion was unaffected: neither new
/// shape is downgrade-safe.)
///
/// Nothing in this gear promises payload downgrade safety, and the object form is
/// the one a future field can extend without another compatibility clause; but
/// the cost is real and belongs written down where the codec is, not discovered
/// in a rollback. **It is asserted by nothing** — the old decoder no longer
/// exists in this tree to test against, so the table above is a measurement, not
/// a regression guard.
#[derive(serde::Serialize, serde::Deserialize)]
struct StoredCustomPlanEntry {
    repo_id: Uuid,
    path: String,
    #[serde(default)]
    plan_path: Option<String>,
}

impl From<StoredCustomPlanEntry> for CustomPlanEntry {
    fn from(stored: StoredCustomPlanEntry) -> Self {
        Self {
            repo_id: stored.repo_id,
            path: stored.path,
            plan_path: stored.plan_path,
        }
    }
}

impl From<&CustomPlanEntry> for StoredCustomPlanEntry {
    fn from(entry: &CustomPlanEntry) -> Self {
        Self {
            repo_id: entry.repo_id,
            path: entry.path.clone(),
            plan_path: entry.plan_path.clone(),
        }
    }
}

/// Decode `custom_plans.files` — see [`StoredCustomPlanEntry`] for the shapes
/// accepted and why more than one has to be.
///
/// # Errors
///
/// Returns `DomainError::Internal` if `value` is not an array of entries in one
/// of those shapes.
pub fn custom_plan_entries_from_json(
    value: &Value,
    column: &str,
    row_id: Uuid,
) -> Result<Vec<CustomPlanEntry>, DomainError> {
    let stored: Vec<StoredCustomPlanEntry> = serde_json::from_value(value.clone())
        .map_err(|e| DomainError::Internal(format!("corrupt {column} for id={row_id}: {e}")))?;
    Ok(stored.into_iter().map(CustomPlanEntry::from).collect())
}

/// Encode `custom_plans.files` for storage, in the object form
/// [`StoredCustomPlanEntry`] documents.
///
/// # Errors
///
/// Returns `DomainError::Internal` if serialization fails.
pub fn custom_plan_entries_to_json(entries: &[CustomPlanEntry]) -> Result<Value, DomainError> {
    let stored: Vec<StoredCustomPlanEntry> =
        entries.iter().map(StoredCustomPlanEntry::from).collect();
    serde_json::to_value(&stored)
        .map_err(|e| DomainError::Internal(format!("failed to encode custom-plan entries: {e}")))
}

/// Decode a JSON array of tag strings.
///
/// # Errors
///
/// Returns `DomainError::Internal` if `value` is not an array of strings.
pub fn tags_from_json(
    value: &Value,
    column: &str,
    row_id: Uuid,
) -> Result<Vec<String>, DomainError> {
    serde_json::from_value(value.clone())
        .map_err(|e| DomainError::Internal(format!("corrupt {column} for id={row_id}: {e}")))
}

/// Encode tag strings for storage.
///
/// # Errors
///
/// Returns `DomainError::Internal` if serialization fails.
pub fn tags_to_json(tags: &[String]) -> Result<Value, DomainError> {
    serde_json::to_value(tags)
        .map_err(|e| DomainError::Internal(format!("failed to encode tags: {e}")))
}

/// Narrow an unsigned contract value into the `BIGINT` column type.
///
/// # Errors
///
/// Returns `DomainError::Validation` if `value` exceeds `i64::MAX`. Unlike the
/// read direction this is caller input, not corrupt state, so it is a
/// validation failure rather than an internal error.
pub fn db_i64_from_u64(value: u64, field: &str) -> Result<i64, DomainError> {
    i64::try_from(value).map_err(|_| DomainError::Validation {
        field: field.to_owned(),
        message: format!("must not exceed {}", i64::MAX),
    })
}

/// Optional variant of [`db_i64_from_u64`].
///
/// # Errors
///
/// Returns `DomainError::Validation` if the value exceeds `i64::MAX`.
pub fn db_optional_i64_from_u64(
    value: Option<u64>,
    field: &str,
) -> Result<Option<i64>, DomainError> {
    value.map(|v| db_i64_from_u64(v, field)).transpose()
}

/// Widen a `BIGINT` column into its unsigned contract type, failing closed on
/// negative values.
fn u64_from_db(value: i64, column: &str, row_id: Uuid) -> Result<u64, DomainError> {
    u64::try_from(value).map_err(|_| {
        DomainError::Internal(format!(
            "corrupt {column} for id={row_id}: negative value {value}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(repo: u128, path: &str, plan_path: Option<&str>) -> CustomPlanEntry {
        CustomPlanEntry {
            repo_id: Uuid::from_u128(repo),
            path: path.to_owned(),
            plan_path: plan_path.map(str::to_owned),
        }
    }

    #[test]
    fn custom_plan_entries_round_trip() {
        let entries = vec![
            entry(1, "tests/a.py", Some("tests/plan.yaml")),
            entry(2, "tests/b.py", None),
        ];
        let json = custom_plan_entries_to_json(&entries).expect("encode");
        let decoded = custom_plan_entries_from_json(&json, "col", Uuid::nil()).expect("decode");
        assert_eq!(decoded, entries);
    }

    /// **The compatibility test for a stored payload, not for a type.**
    ///
    /// `plan_path` was added to `CustomPlanEntry` after the column shipped, so
    /// the rows already in `qa_custom_plans.files` hold the two-element array
    /// the old `(Uuid, String)` pair serialized to. This asserts against a
    /// **literal** of that payload rather than anything this module produces:
    /// a round-trip test cannot see this failure, because both of its halves
    /// would move together. If this reddens, every stored custom plan reads as
    /// `DomainError::Internal` and every launch targeting one fails.
    ///
    /// The three-element array is covered too, so a future writer that emits
    /// the compact form is not a compatibility break either.
    #[test]
    fn stored_pre_plan_path_rows_still_decode() {
        let repo_id = Uuid::from_u128(0x0B01);
        let old_shape = serde_json::json!([[repo_id.to_string(), "tests/a.py"]]);

        let decoded =
            custom_plan_entries_from_json(&old_shape, "qa_custom_plans.files", Uuid::nil())
                .expect("a row written before plan_path existed must still load");

        assert_eq!(decoded, vec![entry(0x0B01, "tests/a.py", None)]);

        let three = serde_json::json!([[repo_id.to_string(), "tests/a.py", "tests/plan.yaml"]]);
        let decoded = custom_plan_entries_from_json(&three, "qa_custom_plans.files", Uuid::nil())
            .expect("the compact three-element form must decode too");
        assert_eq!(
            decoded,
            vec![entry(0x0B01, "tests/a.py", Some("tests/plan.yaml"))]
        );
    }

    #[test]
    fn tags_round_trip() {
        let tags = vec!["smoke".to_owned(), "slow".to_owned()];
        let json = tags_to_json(&tags).expect("encode");
        let decoded = tags_from_json(&json, "col", Uuid::nil()).expect("decode");
        assert_eq!(decoded, tags);
    }

    #[test]
    fn corrupt_json_fails_closed_instead_of_reading_as_empty() {
        let garbage = serde_json::json!({"not": "an array"});

        let files = custom_plan_entries_from_json(&garbage, "qa_custom_plans.files", Uuid::nil());
        assert!(matches!(files, Err(DomainError::Internal(_))));

        let tags = tags_from_json(&garbage, "qa_custom_plans.tags", Uuid::nil());
        assert!(matches!(tags, Err(DomainError::Internal(_))));

        // A JSON array whose items have the wrong shape must fail too.
        let wrong_items = serde_json::json!(["just-a-string"]);
        let pairs =
            custom_plan_entries_from_json(&wrong_items, "qa_custom_plans.files", Uuid::nil());
        assert!(matches!(pairs, Err(DomainError::Internal(_))));

        // `#[serde(default)]` is on `plan_path` alone, so the *required* fields
        // stay fail-closed: a truncated array and an object missing `path` are
        // corruption, not an older shape. Without this, the compatibility clause
        // above would have quietly widened into "accept almost anything".
        let truncated = serde_json::json!([[Uuid::from_u128(1).to_string()]]);
        let pairs = custom_plan_entries_from_json(&truncated, "qa_custom_plans.files", Uuid::nil());
        assert!(matches!(pairs, Err(DomainError::Internal(_))));

        let missing_path = serde_json::json!([{"repo_id": Uuid::from_u128(1).to_string()}]);
        let pairs =
            custom_plan_entries_from_json(&missing_path, "qa_custom_plans.files", Uuid::nil());
        assert!(matches!(pairs, Err(DomainError::Internal(_))));

        // A four-element array is corruption too: the sequence path has a fixed
        // arity, which is what keeps the object form's permissiveness (below)
        // from leaking into the shape that pre-dates it.
        let too_long = serde_json::json!([[Uuid::from_u128(1).to_string(), "a.py", "p.yaml", "x"]]);
        let pairs = custom_plan_entries_from_json(&too_long, "qa_custom_plans.files", Uuid::nil());
        assert!(matches!(pairs, Err(DomainError::Internal(_))));
    }

    /// The one shape this decoder accepts that the fail-closed rule does not
    /// cover: an **unknown key in the object form**, which is dropped rather than
    /// rejected because there is no `#[serde(deny_unknown_fields)]`.
    ///
    /// Asserted rather than left implicit. The module header promises fail-closed
    /// decoding, so this exception is either documented and pinned or it is a
    /// silent hole; and if a future change adds `deny_unknown_fields`, that is a
    /// deliberate reversal of a forward-compatibility choice and should have to
    /// edit a test that says so.
    #[test]
    fn an_unknown_key_in_the_object_form_is_dropped_not_rejected() {
        let repo_id = Uuid::from_u128(0x0B01);
        let with_extra = serde_json::json!([{
            "repo_id": repo_id.to_string(),
            "path": "tests/a.py",
            "zzz_from_a_newer_writer": 1,
        }]);

        let decoded =
            custom_plan_entries_from_json(&with_extra, "qa_custom_plans.files", Uuid::nil())
                .expect("an unknown key must not fail-close an older reader");

        assert_eq!(decoded, vec![entry(0x0B01, "tests/a.py", None)]);
    }

    #[test]
    fn negative_bigint_fails_closed() {
        let err = u64_from_db(-1, "qa_test_bundles.size_bytes", Uuid::nil());
        assert!(matches!(err, Err(DomainError::Internal(_))));
    }

    #[test]
    fn oversized_unsigned_input_is_a_validation_error() {
        let err = db_i64_from_u64(u64::MAX, "timeout_seconds");
        assert!(matches!(err, Err(DomainError::Validation { .. })));
        assert_eq!(
            db_optional_i64_from_u64(None, "timeout_seconds").expect("none passes through"),
            None
        );
        assert_eq!(
            db_optional_i64_from_u64(Some(60), "timeout_seconds").expect("in-range value"),
            Some(60)
        );
    }
}
