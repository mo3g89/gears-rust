//! What a plugin *declares*: the shape of an environment's credentials and of
//! what observing it yields.
//!
//! The UI renders forms, tables and detail pages from these descriptors
//! (spec §8), so a plugin adds a field without any UI change. Two invariants
//! are checked at registration rather than at render time, because both
//! failures are silent when they happen late — see [`validate_schemas`].

use serde::{Deserialize, Serialize};

/// How a field is entered and rendered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    Text,
    Url,
    Int,
    Bool,
    Enum,
    /// A single-line secret: an API token, a password.
    Secret,
    /// A multi-line secret: a kubeconfig, a PEM key. Renders as the textarea
    /// today's kubeconfig field uses, which is why VHP's form is unchanged in
    /// appearance despite being generated.
    MultilineSecret,
}

impl FieldKind {
    /// Whether a value of this kind is credential material.
    #[must_use]
    pub const fn is_secret(self) -> bool {
        matches!(self, Self::Secret | Self::MultilineSecret)
    }
}

/// A meaning the *platform* understands, claimed by at most one observed field.
///
/// This is what lets observation be fully plugin-defined without costing
/// `qa-insights` a stable `version`/`build` to group by: the plugin returns an
/// opaque attribute map, and the platform copies the role-claimed attributes
/// into real, indexable columns on every write (spec **D10**).
///
/// # Deviation from spec §5.1: there is no `Health` role
///
/// §5.1 lists five roles; four are implemented. `Health` is deliberately
/// absent: health does not arrive through the attribute map at all. It has
/// its own channel — [`crate::observation::HealthOutcome`], returned
/// alongside the attributes by one `observe` call — because the two fail
/// independently (a namespace-scoped credential can read a version perfectly
/// and be forbidden from reading cluster health), and because a role is a
/// *projection of a string attribute* into a column, while health is a
/// closed enum with its own `health_detail` and `health_checked_at` columns.
/// A `Health` role would give the same fact two sources of truth that could
/// disagree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldRole {
    /// Becomes `qa_environments.observed_version` and a run's `APP_VERSION`.
    Version,
    /// Becomes `qa_environments.observed_build` and a run's `APP_BUILD`.
    Build,
    /// Becomes `qa_environments.observed_base_url` — the column that replaces
    /// `vhp_base_url`.
    BaseUrl,
    /// Surfaced on the detail page; has no column of its own because only
    /// Kubernetes products have a namespace at all.
    Namespace,
}

/// One declared field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct FieldDesc {
    /// Stable identifier, unique within its schema. Also the key under which
    /// the value appears in `observed_attrs` or `credentials`.
    pub key: String,
    /// Human-facing label. The UI shows this, never `key`.
    pub label: String,
    pub kind: FieldKind,
    pub required: bool,
    /// `None` for the great majority of fields.
    pub role: Option<FieldRole>,
    /// Render as a column in the environments table.
    pub in_table: bool,
    /// Render on the environment detail page.
    pub in_detail: bool,
    pub help: Option<String>,
}

/// Why a plugin's declared schemas were rejected at registration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchemaError {
    /// `observed_schema()` declared a secret field. `observed_attrs` is
    /// rendered on the environment page, so nothing secret may reach it.
    SecretInObservedSchema {
        key: String,
    },
    /// Two observed fields claimed the same role.
    DuplicateRole {
        role: FieldRole,
        first: String,
        second: String,
    },
    /// A credential field claimed a role. Roles describe observations.
    RoleOnCredentialField {
        key: String,
    },
    DuplicateKey {
        key: String,
    },
    /// One key appears in *both* schemas. Tasks 21/22 render the credential
    /// form and the environment detail page from these two descriptor lists
    /// side by side, and a key present in both makes "which descriptor
    /// describes this field?" depend on which list a renderer happened to
    /// consult first — with a `MultilineSecret` credential and a `Text`
    /// observation under one key, the wrong answer renders a secret.
    KeyInBothSchemas {
        key: String,
    },
    BlankKey,
}

impl std::fmt::Display for SchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SecretInObservedSchema { key } => write!(
                f,
                "observed field `{key}` declares a secret kind; observed values are rendered on the environment page and may never be secret"
            ),
            Self::DuplicateRole {
                role,
                first,
                second,
            } => {
                #[allow(clippy::use_debug)]
                {
                    write!(
                        f,
                        "fields `{first}` and `{second}` both claim role {role:?}; at most one field may claim each role"
                    )
                }
            }
            Self::RoleOnCredentialField { key } => write!(
                f,
                "credential field `{key}` claims a role; roles describe observed values, not credentials"
            ),
            Self::DuplicateKey { key } => {
                write!(f, "duplicate field key `{key}` within one schema")
            }
            Self::KeyInBothSchemas { key } => write!(
                f,
                "field key `{key}` is declared in both the credential and the observed schema; a key describes one field or the other, never both"
            ),
            Self::BlankKey => write!(f, "a field key is empty or whitespace"),
        }
    }
}

impl std::error::Error for SchemaError {}

/// Check both declared schemas. Called once per plugin at registration; a
/// failure is a boot failure.
///
/// The two rules worth stating plainly, because both fail silently if deferred:
///
/// * **No secret in `observed`.** `observed_attrs` is serialised into
///   `EnvironmentDto` and rendered. A plugin that echoed its kubeconfig into an
///   observed field would reproduce the 2026-08-28 leak through a new door.
/// * **One field per role.** Two `Version` claims make `APP_VERSION` depend on
///   iteration order — a run stamped with an arbitrary one of two versions,
///   with nothing to notice it.
/// * **No key in both schemas.** Tasks 21 and 22 render the UI from both
///   descriptor lists; one key described twice, differently, makes what gets
///   rendered depend on lookup order.
///
/// # Errors
///
/// Returns the first violation found, checking credentials before observations.
pub fn validate_schemas(
    credential: &[FieldDesc],
    observed: &[FieldDesc],
) -> Result<(), SchemaError> {
    check_keys(credential)?;
    check_keys(observed)?;

    if let Some(field) = credential
        .iter()
        .find(|f| observed.iter().any(|o| o.key == f.key))
    {
        return Err(SchemaError::KeyInBothSchemas {
            key: field.key.clone(),
        });
    }

    if let Some(field) = credential.iter().find(|f| f.role.is_some()) {
        return Err(SchemaError::RoleOnCredentialField {
            key: field.key.clone(),
        });
    }

    if let Some(field) = observed.iter().find(|f| f.kind.is_secret()) {
        return Err(SchemaError::SecretInObservedSchema {
            key: field.key.clone(),
        });
    }

    let mut claimed: Vec<(FieldRole, &str)> = Vec::new();
    for field in observed {
        let Some(role) = field.role else { continue };
        if let Some((_, first)) = claimed.iter().find(|(r, _)| *r == role) {
            return Err(SchemaError::DuplicateRole {
                role,
                first: (*first).to_owned(),
                second: field.key.clone(),
            });
        }
        claimed.push((role, &field.key));
    }
    Ok(())
}

fn check_keys(fields: &[FieldDesc]) -> Result<(), SchemaError> {
    let mut seen: Vec<&str> = Vec::new();
    for field in fields {
        if field.key.trim().is_empty() {
            return Err(SchemaError::BlankKey);
        }
        if seen.contains(&field.key.as_str()) {
            return Err(SchemaError::DuplicateKey {
                key: field.key.clone(),
            });
        }
        seen.push(&field.key);
    }
    Ok(())
}

/// The one field a plugin declares as both **required** and **secret**, if
/// there is exactly one.
///
/// This is how a platform column that holds a *single* credential reference
/// gets a **key** without the platform naming any product's credential —
/// `qa_environments.kubeconfig_credstore_ref`, which both observation and
/// dispatch read to build a [`CredentialSlot`](crate::plugin::CredentialSlot).
///
/// `None` for a schema with no required secret and for a schema with two: one
/// column holds one reference and cannot say which of two required secrets it
/// is, and a guess would hand a plugin the wrong material under the right
/// name. A caller that gets `None` must refuse the operation, not pick.
///
/// Optional secret fields are not candidates: an environment that must be
/// reachable has to have the credential it cannot work without, and that is
/// exactly the one marked required.
///
/// # Why it lives in the SDK
///
/// It is a derivation over [`FieldDesc`] and nothing else, and **two gears
/// need the same answer**: `qa-environments` to resolve a slot for
/// [`observe`](crate::QaProductPluginV1::observe), `qa-runs` to name one for
/// [`prepare_run_access`](crate::QaProductPluginV1::prepare_run_access). Two
/// private copies of one derivation is the coupling class that produced five
/// defects in Phase B of this work; a plugin that renames its credential field
/// renames the slot in both gears, in one release, with nothing in either to
/// update.
#[must_use]
pub fn sole_required_secret_key(schema: &[FieldDesc]) -> Option<String> {
    let mut required_secrets = schema
        .iter()
        .filter(|field| field.required && field.kind.is_secret());
    let first = required_secrets.next()?;
    required_secrets.next().is_none().then(|| first.key.clone())
}

#[cfg(test)]
#[path = "descriptor_tests.rs"]
mod descriptor_tests;
