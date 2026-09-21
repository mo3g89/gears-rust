//! Field validation rules shared across the resource services.
//!
//! One definition per rule, in the general (field-parameterized) form. The
//! per-service copies these replaced had drifted: two byte-identical 1-arg
//! `validate_name`s, one parameterized one, one inlined in a larger
//! validator, and four separate `MAX_NAME_LEN` constants — with the branch
//! rule wording its (identical) limit differently.

use crate::domain::error::DomainError;

/// Maximum length, in bytes, of every user-supplied name-like field in this
/// gear (repository, custom-plan, product and SSH-key names, product
/// versions, git branch names). Matches the `VARCHAR(255)` columns the
/// migration declares.
pub(super) const MAX_NAME_LEN: usize = 255;

/// Validate a name-like field: non-empty, at most [`MAX_NAME_LEN`] bytes.
///
/// `field` names the field in the resulting [`DomainError::Validation`], so
/// the REST layer reports the caller's own field name (`name`,
/// `default_branch`, `version`, …).
///
/// Branch names use exactly this rule — a git branch name is a
/// non-empty, length-bounded string as far as this gear is concerned; the
/// remote is the authority on whether it exists.
///
/// # Errors
///
/// [`DomainError::Validation`] on an empty or over-long value.
pub(super) fn validate_name(field: &str, value: &str) -> Result<(), DomainError> {
    if value.is_empty() {
        return Err(DomainError::Validation {
            field: field.to_owned(),
            message: "must not be empty".to_owned(),
        });
    }
    if value.len() > MAX_NAME_LEN {
        return Err(DomainError::Validation {
            field: field.to_owned(),
            message: format!("must not exceed {MAX_NAME_LEN} characters"),
        });
    }
    Ok(())
}

/// Maximum length, in bytes, of a `plugin_instance_id` — the width of the
/// `qa_products.plugin_instance_id` column
/// (`m20260903_000003_product_plugin_instance` (folded into `migrations::m20260812_000002_initial` by the docs squash)).
///
/// Bounded here so an over-long id is a named 400 from this gear rather than
/// a `DbErr` from the driver, which is the same argument qa-environments'
/// `validate_default_branch` makes for its own column width.
pub(super) const MAX_PLUGIN_INSTANCE_ID_LEN: usize = 512;

/// Validate an optional product-plugin binding.
///
/// `None` is accepted: the column is nullable through the expand half of the
/// expand/contract pair, and an update passing `None` leaves the product's
/// current binding **unchanged** rather than unbinding it (ruling D-18 — a
/// normal UI edit cannot send the field, so full-replace semantics silently
/// unbound every product it touched).
///
/// `Some` must be a **full GTS instance id** — the plugin spec's type id with
/// the plugin's instance segment appended, which is what a plugin registers
/// its `ClientHub` scope under. The check is `GtsInstanceId::try_new`, so it
/// also rejects the two mistakes that would otherwise resolve to nothing at
/// all and read as "no such plugin": a bare instance segment, and a *type*
/// id (one ending in `~`).
///
/// # Errors
///
/// [`DomainError::Validation`] on an empty, over-long, or malformed id.
pub(super) fn validate_plugin_instance_id(value: Option<&str>) -> Result<(), DomainError> {
    let Some(value) = value else {
        return Ok(());
    };

    let invalid = |message: String| DomainError::Validation {
        field: "plugin_instance_id".to_owned(),
        message,
    };

    if value.is_empty() {
        return Err(invalid(
            // **Not "omit the field to leave it unbound"**, which is what
            // this said and which is advice that silently does nothing:
            // omitting it returns 200 with the binding intact (ruling D-18).
            // Corrected at the Phase E review, finding FW-4.
            //
            // And not the *update*-only advice it became either: since Task
            // 20a this validator also serves `create_product`, where there is
            // no current binding to leave alone and omitting the field is a
            // different 400 from `TryFrom<CreateProductReq>`. So the message
            // names the one action that is right on both paths (review
            // finding m-1).
            "must not be empty; supply the full GTS instance id of a registered product \
             plugin -- `GET /qa/v1/product-plugins` lists them"
                .to_owned(),
        ));
    }
    if value.len() > MAX_PLUGIN_INSTANCE_ID_LEN {
        return Err(invalid(format!(
            "must not exceed {MAX_PLUGIN_INSTANCE_ID_LEN} characters"
        )));
    }
    gts::GtsInstanceId::try_new(value).map_err(|e| {
        invalid(format!(
            "must be a full GTS instance id, not an instance segment or a type id: {e}"
        ))
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_NAME_LEN, MAX_PLUGIN_INSTANCE_ID_LEN, validate_name, validate_plugin_instance_id,
    };
    use crate::domain::error::DomainError;

    #[test]
    fn empty_and_overlong_values_are_rejected_under_the_callers_field_name() {
        for (field, value) in [("name", String::new()), ("version", String::new())] {
            let err = validate_name(field, &value).expect_err("empty must be rejected");
            assert!(
                matches!(err, DomainError::Validation { field: f, .. } if f == field),
                "the caller's field name must survive"
            );
        }

        let long = "x".repeat(MAX_NAME_LEN + 1);
        let err = validate_name("default_branch", &long).expect_err("over-long must be rejected");
        assert!(
            matches!(err, DomainError::Validation { ref message, .. } if message.contains("255")),
            "got {err:?}"
        );
    }

    #[test]
    fn boundary_length_is_accepted() {
        assert!(validate_name("name", &"x".repeat(MAX_NAME_LEN)).is_ok());
        assert!(validate_name("name", "a").is_ok());
    }

    /// The full id the VHP plugin registers under, which is the shape the
    /// binding must accept.
    const FULL_INSTANCE_ID: &str =
        "gts.cf.toolkit.plugins.plugin.v1~cf.core.qa_product.plugin.v1~cf.core._.vhp_product.v1";

    #[test]
    fn an_absent_binding_and_a_full_instance_id_are_both_accepted() {
        assert!(validate_plugin_instance_id(None).is_ok());
        assert!(
            validate_plugin_instance_id(Some(FULL_INSTANCE_ID)).is_ok(),
            "the id every backfilled product carries must validate"
        );
    }

    /// The two near-misses that resolve to nothing at all rather than
    /// failing: the bare instance segment (what the plan first specified as
    /// the stored value), and the plugin spec's *type* id (trailing `~`).
    #[test]
    fn a_bare_segment_and_a_type_id_are_rejected_under_the_columns_field_name() {
        for value in [
            "cf.core._.vhp_product.v1",
            "gts.cf.toolkit.plugins.plugin.v1~cf.core.qa_product.plugin.v1~",
            "",
            "not a gts id at all",
        ] {
            let err = validate_plugin_instance_id(Some(value))
                .expect_err("a malformed instance id must be rejected");
            assert!(
                matches!(err, DomainError::Validation { ref field, .. } if field == "plugin_instance_id"),
                "got {err:?} for `{value}`"
            );
        }
    }

    #[test]
    fn an_overlong_binding_is_rejected_before_the_driver_sees_it() {
        let long = format!(
            "{FULL_INSTANCE_ID}{}",
            "x".repeat(MAX_PLUGIN_INSTANCE_ID_LEN)
        );
        let err = validate_plugin_instance_id(Some(&long)).expect_err("over-long must be rejected");
        assert!(
            matches!(err, DomainError::Validation { ref message, .. }
                if message.contains(&MAX_PLUGIN_INSTANCE_ID_LEN.to_string())),
            "got {err:?}"
        );
    }
}
