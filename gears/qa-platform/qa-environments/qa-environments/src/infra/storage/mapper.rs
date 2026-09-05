//! Conversions between `SeaORM` entity models and SDK contract types.

use qa_environments_sdk::{Environment, EnvironmentCredential, LeaseState, Variable};
use qa_product_sdk::observation::{HealthState, ObservedAttrs};
use uuid::Uuid;

use super::entity::{environment, environment_lease, environment_variable, pipeline_variable};
use crate::domain::error::DomainError;

/// Convert an environment database entity to a contract model.
#[must_use]
pub fn environment_to_sdk(m: environment::Model) -> Environment {
    Environment {
        id: m.id,
        name: m.name,
        product_id: m.product_id,
        description: m.description,
        available: m.available,
        observed_version: m.observed_version,
        observed_build: m.observed_build,
        default_branch: m.default_branch,
        is_default: m.is_default,
        version_detect_error: m.version_detect_error,
        version_detected_at: m.version_detected_at,
        credentials: environment_credentials(m.id, m.credentials),
        observed_attrs: observed_attrs(m.id, m.observed_attrs),
        config: m.config,
        observed_base_url: m.observed_base_url,
        health_state: HealthState::from_str_or_unknown(&m.health_state),
        health_detail: m.health_detail,
        health_checked_at: m.health_checked_at,
        created_at: m.created_at,
        updated_at: m.updated_at,
    }
}

/// The persisted shape of one `qa_environments.credentials` entry.
///
/// `qa-environments-sdk` carries no serde derive of its own (the convention
/// its module header states), and `qa_product_sdk::plugin::CredentialSlot`
/// derives only `Debug` — deliberately, because it carries a `SecretValue`.
/// So the codec for this column lives here, beside the entity, and this is the
/// type it goes through.
///
/// **The two field names are load-bearing**: they are what
/// `m20260903_000011_environment_plugin_columns`' backfill writes, and
/// `the_credentials_backfill_is_byte_identical_to_what_serde_writes` compares
/// this type's `serde_json` output against the bytes that migration really
/// stored, so a rename here fails a test rather than silently orphaning every
/// backfilled row.
///
/// There is no `value` field, for [`EnvironmentCredential`]'s reason.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct StoredEnvironmentCredential {
    pub key: String,
    pub credstore_ref: String,
}

impl From<StoredEnvironmentCredential> for EnvironmentCredential {
    fn from(stored: StoredEnvironmentCredential) -> Self {
        Self {
            key: stored.key,
            credstore_ref: stored.credstore_ref,
        }
    }
}

impl From<&EnvironmentCredential> for StoredEnvironmentCredential {
    fn from(credential: &EnvironmentCredential) -> Self {
        Self {
            key: credential.key.clone(),
            credstore_ref: credential.credstore_ref.clone(),
        }
    }
}

/// Encode `qa_environments.credentials` — the inverse of
/// `environment_credentials` (private, hence not linked), added by Task 18b
/// when the column gained its first writer outside `m20260903_000011`'s
/// backfill.
///
/// It goes through [`StoredEnvironmentCredential`] rather than serialising
/// `EnvironmentCredential` directly, which is the whole reason the stored type
/// exists: the two field names it pins are the ones that migration's SQL
/// `jsonb_build_object` wrote, and
/// `the_credentials_backfill_is_byte_identical_to_what_serde_writes` compares
/// this type's output against those bytes. A write path that bypassed it could
/// store a shape the backfilled rows do not have.
///
/// # Why this cannot fail
///
/// `Vec<StoredEnvironmentCredential>` is a sequence of structs with two
/// `String` fields, so the only documented `to_value` failures — a map with
/// non-string keys, a `Serialize` impl that errors, a non-finite float — are
/// all unreachable. The fallback is an empty array rather than a panic
/// anyway: losing a credential reference is recoverable by re-saving the
/// environment, and this runs inside a create/update the caller is waiting on.
pub fn credentials_to_json(credentials: &[EnvironmentCredential]) -> serde_json::Value {
    let stored: Vec<StoredEnvironmentCredential> = credentials
        .iter()
        .map(StoredEnvironmentCredential::from)
        .collect();
    serde_json::to_value(stored).unwrap_or_else(|error| {
        tracing::warn!(
            error_category = ?error.classify(),
            "qa-environments: a resolved credential list did not serialise; storing an \
             empty list rather than failing the write"
        );
        serde_json::json!([])
    })
}

/// Decode `qa_environments.credentials`.
///
/// An unparseable blob degrades to an empty list and is logged, following the
/// precedent `cluster_nodes` set in the deleted `cluster_health_view` (Task 19)
/// rather than qa-catalog's
/// erroring `custom_plan_entries_from_json`: this column is machine-written,
/// and failing the whole mapping would take the environment list down for
/// every caller over one corrupt row.
///
/// # The log line carries no part of the blob, and `%error` would have
///
/// `serde_json::Error`'s `Display` **quotes the offending value**: a blob of
/// `["credstore://kc/prod"]` fails as `invalid type: string
/// "credstore://kc/prod", expected struct StoredEnvironmentCredential`, the
/// whole string, untruncated (`serde_json`'s `de.rs` maps
/// `Value::String(s) => Unexpected::Str(s)`, and `serde` formats that as
/// `string "{s:?}"`). This column carries credstore references, and a
/// reference is a read path to the material under `SharingMode::Tenant` —
/// which is exactly why `EnvironmentDto` withholds it. So what is logged is
/// the failure's **category** and position, never its message: enough to
/// know a row is corrupt and where, with nothing quoted from it.
///
/// This is the same rule as `PluginFailure::detail`'s `&'static str`, applied
/// to a log line instead of a wire field.
fn environment_credentials(
    environment_id: Uuid,
    value: serde_json::Value,
) -> Vec<EnvironmentCredential> {
    let stored: Vec<StoredEnvironmentCredential> = match serde_json::from_value(value) {
        Ok(stored) => stored,
        Err(error) => {
            tracing::warn!(
                environment_id = %environment_id,
                error_category = ?error.classify(),
                error_line = error.line(),
                error_column = error.column(),
                "qa-environments: this environment's stored credentials blob does not \
                 parse as a credential list; reading it as empty rather than failing the \
                 whole mapping"
            );
            Vec::new()
        }
    };
    stored
        .into_iter()
        .map(EnvironmentCredential::from)
        .collect()
}

/// Decode `qa_environments.observed_attrs`.
///
/// [`ObservedAttrs`] is stored as itself — `#[serde(transparent)]` over a
/// `BTreeMap<String, String>` — so there is no parallel type here to keep in
/// step. Degrades to empty and logs, for [`environment_credentials`]' reason,
/// and logs a **category** rather than the failure's message for that
/// function's other reason: everything in this column is non-secret by
/// construction (`retain_declared` plus a schema that may declare no secret
/// kind), but a *corrupt* blob is by definition not that shape, so what it
/// holds is unknown and `serde_json::Error`'s `Display` quotes it.
fn observed_attrs(environment_id: Uuid, value: serde_json::Value) -> ObservedAttrs {
    serde_json::from_value(value).unwrap_or_else(|error| {
        tracing::warn!(
            environment_id = %environment_id,
            error_category = ?error.classify(),
            error_line = error.line(),
            error_column = error.column(),
            "qa-environments: this environment's stored observed_attrs blob does not \
             parse as a string map; reading it as empty rather than failing the whole \
             mapping"
        );
        ObservedAttrs::default()
    })
}

/// Convert a per-environment variable database entity to a contract model.
#[must_use]
pub fn environment_var_to_sdk(m: environment_variable::Model) -> Variable {
    Variable {
        id: m.id,
        environment_id: Some(m.environment_id),
        name: m.name,
        value: m.value,
    }
}

/// Convert a pipeline (global) variable database entity to a contract model.
#[must_use]
pub fn pipeline_var_to_sdk(m: pipeline_variable::Model) -> Variable {
    Variable {
        id: m.id,
        environment_id: None,
        name: m.name,
        value: m.value,
    }
}

/// holders JSON + mode column → `LeaseState`.
///
/// Fail-closed: corrupt state blocks dispatch loudly rather than reading as
/// Free. An unrecognized `mode` value, or an `"exclusive"` row that doesn't
/// carry exactly one holder, indicates a corrupt row (bug, manual edit, or
/// schema drift) and must error rather than silently reinterpret as
/// "available for a new run" — a false `Free` reading would let a second run
/// acquire an environment another run believes it holds exclusively. `"free"`
/// (or `"parallel"` with an empty holders array) is a legitimate,
/// intentionally-written free state and maps to `Free` without error.
///
/// # The decode must fail closed, and once did not
///
/// `holders` was decoded with `unwrap_or_default()` until 2026-08-13. That
/// turned **any** unparseable value into an empty vec, which then matched the
/// `("parallel", [])` arm and returned `LeaseState::Free` — the single most
/// dangerous answer this function can give, on the row that arbitrates
/// exclusive access to a shared physical environment. The paragraph above already
/// promised otherwise, so the doc was false and the failure was open at the
/// same time. Found by the security review of qa-runs Task 9.
///
/// The distinction that makes the fix non-obvious: `("parallel", [])` really is
/// legitimate — a holder list that decoded successfully and was empty means the
/// environment is free, and `the_empty_parallel_holder_list_is_a_real_free_state`
/// pins that. So "decoded to empty" and "failed to decode" must be told apart,
/// which is exactly what `unwrap_or_default()` erases. Use `?`, never a
/// fallback.
///
/// # Errors
///
/// Returns `DomainError::Internal` if `m.holders` is not a JSON array of
/// UUIDs, if `m.mode` is not one of `"free"`, `"parallel"`, or `"exclusive"`,
/// or if `m.mode == "exclusive"` but `m.holders` does not contain exactly one
/// holder — all three indicate a corrupt row rather than a legitimate lease
/// state.
pub fn lease_to_state(m: &environment_lease::Model) -> Result<LeaseState, DomainError> {
    let holders: Vec<Uuid> = serde_json::from_value(m.holders.clone()).map_err(|e| {
        DomainError::Internal(format!(
            "corrupt lease row: holders is not a JSON array of UUIDs for \
             platform_id (the environment's id)={}: {e}",
            m.environment_id
        ))
    })?;
    match (m.mode.as_str(), holders.as_slice()) {
        ("free", _) | ("parallel", []) => Ok(LeaseState::Free),
        ("parallel", _) => Ok(LeaseState::HeldParallel { holders }),
        ("exclusive", [holder]) => Ok(LeaseState::HeldExclusive { holder: *holder }),
        (mode, hs) => Err(DomainError::Internal(format!(
            "corrupt lease row: unknown mode {mode:?} with {} holder(s) for \
             platform_id (the environment's id)={}",
            hs.len(),
            m.environment_id
        ))),
    }
}

/// `LeaseState` → (mode, holders json) column pair.
#[must_use]
pub fn state_to_columns(state: &LeaseState) -> (String, serde_json::Value) {
    match state {
        LeaseState::Free => ("free".into(), serde_json::json!([])),
        LeaseState::HeldParallel { holders } => (
            "parallel".into(),
            serde_json::to_value(holders).unwrap_or_default(),
        ),
        LeaseState::HeldExclusive { holder } => ("exclusive".into(), serde_json::json!([holder])),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use time::OffsetDateTime;

    use super::*;

    /// An environment row with **every nullable column populated**, which is the
    /// entire point: a `None` survives a dropped field unchanged, so a fixture
    /// full of `None`s proves nothing about the mapper.
    fn full_environment_row() -> environment::Model {
        environment::Model {
            id: Uuid::from_u128(1),
            tenant_id: Uuid::from_u128(2),
            name: "staging-a".to_owned(),
            product_id: Uuid::from_u128(3),
            description: Some("desc".to_owned()),
            available: true,
            observed_version: Some("5.0.1".to_owned()),
            observed_build: Some("20260813".to_owned()),
            default_branch: Some("release-9.0".to_owned()),
            // `true`, not the column default: a mapper that dropped this read
            // and left `bool::default()` matched a `false` fixture (re-review,
            // found while closing N-7/N-8).
            is_default: true,
            version_detect_error: Some("namespaces \"virtuozzo\" not found".to_owned()),
            version_detected_at: Some(OffsetDateTime::from_unix_timestamp(1_786_579_250).unwrap()),
            // The plugin-shaped half. Every value here differs from its legacy
            // twin above -- `observed_base_url` from `vhp_base_url`,
            // `health_state` from `cluster_status`, `health_checked_at` from
            // `cluster_checked_at` -- so a mapper that read the legacy column
            // into the new field, or the other way round, cannot pass.
            credentials: serde_json::json!([
                {"key": "kubeconfig", "credstore_ref": "credstore://plugin-ref"}
            ]),
            observed_attrs: serde_json::json!({
                "platformVersion": "5.0",
                "build": "20260814",
                "baseDomain": "https://plugin.jele.io",
            }),
            config: serde_json::json!({"vpadm_namespace": "vzt"}),
            observed_base_url: Some("https://plugin.jele.io".to_owned()),
            health_state: "degraded".to_owned(),
            health_detail: Some("one node not ready".to_owned()),
            health_checked_at: Some(OffsetDateTime::from_unix_timestamp(1_786_579_270).unwrap()),
            created_at: OffsetDateTime::from_unix_timestamp(1_786_579_200).unwrap(),
            updated_at: OffsetDateTime::from_unix_timestamp(1_786_579_300).unwrap(),
        }
    }

    /// Every column `environment_to_sdk` reads must reach the SDK model.
    ///
    /// # Why this test exists, and why it is not about `default_branch`
    ///
    /// Task 9b added `observed_build` after finding that **no DB-backed fixture in
    /// this gear ever wrote a non-`NULL` `observed_version`**, so a value dropped
    /// on the way out was invisible to every test. It closed the fixture half but
    /// never added the assertion, and Task 13b's review then measured what that
    /// left behind: mutating this function to drop `product_id`, `description` or
    /// `observed_version` in turn left **every one of the gear's then-72 tests
    /// green**. Only `observed_build` (1 red) and `default_branch` (3 red) were
    /// covered. Re-measured after adding the two tests below, each of the five
    /// nullable columns now turns at least one test red on its own.
    ///
    /// So this gear's storage mapper could silently stop returning three shipped
    /// columns — including the very one Task 9b existed to protect — and nothing
    /// would notice. `api/rest/dto.rs`'s
    /// `environment_dto_from_sdk_carries_every_field_it_publishes` looks like the
    /// guard but is not: it covers `Environment → EnvironmentDto`, one layer
    /// further out, never touches this function, and — since `EnvironmentDto` stopped
    /// publishing `kubeconfig_credstore_ref` — does not even cover every field of
    /// the SDK model.
    ///
    /// Break-tested per field: replacing any one of `environment_to_sdk`'s **fifteen
    /// scalar** reads turns at least one of this pair red — verified field by
    /// field, all fifteen. That per-field property is what makes it a guard rather
    /// than a smoke test; asserting only the fields someone remembered is how the
    /// hole was left in the first place.
    ///
    /// The claim needed the companion test to stop using `..full_environment_row()`.
    /// While it shared this fixture's `id`, `name`, `kubeconfig_credstore_ref`,
    /// `available` and timestamps, a literal substituted for any of those six
    /// matched in *both* tests and stayed green — so an earlier version of this
    /// sentence was false for six of the reads it claimed. See
    /// `environment_to_sdk_preserves_absent_optionals`.
    ///
    /// **Eighteen scalar reads plus two codec-mediated ones, not fifteen** —
    /// recounted after Task 19 dropped the eight legacy platform columns and
    /// Tasks 14/20b added the plugin-shaped ones. `environment::Model` has
    /// twenty-one columns; `tenant_id` is the one deliberately not mapped
    /// (tenancy is enforced by the scope on every query, never carried in a
    /// contract type), leaving twenty. `credentials` and `observed_attrs` go
    /// through codecs and are asserted separately below, so eighteen scalar
    /// reads are the rest of the surface. Keeping this count current is exactly
    /// the discipline the earlier corrections ("eleven, not ten"; "fifteen, not
    /// eleven") were about — the recount is what found `is_default` mapped and
    /// unasserted.
    ///
    /// Added by Task 13b at the coordinator's request; the gap it closes is
    /// Task 9b's, not Task 13b's.
    ///
    /// A paragraph here used to describe a `cluster: Option<ClusterHealthView>`
    /// field folded from five `cluster_*` columns by `cluster_health_view`.
    /// Task 19 dropped those columns and Task 21 deleted the function; a
    /// product's own facts reach the contract through `observed_attrs`, and its
    /// verdict through `health_state`/`health_detail`/`health_checked_at`, all
    /// asserted below (re-review, N-7/N-8).
    #[test]
    #[allow(
        clippy::cognitive_complexity,
        reason = "a flat list of one assertion per column/field, the exact shape this test's \
                  own doc says a guard like this needs; splitting it into helper functions \
                  would not reduce what it checks, only how it reads"
    )]
    fn environment_to_sdk_preserves_every_column() {
        let row = full_environment_row();
        let sdk = environment_to_sdk(row.clone());

        assert_eq!(sdk.id, row.id, "id");
        assert_eq!(sdk.name, row.name, "name");
        assert_eq!(sdk.product_id, row.product_id, "product_id");
        assert_eq!(sdk.description, row.description, "description");
        assert_eq!(sdk.available, row.available, "available");
        assert_eq!(
            sdk.observed_version, row.observed_version,
            "observed_version -- the column Task 9b existed to protect, and the \
             one this assertion was missing for"
        );
        assert_eq!(sdk.observed_build, row.observed_build, "observed_build");
        assert_eq!(sdk.default_branch, row.default_branch, "default_branch");
        assert_eq!(sdk.is_default, row.is_default, "is_default");
        assert_eq!(
            sdk.version_detect_error, row.version_detect_error,
            "version_detect_error"
        );
        assert_eq!(
            sdk.version_detected_at, row.version_detected_at,
            "version_detected_at"
        );
        assert_eq!(sdk.created_at, row.created_at, "created_at");
        assert_eq!(sdk.updated_at, row.updated_at, "updated_at");

        // The five plugin-shaped scalar reads, added by Task 14.
        assert_eq!(sdk.config, row.config, "config");
        assert_eq!(
            sdk.observed_base_url, row.observed_base_url,
            "observed_base_url -- and NOT vhp_base_url, which this fixture \
             deliberately gives a different value"
        );
        assert_eq!(
            sdk.health_state,
            HealthState::Degraded,
            "health_state must come back through HealthState::from_str_or_unknown, \
             not as Unknown"
        );
        assert_eq!(sdk.health_detail, row.health_detail, "health_detail");
        assert_eq!(
            sdk.health_checked_at, row.health_checked_at,
            "health_checked_at -- and NOT cluster_checked_at"
        );

        // The two codec-mediated ones.
        assert_eq!(
            sdk.credentials,
            vec![EnvironmentCredential {
                key: "kubeconfig".to_owned(),
                credstore_ref: "credstore://plugin-ref".to_owned(),
            }],
            "credentials -- decoded from the JSONB, and carrying the plugin-shaped \
             reference rather than kubeconfig_credstore_ref"
        );
        let mut expected_attrs = ObservedAttrs::default();
        expected_attrs.set("platformVersion", "5.0");
        expected_attrs.set("build", "20260814");
        expected_attrs.set("baseDomain", "https://plugin.jele.io");
        assert_eq!(sdk.observed_attrs, expected_attrs, "observed_attrs");

        // `tenant_id` is deliberately absent from `Environment`: tenancy is
        // enforced by the scope on every query, not carried in the contract type.
        // Stated so its absence above reads as intentional rather than forgotten.
    }

    /// The nullable columns must also survive as `None`, so an environment with
    /// nothing observed and no override does not acquire phantom values.
    ///
    /// The companion to the test above: that one would pass if the mapper
    /// substituted a constant for every nullable field, this one would not.
    ///
    /// # Every non-optional value here differs from `full_environment_row`'s
    ///
    /// This row is built field-by-field rather than with `..full_environment_row()`,
    /// and that is load-bearing. Sharing the populated fixture's `id`, `name`,
    /// `kubeconfig_credstore_ref`, `available` and timestamps meant a mapper that
    /// replaced any of those six reads with the fixture's own literal stayed
    /// **green in both tests** — so the sibling's claim to cover "any one of the
    /// fifteen reads" was false for exactly those six. Distinct values here make
    /// the two tests disagree on every field, which is what the claim needs.
    #[test]
    #[allow(
        clippy::cognitive_complexity,
        reason = "the same flat one-assertion-per-field list as its sibling above, and \
                  allowed for the same reason: Task 14's seven columns pushed it over \
                  the threshold, and splitting the list into helper functions would not \
                  reduce what it checks, only how it reads"
    )]
    fn environment_to_sdk_preserves_absent_optionals() {
        let row = environment::Model {
            id: Uuid::from_u128(11),
            tenant_id: Uuid::from_u128(12),
            name: "staging-b".to_owned(),
            // Required since Task 20b -- there is no absent-optional case for
            // this column any more, which is why it is not in the list below.
            product_id: Uuid::from_u128(0x9001),
            description: None,
            available: false,
            observed_version: None,
            observed_build: None,
            default_branch: None,
            is_default: false,
            version_detect_error: None,
            version_detected_at: None,
            // At their column defaults, which is what a never-observed
            // environment really holds.
            credentials: serde_json::json!([]),
            observed_attrs: serde_json::json!({}),
            config: serde_json::json!({}),
            observed_base_url: None,
            health_state: "unknown".to_owned(),
            health_detail: None,
            health_checked_at: None,
            created_at: OffsetDateTime::from_unix_timestamp(1_786_600_000).unwrap(),
            updated_at: OffsetDateTime::from_unix_timestamp(1_786_600_100).unwrap(),
        };
        let sdk = environment_to_sdk(row.clone());

        // The non-optional fields, asserted here too: with values distinct from
        // `full_environment_row`'s, these are what close the six-field hole above.
        assert_eq!(sdk.id, row.id, "id");
        assert_eq!(sdk.name, row.name, "name");
        assert_eq!(sdk.available, row.available, "available");
        assert_eq!(sdk.created_at, row.created_at, "created_at");
        assert_eq!(sdk.updated_at, row.updated_at, "updated_at");

        assert_eq!(sdk.description, None, "description");
        assert_eq!(sdk.observed_version, None, "observed_version");
        assert_eq!(sdk.observed_build, None, "observed_build");
        assert_eq!(sdk.default_branch, None, "default_branch");
        assert_eq!(sdk.version_detect_error, None, "version_detect_error");
        assert_eq!(sdk.version_detected_at, None, "version_detected_at");

        assert_eq!(sdk.observed_base_url, None, "observed_base_url");
        assert_eq!(
            sdk.health_state,
            HealthState::Unknown,
            "the default column value must read back as Unknown"
        );
        assert_eq!(sdk.health_detail, None, "health_detail");
        assert_eq!(
            sdk.health_checked_at, None,
            "health_checked_at stays None -- 'nothing ever looked' is the only \
             honest value, and the only thing separating it from a failed look"
        );
        assert!(sdk.credentials.is_empty(), "credentials");
        assert!(
            sdk.observed_attrs.is_empty(),
            "observed_attrs -- an empty map, not a map of empty strings"
        );
        assert_eq!(sdk.config, serde_json::json!({}), "config");
    }

    /// [`credentials_to_json`] and [`environment_credentials`] are inverses,
    /// and what the writer emits is the shape the migration's backfill wrote.
    ///
    /// Task 18b added the writer; until then the column's only writer was
    /// `m20260903_000011`'s SQL, and
    /// `the_credentials_backfill_is_byte_identical_to_what_serde_writes` pinned
    /// that against `StoredEnvironmentCredential`. This pins the other
    /// direction: a row this gear writes must be readable by the same codec,
    /// and must carry the same two field names, or the write path and the
    /// backfilled rows would disagree about the shape of the column.
    #[test]
    fn credentials_round_trip_through_the_column_codec() {
        let credentials = vec![
            EnvironmentCredential {
                key: "kubeconfig".to_owned(),
                credstore_ref: "credstore://kc/prod".to_owned(),
            },
            EnvironmentCredential {
                key: "api_token".to_owned(),
                credstore_ref: "credstore://tokens/prod".to_owned(),
            },
        ];

        let encoded = credentials_to_json(&credentials);
        assert_eq!(
            encoded,
            serde_json::json!([
                {"key": "kubeconfig", "credstore_ref": "credstore://kc/prod"},
                {"key": "api_token", "credstore_ref": "credstore://tokens/prod"},
            ]),
            "the two field names are the migration's, and order is preserved"
        );
        assert_eq!(
            environment_credentials(Uuid::nil(), encoded),
            credentials,
            "and the codec reads back exactly what it wrote"
        );
    }

    /// An empty list round-trips as `[]`, which is the value that means "fall
    /// back to the pre-plugin column" — so it must not become `null`.
    #[test]
    fn no_credentials_encodes_as_an_empty_array_not_null() {
        let encoded = credentials_to_json(&[]);
        assert_eq!(
            encoded,
            serde_json::json!([]),
            "`null` would take the corrupt-blob path on the way back in, and              the column is NOT NULL"
        );
        assert!(environment_credentials(Uuid::nil(), encoded).is_empty());
    }

    /// **Review finding I-8.** The corrupt arm must be *distinguishable* from
    /// the legitimately-empty one, and the only thing that distinguishes them
    /// is the `warn`.
    ///
    /// Both arms produce an empty `Vec<EnvironmentCredential>`, and since Task
    /// 18b both readers treat an empty `credentials` as "fall back to the
    /// pre-plugin column". After Task 19 drops that column an empty
    /// `credentials` will mean "this environment has no credentials", so a
    /// corrupt blob reading as empty is a silent claim that a credential does
    /// not exist. Task 19's warning listed this as a thing to *decide* rather
    /// than inherit; the decision is "keep the degrade, and make the log line
    /// the discriminator". This is what holds that decision to the code —
    /// before it, nothing did, and the task's contribution had been a comment.
    ///
    /// The environment id is asserted present because it is the only thing an
    /// operator can act on: it names the row to re-save.
    #[test]
    fn a_corrupt_credentials_blob_warns_and_a_legitimately_empty_one_does_not() {
        use std::sync::{Arc, Mutex};
        use tracing_subscriber::fmt::MakeWriter;

        #[derive(Clone)]
        struct Buffer(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Buffer {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> MakeWriter<'a> for Buffer {
            type Writer = Self;
            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let render = |blob: serde_json::Value| -> String {
            let buffer = Buffer(Arc::new(Mutex::new(Vec::new())));
            let subscriber = tracing_subscriber::fmt()
                .with_writer(buffer.clone())
                .with_ansi(false)
                .finish();
            let id = Uuid::from_u128(0x5150);
            {
                let _guard = tracing::subscriber::set_default(subscriber);
                let credentials = environment_credentials(id, blob);
                assert!(credentials.is_empty(), "both arms read as empty");
            }
            String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap()
        };

        let corrupt = render(serde_json::json!("not-an-array"));
        assert!(
            corrupt.contains("does not parse as a credential list"),
            "a corrupt blob must warn, or it is indistinguishable from an \
             environment that genuinely has no credentials: {corrupt}"
        );
        assert!(
            corrupt.contains("00000000-0000-0000-0000-000000005150"),
            "and the warn must name the row an operator has to re-save: {corrupt}"
        );
        assert!(
            !corrupt.contains("not-an-array"),
            "but never any part of the blob itself -- this column carries \
             credstore references: {corrupt}"
        );

        let empty = render(serde_json::json!([]));
        assert!(
            empty.is_empty(),
            "a legitimately empty column must be silent, or the warn \
             discriminates nothing: {empty}"
        );
    }

    /// The same degrade rule for the two blobs Task 14 added, and for the same
    /// reason: both columns are machine-written, so a corrupt one is a corrupt
    /// row -- and failing the whole mapping would take the environment list
    /// down for every caller over one of them. qa-catalog's
    /// `custom_plan_entries_from_json` errors instead, because *its* column
    /// holds an operator's plan and silently dropping it would run the wrong
    /// tests.
    ///
    /// A stored `null` is included deliberately: `serde_json::from_value`
    /// rejects it for both target types, so it takes the same path as
    /// structural garbage rather than the `Default` path it would take for an
    /// `Option`.
    #[test]
    fn an_unparseable_credentials_blob_degrades_to_an_empty_list() {
        for blob in [
            serde_json::json!("not-an-array"),
            serde_json::json!([{"key": "kubeconfig"}]),
            serde_json::json!([{"key": "kubeconfig", "credstore_ref": 7}]),
            serde_json::Value::Null,
        ] {
            let row = environment::Model {
                credentials: blob.clone(),
                ..full_environment_row()
            };
            let sdk = environment_to_sdk(row);
            assert!(
                sdk.credentials.is_empty(),
                "{blob} must read as no credentials rather than panic or propagate"
            );
        }
    }

    #[test]
    fn an_unparseable_observed_attrs_blob_degrades_to_an_empty_map() {
        for blob in [
            serde_json::json!("not-an-object"),
            serde_json::json!({"platformVersion": 5.0}),
            serde_json::json!([]),
            serde_json::Value::Null,
        ] {
            let row = environment::Model {
                observed_attrs: blob.clone(),
                ..full_environment_row()
            };
            let sdk = environment_to_sdk(row);
            assert!(
                sdk.observed_attrs.is_empty(),
                "{blob} must read as no attributes rather than panic or propagate"
            );
            assert_eq!(
                sdk.observed_version.as_deref(),
                Some("5.0.1"),
                "and the projected columns beside it must be unaffected: they are \
                 their own columns, not derived from this blob at read time"
            );
        }
    }

    /// `environment_var_to_sdk` must carry all four of the columns it reads.
    ///
    /// # Why this test exists
    ///
    /// This function had **no test at all** until Task 13d. Mutating
    /// `value: m.value` to `String::new()` left all 77 of the gear's tests green
    /// — measured, not assumed — so every environment variable could have come back
    /// with an empty value and nothing would have noticed. That failure is
    /// especially quiet for a variable: an empty value is a legal write —
    /// `validate_value` bounds the length and never requires content, and only
    /// `name` is checked for emptiness (`domain/service/variables.rs:303-309`,
    /// and `:231`) — so an emptied read is indistinguishable from one an operator
    /// set to empty deliberately, and it surfaces as a test run behaving oddly
    /// rather than as an error anywhere.
    ///
    /// Two reasons nothing caught it, and only the first is about wiring. The
    /// mocks that cover the variables *service* "never touch the database"
    /// (`domain/service/test_support.rs:48-51`), so this function is not on their
    /// path at all. `tests_tenant_scoping` does reach it, but only incidentally:
    /// measured field by field, of the **eight** fields these two functions
    /// populate — seven of them column reads, plus `pipeline_var_to_sdk`'s
    /// constant `environment_id: None` — exactly one was covered by an existing
    /// test: `pipeline_var_to_sdk`'s `name`, which reddens
    /// `variables_scoped_by_tenant`. The other seven, including both `value`s,
    /// both `id`s, both `environment_id`s and `environment_var_to_sdk`'s own `name`,
    /// turned nothing red. This test and its sibling below cover all eight, and
    /// each of the eight was break-tested individually.
    ///
    /// # The asymmetry, and the values chosen to pin it
    ///
    /// `environment_id` is the only field on which this function and
    /// `pipeline_var_to_sdk` differ, and so the one a copy-paste between them
    /// would get wrong. An environment variable is scoped to an environment and must
    /// report `Some`; a pipeline variable is global and must report `None`.
    ///
    /// Two consequences for the fixture, both deliberate: `id` and `environment_id`
    /// are distinct values, so a mapper reading `Some(m.id)` for `environment_id` is
    /// caught rather than matching by accident; and every value here differs from
    /// `pipeline_var_to_sdk_preserves_every_column_and_has_no_environment`'s, so the
    /// two tests disagree on every field they share. That is the property the
    /// environment mapper's sibling pair had to be rebuilt to get (see
    /// `environment_to_sdk_preserves_absent_optionals`).
    ///
    /// `tenant_id`, `created_at` and `updated_at` are columns on the row that
    /// `Variable` does not carry; as with `environment_to_sdk`, tenancy is enforced
    /// by the scope on every query rather than in the contract type.
    #[test]
    fn environment_var_to_sdk_preserves_every_column_and_names_its_environment() {
        let row = environment_variable::Model {
            id: Uuid::from_u128(31),
            tenant_id: Uuid::from_u128(32),
            environment_id: Uuid::from_u128(33),
            name: "REGION".to_owned(),
            value: "eu-west-1".to_owned(),
            created_at: OffsetDateTime::from_unix_timestamp(1_786_700_000).unwrap(),
            updated_at: OffsetDateTime::from_unix_timestamp(1_786_700_100).unwrap(),
        };
        let sdk = environment_var_to_sdk(row.clone());

        assert_eq!(sdk.id, row.id, "id");
        assert_eq!(sdk.name, row.name, "name");
        assert_eq!(
            sdk.value, row.value,
            "value -- the read whose loss the whole suite could not see"
        );
        assert_eq!(
            sdk.environment_id,
            Some(row.environment_id),
            "environment_id must be Some for a per-environment variable, and must be \
             the environment's id rather than the variable's own"
        );
    }

    /// The pipeline half of the pair above: the same four fields, three of them
    /// column reads, and the fourth the one that must differ.
    ///
    /// Also untested until Task 13d. `environment_id: None` is not an omission but
    /// the contract — a pipeline variable is global, and a `Some` here would
    /// misreport it as scoped to whichever environment the value came from. Since
    /// `pipeline_variable::Model` has no `environment_id` column, the shape a
    /// copy-paste from `environment_var_to_sdk` actually produces is `Some(m.id)`,
    /// which the `None` assertion below catches.
    #[test]
    fn pipeline_var_to_sdk_preserves_every_column_and_has_no_environment() {
        let row = pipeline_variable::Model {
            id: Uuid::from_u128(41),
            tenant_id: Uuid::from_u128(42),
            name: "ARTIFACT_ROOT".to_owned(),
            value: "s3://bucket/prefix".to_owned(),
            created_at: OffsetDateTime::from_unix_timestamp(1_786_800_000).unwrap(),
            updated_at: OffsetDateTime::from_unix_timestamp(1_786_800_100).unwrap(),
        };
        let sdk = pipeline_var_to_sdk(row.clone());

        assert_eq!(sdk.id, row.id, "id");
        assert_eq!(sdk.name, row.name, "name");
        assert_eq!(sdk.value, row.value, "value");
        assert_eq!(
            sdk.environment_id, None,
            "environment_id must be None for a pipeline (global) variable -- the \
             one field that distinguishes this mapper from environment_var_to_sdk"
        );
    }

    fn lease(mode: &str, holders: serde_json::Value) -> environment_lease::Model {
        environment_lease::Model {
            environment_id: Uuid::from_u128(1),
            tenant_id: Uuid::from_u128(2),
            mode: mode.to_owned(),
            holders,
            version: 3,
            updated_at: OffsetDateTime::from_unix_timestamp(1_786_579_200).unwrap(),
        }
    }

    /// The regression this module's fix exists for, and the **only** arm that
    /// was ever exploitable.
    ///
    /// Every value below decoded to an empty vec under `unwrap_or_default()`,
    /// matched the `("parallel", [])` arm, and reported the environment **free** —
    /// letting a second run acquire an environment another run believes it holds
    /// exclusively. They must now all error.
    ///
    /// Worth knowing before writing a companion test for the `"exclusive"` arm:
    /// **there is nothing there to catch.** A corrupt `exclusive` row also
    /// decoded to `[]`, which does not match the single-element slice pattern
    /// `("exclusive", [holder])`, so it fell through to the catch-all and
    /// errored even under the old code — with a misleading message blaming the
    /// mode rather than the holders, but it errored. A test asserting "a
    /// corrupt exclusive lease never reads as Free" therefore passes with and
    /// without the fix and pins nothing; one was written here and deleted after
    /// the break-test showed it green on both sides. `mode` is what
    /// discriminates, so only the arm whose *empty* form is legitimate could be
    /// fooled.
    #[test]
    fn a_holders_value_that_is_not_an_array_of_uuids_is_corrupt_not_free() {
        for holders in [
            serde_json::json!("not-an-array"),
            serde_json::json!({}),
            serde_json::json!(null),
            serde_json::json!([1, 2, 3]),
            serde_json::json!(["not-a-uuid"]),
        ] {
            let err = lease_to_state(&lease("parallel", holders.clone()))
                .expect_err("a holders value that does not decode must never read as Free");
            assert!(
                matches!(err, DomainError::Internal(_)),
                "corrupt holders must be Internal, got {err:?} for {holders}"
            );
        }
    }

    /// The distinction the fix had to preserve: an empty list that **decoded**
    /// is a real free state, not corruption. Deleting this test would let
    /// someone "harden" the function into rejecting a legitimate row and block
    /// every dispatch.
    #[test]
    fn the_empty_parallel_holder_list_is_a_real_free_state() {
        assert_eq!(
            lease_to_state(&lease("parallel", serde_json::json!([]))).unwrap(),
            LeaseState::Free
        );
        assert_eq!(
            lease_to_state(&lease("free", serde_json::json!([]))).unwrap(),
            LeaseState::Free
        );
    }

    #[test]
    fn well_formed_leases_still_decode() {
        let a = Uuid::from_u128(10);
        let b = Uuid::from_u128(11);
        assert_eq!(
            lease_to_state(&lease("parallel", serde_json::json!([a, b]))).unwrap(),
            LeaseState::HeldParallel {
                holders: vec![a, b]
            }
        );
        assert_eq!(
            lease_to_state(&lease("exclusive", serde_json::json!([a]))).unwrap(),
            LeaseState::HeldExclusive { holder: a }
        );
    }

    /// The two pre-existing corruption modes, kept covered so the `?` rewrite
    /// above did not quietly swallow them.
    #[test]
    fn an_unknown_mode_and_a_multi_holder_exclusive_lease_are_corrupt() {
        let a = Uuid::from_u128(10);
        let b = Uuid::from_u128(11);
        assert!(lease_to_state(&lease("bogus", serde_json::json!([]))).is_err());
        assert!(lease_to_state(&lease("exclusive", serde_json::json!([a, b]))).is_err());
        assert!(lease_to_state(&lease("exclusive", serde_json::json!([]))).is_err());
    }

    /// The **write** half of the lease codec, which nothing asserted until
    /// Task 13d. This is the one that matters in this file.
    ///
    /// # The failure it catches, traced to its consequence
    ///
    /// Mutating `state_to_columns` to write `LeaseState::HeldExclusive` as
    /// `mode: "parallel"` left all 77 of the gear's tests green — measured, not
    /// assumed. The consequence is not a cosmetically wrong string in a column.
    /// `lease_to_state` maps `("parallel", [h])` to `HeldParallel { holders: [h] }`,
    /// and `decide_acquire` grants `HeldParallel + Parallel -> Acquired`
    /// (`domain/lease.rs:34-43`). So the next run to ask for the environment in
    /// parallel mode **joins one another run holds exclusively** — the same
    /// outcome `lease_to_state`'s doc above calls "the single most dangerous
    /// answer this function can give", reached from the write side instead of
    /// the read side.
    ///
    /// The read half carries four tests and a fix-history section. The write half
    /// carried none, though `state_to_columns` is the sole producer of these two
    /// columns in the gear's write path, feeding both of `compare_and_set`'s
    /// branches — the version-0 insert and the CAS update
    /// (`infra/storage/leases_sea_repo.rs:64`, used at `:75-76` and `:96-97`).
    ///
    /// # Why the existing tests were structurally unable to see it
    ///
    /// Not because any of them is wrong — two distinct reasons, and only the
    /// first is about wiring.
    ///
    /// `leases_tests` and `variables_tests` run against mocks that "never touch
    /// the database" (`domain/service/test_support.rs:48-51`), so the storage
    /// mapper is not on their path at all.
    ///
    /// `tests_tenant_scoping` *is* DB-backed and does write lease rows through
    /// this function, so it is reachable — it simply cannot distinguish the
    /// modes, because it asserts on downstream effects rather than on what was
    /// written. `delete_leased_environment_blocked_until_release` acquires
    /// `Exclusive`, then checks that deletion is refused; but the delete guard is
    /// `!matches!(lease.state, LeaseState::Free)` (`domain/service/environments.rs:368`),
    /// which asks only *whether* the environment is held. An exclusive hold written
    /// as `"parallel"` reads back as `HeldParallel { holders: [run] }`, is
    /// still not `Free`, still blocks the delete, and still releases cleanly —
    /// so the test stays green. Measured for contrast: two *other* mutations of
    /// this function do redden it — writing `HeldParallel` as `"free"` breaks
    /// `lease_scoped_by_tenant`, and writing an empty holder list for
    /// `"exclusive"` makes the row corrupt on read and breaks the delete test.
    /// The exclusive-as-parallel substitution is precisely the one that survives,
    /// because it is the only one that is neither corrupt nor visibly free.
    ///
    /// # What each half of this test pins
    ///
    /// The round trip is the safety property: whatever columns the writer emits,
    /// the reader must recover the same state. The `mode` assertions pin the
    /// encoding itself, which the round trip alone cannot — a writer that
    /// consistently renamed a mode would round-trip fine while making every row
    /// unreadable to any other reader of the table. Nothing in the schema
    /// enforces the vocabulary: `mode` is a bare `VARCHAR(16) NOT NULL`
    /// (`TEXT NOT NULL` on `SQLite`) with no `CHECK` constraint in any of the
    /// three dialect blobs (`migrations/m20260812_000001_initial.rs:59`, `:106`,
    /// `:160`), and `lease_to_state` rejects every value outside these three, so
    /// a fourth one written here would turn each later read of that row into a
    /// hard `Internal` error.
    #[test]
    fn every_lease_state_round_trips_so_an_exclusive_hold_never_reads_as_parallel() {
        let a = Uuid::from_u128(20);
        let b = Uuid::from_u128(21);

        for state in [
            LeaseState::Free,
            LeaseState::HeldParallel {
                holders: vec![a, b],
            },
            LeaseState::HeldExclusive { holder: a },
        ] {
            let (mode, holders) = state_to_columns(&state);
            assert_eq!(
                lease_to_state(&lease(&mode, holders)).unwrap(),
                state,
                "state_to_columns wrote columns that lease_to_state reads back \
                 as a different state, for {state:?}"
            );
        }

        assert_eq!(state_to_columns(&LeaseState::Free).0, "free", "Free mode");
        assert_eq!(
            state_to_columns(&LeaseState::HeldParallel { holders: vec![a] }).0,
            "parallel",
            "HeldParallel mode"
        );
        assert_eq!(
            state_to_columns(&LeaseState::HeldExclusive { holder: a }).0,
            "exclusive",
            "HeldExclusive mode"
        );
    }

    /// The single case where the round trip above is deliberately not an
    /// identity, asserted rather than left out so "every variant round trips"
    /// cannot be read as a stronger claim than it is.
    ///
    /// `HeldParallel { holders: [] }` is written as `("parallel", [])` and read
    /// back as `Free`. That is correct rather than merely tolerable — a parallel
    /// hold with no holders *is* a free environment, and `("parallel", [])` is the
    /// legitimate free encoding pinned by
    /// `the_empty_parallel_holder_list_is_a_real_free_state`. It is also
    /// unreachable from the domain. Every `LeaseState` that reaches
    /// `compare_and_set` comes from one of exactly two functions
    /// (`domain/service/leases.rs:134` and `:191`), and neither can produce it:
    /// `decide_acquire` always pushes the acquiring run onto the holder list, and
    /// `decide_release` collapses an emptied one to `Free` itself
    /// (`domain/lease.rs:64-70`). So the writer is never handed this value in the
    /// first place; the assertion documents the codec's behaviour if it ever were.
    #[test]
    fn a_parallel_hold_with_no_holders_is_written_and_read_back_as_free() {
        let (mode, holders) = state_to_columns(&LeaseState::HeldParallel { holders: vec![] });
        assert_eq!(mode, "parallel");
        assert_eq!(holders, serde_json::json!([]));
        assert_eq!(
            lease_to_state(&lease(&mode, holders)).unwrap(),
            LeaseState::Free
        );
    }
}
