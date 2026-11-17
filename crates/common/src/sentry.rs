//! Sentry error reporting initialisation.
//!
//! Call [`init`] once near the top of a binary's `main`, binding the returned
//! guard to a local so it lives for the duration of the process. The guard
//! flushes pending events on drop.
//!
//! The DSN is read from the `SENTRY_DSN` environment variable when the binary
//! starts, and falls back to the value of `SENTRY_DSN` at compile time. When
//! neither is set (e.g. local dev without the secret) init becomes a no-op
//! and logs a warning.

/// RAII guard returned by [`init`]. Dropping it flushes any pending events.
///
/// The type is identical regardless of whether the `sentry` feature is
/// enabled, so callers never need `cfg` attributes.
pub struct Guard {
    #[cfg(feature = "sentry")]
    _inner: Option<::sentry::ClientInitGuard>,
}

/// Initialise sentry for the given binary. The `binary_name` feeds into the
/// release string as `"{binary_name}@{CARGO_PKG_VERSION}"`.
#[cfg(feature = "sentry")]
pub fn init(binary_name: &str) -> Guard {
    let runtime_dsn = std::env::var("SENTRY_DSN")
        .ok()
        .filter(|dsn| !dsn.is_empty());
    let compile_time_dsn = option_env!("SENTRY_DSN").map(String::from);

    let Some(dsn) = runtime_dsn.or(compile_time_dsn) else {
        tracing::warn!("SENTRY_DSN unset at runtime and at build time; sentry disabled");
        return Guard { _inner: None };
    };

    let release = format!("{binary_name}@{}", env!("CARGO_PKG_VERSION"));
    let inner = ::sentry::init((
        dsn,
        ::sentry::ClientOptions::new()
            .release(release)
            .attach_stacktrace(true)
            .sample_rate(1.0),
    ));

    Guard {
        _inner: Some(inner),
    }
}

/// Feature-disabled no-op. Signature matches the enabled variant.
#[cfg(not(feature = "sentry"))]
pub fn init(_binary_name: &str) -> Guard {
    Guard {}
}

/// Forward an error to Sentry. No-op when the `sentry` feature is disabled.
///
/// Callers can use this without `cfg` gating at the call site.
#[cfg(feature = "sentry")]
pub fn capture_error<E: std::error::Error + ?Sized>(err: &E) {
    ::sentry::capture_error(err);
}

/// Feature-disabled no-op. Signature matches the enabled variant.
#[cfg(not(feature = "sentry"))]
pub fn capture_error<E: std::error::Error + ?Sized>(_err: &E) {}
