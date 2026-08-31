//! Conversions between `SeaORM` entity models and SDK contract types.

use qa_environments_sdk::{
    ClusterHealthView, LeaseState, NodeCounts as SdkNodeCounts, NodeSummary as SdkNodeSummary,
    TargetPlatform, Variable,
};
use time::OffsetDateTime;
use uuid::Uuid;

use super::entity::{pipeline_variable, platform, platform_lease, platform_variable};
use crate::domain::error::DomainError;
use crate::domain::observation::{ClusterHealth, NodeSummary as DomainNodeSummary};

/// Convert a platform database entity to a contract model.
#[must_use]
pub fn platform_to_sdk(m: platform::Model) -> TargetPlatform {
    TargetPlatform {
        id: m.id,
        name: m.name,
        product_id: m.product_id,
        description: m.description,
        kubeconfig_credstore_ref: m.kubeconfig_credstore_ref,
        available: m.available,
        observed_version: m.observed_version,
        observed_build: m.observed_build,
        default_branch: m.default_branch,
        is_default: m.is_default,
        vhp_base_url: m.vhp_base_url,
        observed_namespace: m.observed_namespace,
        version_detect_error: m.version_detect_error,
        version_detected_at: m.version_detected_at,
        cluster: cluster_health_view(
            m.id,
            m.cluster_status,
            m.cluster_status_message,
            m.cluster_nodes,
            m.cluster_namespace_count,
            m.cluster_checked_at,
        ),
        created_at: m.created_at,
        updated_at: m.updated_at,
    }
}

/// Map the five `cluster_*` columns onto the one optional view the SDK and
/// REST layers publish.
///
/// `status.is_none()` is the only thing that produces `None`: a cycle that
/// reached this platform always writes `cluster_status`, even on a failed
/// read (as `"Unreachable"` — see `record_observation`'s D-CH-3 rule), so
/// `cluster_status`'s presence is exactly "has a cycle ever reached this
/// platform", matching [`ClusterHealthView`]'s own doc on why that must stay
/// distinguishable from a checked-and-failed reading.
///
/// `cluster_nodes` degrades to an empty node list on a `NULL` or an
/// unparseable blob rather than failing the whole mapping — the node list is
/// already allowed to be empty for a real reason (a failed read, or a
/// healthy cluster the observer has not yet visited), so a row this mapper
/// cannot parse degrades the same way rather than turning a read into an
/// error. An unparseable (as opposed to `NULL`) blob is a corrupt row rather
/// than a legitimate empty state, though, so it is logged -- naming the
/// platform so an operator can find the row, never the blob itself, which
/// would risk logging whatever a future schema change put in that column.
///
/// `checked_at` is not defaulted: a row with a `status` but no
/// `cluster_checked_at` cannot happen through `record_observation` (both
/// branches set it in the same `UPDATE` as `status`), so rather than
/// fabricate a "now" that would falsely claim the cluster was just read,
/// such a row maps to `None` — read as "never checked", which is the honest
/// answer for a row this mapper cannot make sense of.
fn cluster_health_view(
    platform_id: Uuid,
    status: Option<String>,
    status_message: Option<String>,
    nodes: Option<serde_json::Value>,
    namespace_count: Option<i32>,
    checked_at: Option<OffsetDateTime>,
) -> Option<ClusterHealthView> {
    let status = status?;
    let checked_at = checked_at?;
    let nodes: Vec<DomainNodeSummary> = match nodes {
        None => Vec::new(),
        Some(value) => serde_json::from_value(value).unwrap_or_else(|error| {
            tracing::warn!(
                platform_id = %platform_id,
                %error,
                "qa-environments: this platform's stored cluster_nodes blob does not \
                 parse as a node list; reading it as empty rather than failing the whole \
                 mapping"
            );
            Vec::new()
        }),
    };
    // `ClusterHealth::counts()` is the one place these numbers are derived
    // (D-CH-2) -- reused here rather than re-implemented so the mapper and
    // the domain layer can never disagree about how to count a node list.
    // `namespace_count` plays no part in `counts()`, so `None` here is a
    // placeholder, not a second source of truth.
    let health = ClusterHealth {
        nodes,
        namespace_count: None,
    };
    let counts = health.counts();
    Some(ClusterHealthView {
        status,
        status_message,
        nodes: health.nodes.into_iter().map(node_summary_to_sdk).collect(),
        namespace_count: namespace_count.and_then(|n| u32::try_from(n).ok()),
        counts: SdkNodeCounts {
            total: counts.total,
            ready: counts.ready,
            control_plane: counts.control_plane,
            ready_control_plane: counts.ready_control_plane,
            worker: counts.worker,
            ready_worker: counts.ready_worker,
        },
        checked_at,
    })
}

/// One node, domain shape to SDK shape -- a field-for-field copy, split out
/// so `cluster_health_view` reads as "what", not "how".
fn node_summary_to_sdk(n: DomainNodeSummary) -> SdkNodeSummary {
    SdkNodeSummary {
        name: n.name,
        control_plane: n.control_plane,
        ready: n.ready,
        kubelet_version: n.kubelet_version,
        os_image: n.os_image,
    }
}

/// Convert a per-platform variable database entity to a contract model.
#[must_use]
pub fn platform_var_to_sdk(m: platform_variable::Model) -> Variable {
    Variable {
        id: m.id,
        platform_id: Some(m.platform_id),
        name: m.name,
        value: m.value,
    }
}

/// Convert a pipeline (global) variable database entity to a contract model.
#[must_use]
pub fn pipeline_var_to_sdk(m: pipeline_variable::Model) -> Variable {
    Variable {
        id: m.id,
        platform_id: None,
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
/// acquire a platform another run believes it holds exclusively. `"free"`
/// (or `"parallel"` with an empty holders array) is a legitimate,
/// intentionally-written free state and maps to `Free` without error.
///
/// # The decode must fail closed, and once did not
///
/// `holders` was decoded with `unwrap_or_default()` until 2026-08-13. That
/// turned **any** unparseable value into an empty vec, which then matched the
/// `("parallel", [])` arm and returned `LeaseState::Free` — the single most
/// dangerous answer this function can give, on the row that arbitrates
/// exclusive access to a shared physical platform. The paragraph above already
/// promised otherwise, so the doc was false and the failure was open at the
/// same time. Found by the security review of qa-runs Task 9.
///
/// The distinction that makes the fix non-obvious: `("parallel", [])` really is
/// legitimate — a holder list that decoded successfully and was empty means the
/// platform is free, and `the_empty_parallel_holder_list_is_a_real_free_state`
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
pub fn lease_to_state(m: &platform_lease::Model) -> Result<LeaseState, DomainError> {
    let holders: Vec<Uuid> = serde_json::from_value(m.holders.clone()).map_err(|e| {
        DomainError::Internal(format!(
            "corrupt lease row: holders is not a JSON array of UUIDs for platform_id={}: {e}",
            m.platform_id
        ))
    })?;
    match (m.mode.as_str(), holders.as_slice()) {
        ("free", _) | ("parallel", []) => Ok(LeaseState::Free),
        ("parallel", _) => Ok(LeaseState::HeldParallel { holders }),
        ("exclusive", [holder]) => Ok(LeaseState::HeldExclusive { holder: *holder }),
        (mode, hs) => Err(DomainError::Internal(format!(
            "corrupt lease row: unknown mode {mode:?} with {} holder(s) for platform_id={}",
            hs.len(),
            m.platform_id
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

    /// A platform row with **every nullable column populated**, which is the
    /// entire point: a `None` survives a dropped field unchanged, so a fixture
    /// full of `None`s proves nothing about the mapper.
    fn full_platform_row() -> platform::Model {
        platform::Model {
            id: Uuid::from_u128(1),
            tenant_id: Uuid::from_u128(2),
            name: "staging-a".to_owned(),
            product_id: Some(Uuid::from_u128(3)),
            description: Some("desc".to_owned()),
            kubeconfig_credstore_ref: "credstore://ref".to_owned(),
            available: true,
            observed_version: Some("5.0.1".to_owned()),
            observed_build: Some("20260813".to_owned()),
            default_branch: Some("release-9.0".to_owned()),
            is_default: false,
            vhp_base_url: Some("https://sv.jele.io".to_owned()),
            observed_namespace: Some("virtuozzo".to_owned()),
            version_detect_error: Some("namespaces \"virtuozzo\" not found".to_owned()),
            version_detected_at: Some(OffsetDateTime::from_unix_timestamp(1_786_579_250).unwrap()),
            // Populated with an `"Unreachable"` reading rather than a
            // `"Healthy"` one: a real row never carries both a non-`NULL`
            // `cluster_status_message` and a non-empty `cluster_nodes`
            // together (D-CH-3 clears the nodes on a failed read), but this
            // mapper does not enforce that invariant -- it only reads
            // whatever is there -- so an unrealistic-but-fully-populated
            // combination is what lets one fixture exercise all five
            // `cluster_*` columns at once.
            cluster_status: Some("Unreachable".to_owned()),
            cluster_status_message: Some("the API server could not be reached".to_owned()),
            cluster_nodes: Some(serde_json::json!([{
                "name": "sv-vhp-jele-io",
                "control_plane": true,
                "ready": true,
                "kubelet_version": "v1.33.4+k3s1",
                "os_image": "Ubuntu 24.04.3 LTS",
            }])),
            cluster_namespace_count: Some(14),
            cluster_checked_at: Some(OffsetDateTime::from_unix_timestamp(1_786_579_260).unwrap()),
            created_at: OffsetDateTime::from_unix_timestamp(1_786_579_200).unwrap(),
            updated_at: OffsetDateTime::from_unix_timestamp(1_786_579_300).unwrap(),
        }
    }

    /// Every column `platform_to_sdk` reads must reach the SDK model.
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
    /// `platform_dto_from_sdk_carries_every_field_it_publishes` looks like the
    /// guard but is not: it covers `TargetPlatform → PlatformDto`, one layer
    /// further out, never touches this function, and — since `PlatformDto` stopped
    /// publishing `kubeconfig_credstore_ref` — does not even cover every field of
    /// the SDK model.
    ///
    /// Break-tested per field: replacing any one of `platform_to_sdk`'s **fifteen
    /// scalar** reads turns at least one of this pair red — verified field by
    /// field, all fifteen. That per-field property is what makes it a guard rather
    /// than a smoke test; asserting only the fields someone remembered is how the
    /// hole was left in the first place.
    ///
    /// The claim needed the companion test to stop using `..full_platform_row()`.
    /// While it shared this fixture's `id`, `name`, `kubeconfig_credstore_ref`,
    /// `available` and timestamps, a literal substituted for any of those six
    /// matched in *both* tests and stayed green — so an earlier version of this
    /// sentence was false for six of the reads it claimed. See
    /// `platform_to_sdk_preserves_absent_optionals`.
    ///
    /// **Fifteen, not eleven** — updated when the 2026-08-28 platform-observation
    /// work gave `platform_to_sdk` four more reads (`vhp_base_url`,
    /// `observed_namespace`, `version_detect_error`, `version_detected_at`).
    /// `platform::Model` has sixteen columns; `tenant_id` is the one deliberately
    /// not mapped (tenancy is enforced by the scope on every query, never carried
    /// in a contract type), leaving fifteen scalar reads as the whole surface.
    /// Keeping this count current is exactly the discipline the previous
    /// correction ("eleven, not ten") was about.
    ///
    /// Added by Task 13b at the coordinator's request; the gap it closes is
    /// Task 9b's, not Task 13b's.
    ///
    /// # `cluster`, added by Task 5, is not part of that count
    ///
    /// `cluster: Option<ClusterHealthView>` folds the *other* five columns —
    /// `cluster_status`, `cluster_status_message`, `cluster_nodes`,
    /// `cluster_namespace_count`, `cluster_checked_at` — into one field via
    /// `cluster_health_view`, so it is not a sixteenth instance of the same
    /// one-column-to-one-field shape the count above describes. It is not left
    /// untested for that reason: the fixture above populates all five, and the
    /// assertions below check every one of `ClusterHealthView`'s own fields
    /// (`status`, `status_message`, `nodes` — including a node's
    /// `kubelet_version`/`os_image` — `namespace_count`, `counts`, and
    /// `checked_at`), plus the `None` case in
    /// `platform_to_sdk_preserves_absent_optionals` below and the corrupt-blob
    /// degrade path in
    /// `an_unparseable_cluster_nodes_blob_degrades_to_empty_nodes_not_to_never_checked`.
    #[test]
    #[allow(
        clippy::cognitive_complexity,
        reason = "a flat list of one assertion per column/field, the exact shape this test's \
                  own doc says a guard like this needs; splitting it into helper functions \
                  would not reduce what it checks, only how it reads"
    )]
    fn platform_to_sdk_preserves_every_column() {
        let row = full_platform_row();
        let sdk = platform_to_sdk(row.clone());

        assert_eq!(sdk.id, row.id, "id");
        assert_eq!(sdk.name, row.name, "name");
        assert_eq!(sdk.product_id, row.product_id, "product_id");
        assert_eq!(sdk.description, row.description, "description");
        assert_eq!(
            sdk.kubeconfig_credstore_ref, row.kubeconfig_credstore_ref,
            "kubeconfig_credstore_ref"
        );
        assert_eq!(sdk.available, row.available, "available");
        assert_eq!(
            sdk.observed_version, row.observed_version,
            "observed_version -- the column Task 9b existed to protect, and the \
             one this assertion was missing for"
        );
        assert_eq!(sdk.observed_build, row.observed_build, "observed_build");
        assert_eq!(sdk.default_branch, row.default_branch, "default_branch");
        assert_eq!(sdk.vhp_base_url, row.vhp_base_url, "vhp_base_url");
        assert_eq!(
            sdk.observed_namespace, row.observed_namespace,
            "observed_namespace"
        );
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

        // `tenant_id` is deliberately absent from `TargetPlatform`: tenancy is
        // enforced by the scope on every query, not carried in the contract type.
        // Stated so its absence above reads as intentional rather than forgotten.

        let cluster = sdk
            .cluster
            .expect("cluster_status was Some, so a view must exist");
        assert_eq!(cluster.status, "Unreachable", "cluster.status");
        assert_eq!(
            cluster.status_message.as_deref(),
            Some("the API server could not be reached"),
            "cluster.status_message"
        );
        assert_eq!(cluster.nodes.len(), 1, "cluster.nodes");
        assert_eq!(
            cluster.nodes[0].name, "sv-vhp-jele-io",
            "cluster.nodes[0].name"
        );
        assert!(
            cluster.nodes[0].control_plane,
            "cluster.nodes[0].control_plane"
        );
        assert!(cluster.nodes[0].ready, "cluster.nodes[0].ready");
        assert_eq!(
            cluster.nodes[0].kubelet_version.as_deref(),
            Some("v1.33.4+k3s1"),
            "cluster.nodes[0].kubelet_version"
        );
        assert_eq!(
            cluster.nodes[0].os_image.as_deref(),
            Some("Ubuntu 24.04.3 LTS"),
            "cluster.nodes[0].os_image"
        );
        assert_eq!(cluster.namespace_count, Some(14), "cluster.namespace_count");
        assert_eq!(cluster.counts.total, 1, "cluster.counts.total");
        assert_eq!(
            cluster.counts.control_plane, 1,
            "cluster.counts.control_plane"
        );
        assert_eq!(
            cluster.checked_at,
            row.cluster_checked_at.unwrap(),
            "cluster.checked_at"
        );
    }

    /// The nullable columns must also survive as `None`, so a platform with
    /// nothing observed and no override does not acquire phantom values.
    ///
    /// The companion to the test above: that one would pass if the mapper
    /// substituted a constant for every nullable field, this one would not.
    ///
    /// # Every non-optional value here differs from `full_platform_row`'s
    ///
    /// This row is built field-by-field rather than with `..full_platform_row()`,
    /// and that is load-bearing. Sharing the populated fixture's `id`, `name`,
    /// `kubeconfig_credstore_ref`, `available` and timestamps meant a mapper that
    /// replaced any of those six reads with the fixture's own literal stayed
    /// **green in both tests** — so the sibling's claim to cover "any one of the
    /// fifteen reads" was false for exactly those six. Distinct values here make
    /// the two tests disagree on every field, which is what the claim needs.
    #[test]
    fn platform_to_sdk_preserves_absent_optionals() {
        let row = platform::Model {
            id: Uuid::from_u128(11),
            tenant_id: Uuid::from_u128(12),
            name: "staging-b".to_owned(),
            product_id: None,
            description: None,
            kubeconfig_credstore_ref: "credstore://other".to_owned(),
            available: false,
            observed_version: None,
            observed_build: None,
            default_branch: None,
            is_default: false,
            vhp_base_url: None,
            observed_namespace: None,
            version_detect_error: None,
            version_detected_at: None,
            cluster_status: None,
            cluster_status_message: None,
            cluster_nodes: None,
            cluster_namespace_count: None,
            cluster_checked_at: None,
            created_at: OffsetDateTime::from_unix_timestamp(1_786_600_000).unwrap(),
            updated_at: OffsetDateTime::from_unix_timestamp(1_786_600_100).unwrap(),
        };
        let sdk = platform_to_sdk(row.clone());

        // The non-optional fields, asserted here too: with values distinct from
        // `full_platform_row`'s, these are what close the six-field hole above.
        assert_eq!(sdk.id, row.id, "id");
        assert_eq!(sdk.name, row.name, "name");
        assert_eq!(
            sdk.kubeconfig_credstore_ref, row.kubeconfig_credstore_ref,
            "kubeconfig_credstore_ref"
        );
        assert_eq!(sdk.available, row.available, "available");
        assert_eq!(sdk.created_at, row.created_at, "created_at");
        assert_eq!(sdk.updated_at, row.updated_at, "updated_at");

        assert_eq!(sdk.product_id, None, "product_id");
        assert_eq!(sdk.description, None, "description");
        assert_eq!(sdk.observed_version, None, "observed_version");
        assert_eq!(sdk.observed_build, None, "observed_build");
        assert_eq!(sdk.default_branch, None, "default_branch");
        assert_eq!(sdk.vhp_base_url, None, "vhp_base_url");
        assert_eq!(sdk.observed_namespace, None, "observed_namespace");
        assert_eq!(sdk.version_detect_error, None, "version_detect_error");
        assert_eq!(sdk.version_detected_at, None, "version_detected_at");
        assert_eq!(
            sdk.cluster, None,
            "a NULL cluster_status must map to None, not to a view with empty/zero fields -- \
             the distinction `ClusterHealthView`'s own doc exists for"
        );
    }

    /// An unparseable `cluster_nodes` blob must degrade to an empty node
    /// list, not to `cluster: None`. Those are different facts: `None` means
    /// no cycle ever reached this platform, while a `Some` with an empty
    /// `nodes` (here, because the stored blob is corrupt) means one did and
    /// left a status behind that this mapper just cannot fully reconstruct.
    /// Collapsing the second into the first would be exactly the
    /// `unwrap_or_default()` mistake this file's own `lease_to_state` note
    /// describes -- decoded-empty and failed-to-decode must stay
    /// distinguishable, and here the distinguishing fact (`cluster.is_some()`)
    /// is the only thing left to hold onto once the blob itself is unusable.
    #[test]
    fn an_unparseable_cluster_nodes_blob_degrades_to_empty_nodes_not_to_never_checked() {
        let row = platform::Model {
            id: Uuid::from_u128(21),
            tenant_id: Uuid::from_u128(22),
            name: "staging-c".to_owned(),
            product_id: None,
            description: None,
            kubeconfig_credstore_ref: "credstore://corrupt".to_owned(),
            available: true,
            observed_version: None,
            observed_build: None,
            default_branch: None,
            is_default: false,
            vhp_base_url: None,
            observed_namespace: None,
            version_detect_error: None,
            version_detected_at: None,
            cluster_status: Some("Healthy".to_owned()),
            cluster_status_message: None,
            cluster_nodes: Some(serde_json::json!("not-an-array")),
            cluster_namespace_count: Some(3),
            cluster_checked_at: Some(OffsetDateTime::from_unix_timestamp(1_786_700_200).unwrap()),
            created_at: OffsetDateTime::from_unix_timestamp(1_786_700_000).unwrap(),
            updated_at: OffsetDateTime::from_unix_timestamp(1_786_700_100).unwrap(),
        };
        let sdk = platform_to_sdk(row);

        let cluster = sdk.cluster.expect(
            "a corrupt cluster_nodes blob must still produce a view -- cluster_status \
                     was Some, so a cycle DID reach this platform",
        );
        assert!(
            cluster.nodes.is_empty(),
            "an unparseable blob must read as no nodes, not panic or propagate"
        );
        assert_eq!(
            cluster.counts.total, 0,
            "counts must be derived from the degraded (empty) list"
        );
    }

    /// `platform_var_to_sdk` must carry all four of the columns it reads.
    ///
    /// # Why this test exists
    ///
    /// This function had **no test at all** until Task 13d. Mutating
    /// `value: m.value` to `String::new()` left all 77 of the gear's tests green
    /// — measured, not assumed — so every platform variable could have come back
    /// with an empty value and nothing would have noticed. That failure is
    /// especially quiet for a variable: an empty value is a legal write —
    /// `validate_value` bounds the length and never requires content, and only
    /// `name` is checked for emptiness (`domain/service/variables.rs:287-293`,
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
    /// constant `platform_id: None` — exactly one was covered by an existing
    /// test: `pipeline_var_to_sdk`'s `name`, which reddens
    /// `variables_scoped_by_tenant`. The other seven, including both `value`s,
    /// both `id`s, both `platform_id`s and `platform_var_to_sdk`'s own `name`,
    /// turned nothing red. This test and its sibling below cover all eight, and
    /// each of the eight was break-tested individually.
    ///
    /// # The asymmetry, and the values chosen to pin it
    ///
    /// `platform_id` is the only field on which this function and
    /// `pipeline_var_to_sdk` differ, and so the one a copy-paste between them
    /// would get wrong. A platform variable is scoped to a platform and must
    /// report `Some`; a pipeline variable is global and must report `None`.
    ///
    /// Two consequences for the fixture, both deliberate: `id` and `platform_id`
    /// are distinct values, so a mapper reading `Some(m.id)` for `platform_id` is
    /// caught rather than matching by accident; and every value here differs from
    /// `pipeline_var_to_sdk_preserves_every_column_and_has_no_platform`'s, so the
    /// two tests disagree on every field they share. That is the property the
    /// platform mapper's sibling pair had to be rebuilt to get (see
    /// `platform_to_sdk_preserves_absent_optionals`).
    ///
    /// `tenant_id`, `created_at` and `updated_at` are columns on the row that
    /// `Variable` does not carry; as with `platform_to_sdk`, tenancy is enforced
    /// by the scope on every query rather than in the contract type.
    #[test]
    fn platform_var_to_sdk_preserves_every_column_and_names_its_platform() {
        let row = platform_variable::Model {
            id: Uuid::from_u128(31),
            tenant_id: Uuid::from_u128(32),
            platform_id: Uuid::from_u128(33),
            name: "REGION".to_owned(),
            value: "eu-west-1".to_owned(),
            created_at: OffsetDateTime::from_unix_timestamp(1_786_700_000).unwrap(),
            updated_at: OffsetDateTime::from_unix_timestamp(1_786_700_100).unwrap(),
        };
        let sdk = platform_var_to_sdk(row.clone());

        assert_eq!(sdk.id, row.id, "id");
        assert_eq!(sdk.name, row.name, "name");
        assert_eq!(
            sdk.value, row.value,
            "value -- the read whose loss the whole suite could not see"
        );
        assert_eq!(
            sdk.platform_id,
            Some(row.platform_id),
            "platform_id must be Some for a per-platform variable, and must be \
             the platform's id rather than the variable's own"
        );
    }

    /// The pipeline half of the pair above: the same four fields, three of them
    /// column reads, and the fourth the one that must differ.
    ///
    /// Also untested until Task 13d. `platform_id: None` is not an omission but
    /// the contract — a pipeline variable is global, and a `Some` here would
    /// misreport it as scoped to whichever platform the value came from. Since
    /// `pipeline_variable::Model` has no `platform_id` column, the shape a
    /// copy-paste from `platform_var_to_sdk` actually produces is `Some(m.id)`,
    /// which the `None` assertion below catches.
    #[test]
    fn pipeline_var_to_sdk_preserves_every_column_and_has_no_platform() {
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
            sdk.platform_id, None,
            "platform_id must be None for a pipeline (global) variable -- the \
             one field that distinguishes this mapper from platform_var_to_sdk"
        );
    }

    fn lease(mode: &str, holders: serde_json::Value) -> platform_lease::Model {
        platform_lease::Model {
            platform_id: Uuid::from_u128(1),
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
    /// matched the `("parallel", [])` arm, and reported the platform **free** —
    /// letting a second run acquire a platform another run believes it holds
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
    /// (`domain/lease.rs:34-43`). So the next run to ask for the platform in
    /// parallel mode **joins one another run holds exclusively** — the same
    /// outcome `lease_to_state`'s doc above calls "the single most dangerous
    /// answer this function can give", reached from the write side instead of
    /// the read side.
    ///
    /// The read half carries four tests and a fix-history section. The write half
    /// carried none, though `state_to_columns` is the sole producer of these two
    /// columns in the gear's write path, feeding both of `compare_and_set`'s
    /// branches — the version-0 insert and the CAS update
    /// (`infra/storage/leases_sea_repo.rs:63`, used at `:74-75` and `:95-96`).
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
    /// written. `delete_leased_platform_blocked_until_release` acquires
    /// `Exclusive`, then checks that deletion is refused; but the delete guard is
    /// `!matches!(lease.state, LeaseState::Free)` (`domain/service/platforms.rs:368`),
    /// which asks only *whether* the platform is held. An exclusive hold written
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
    /// hold with no holders *is* a free platform, and `("parallel", [])` is the
    /// legitimate free encoding pinned by
    /// `the_empty_parallel_holder_list_is_a_real_free_state`. It is also
    /// unreachable from the domain. Every `LeaseState` that reaches
    /// `compare_and_set` comes from one of exactly two functions
    /// (`domain/service/leases.rs:124` and `:176`), and neither can produce it:
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
