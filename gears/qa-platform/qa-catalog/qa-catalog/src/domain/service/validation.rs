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

#[cfg(test)]
mod tests {
    use super::{MAX_NAME_LEN, validate_name};
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
}
