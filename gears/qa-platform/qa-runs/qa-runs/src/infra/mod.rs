pub mod executor;
/// Leader election for the dispatcher ticker.
pub mod leader;
/// Live log fan-out for the SSE endpoint. Task 15.
pub mod logs;
/// The `OpenTelemetry` adapter behind `domain::ports::metrics` — one struct
/// implementing both emission ports over instruments built once at gear init.
pub mod metrics;
/// The product-plugin resolver, reached through the `ClientHub` — see the
/// module's own header for why it resolves per call.
pub mod product_plugin;
pub mod storage;
