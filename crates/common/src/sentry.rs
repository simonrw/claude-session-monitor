//! Sentry error reporting initialisation.
//!
//! Call [`init`] once near the top of a binary's `main`, binding the returned
//! guard to a local so it lives for the duration of the process. The guard
//! flushes pending events on drop.
//!
//! The DSN is baked in at compile time via `SENTRY_DSN`. When the env var is
//! unset at build time (e.g. local dev without the secret) init becomes a
//! no-op and logs a warning.

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
    // Compile-time DSN. `None` when `SENTRY_DSN` was unset at build time.
    let Some(dsn) = option_env!("SENTRY_DSN") else {
        tracing::warn!("SENTRY_DSN unset at build time; sentry disabled");
        return Guard { _inner: None };
    };

    let release = format!("{binary_name}@{}", env!("CARGO_PKG_VERSION"));
    let inner = ::sentry::init((
        dsn,
        ::sentry::ClientOptions {
            release: Some(release.into()),
            attach_stacktrace: true,
            sample_rate: 1.0,
            ..Default::default()
        },
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
