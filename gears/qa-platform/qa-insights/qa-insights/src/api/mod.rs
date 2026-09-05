//! Transport layer. REST today; nothing else is planned.
//!
//! Founded at Task 16, which needed somewhere to put the operator rebuild
//! endpoint and therefore built the whole tier: [`rest::dto`], the single
//! canonical [`rest::error`] mapping, [`rest::handlers`] and [`rest::routes`].
//! Task 9 left this file a doc comment and
//! [`crate::gear::QaInsights::register_rest`] returning the router untouched;
//! both are now real.
//!
//! # One `From<DomainError>` mapping, and it lives in [`rest::error`]
//!
//! This file said so before there was any code to hold to it, and the rule is
//! restated here because Tasks 17-39 each add handlers: **no handler decides a
//! status code.** A handler returns
//! `ApiResult<T>`, `?` resolves through
//! `From<DomainError> for CanonicalError`, and every HTTP-visible shape this
//! gear can produce is therefore visible in one exhaustive `match`. Scattering
//! that decision is how two endpoints come to answer 404 and 409 for the same
//! domain condition.

pub mod rest;
