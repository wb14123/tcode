use std::fmt;

/// A shared secret. Custom `Debug` impl redacts the value.
///
/// Intentionally does NOT derive `Clone` or implement `Display`.
/// No `as_str` / getter accessor is exposed — the login/logout ticket will
/// add a constant-time `fn verify(&self, candidate: &[u8]) -> bool` using
/// `subtle` (or equivalent) rather than exposing the inner string.
///
/// The inner field is intentionally unread in this ticket — the login
/// handler that consumes it lands in a follow-up.
pub(crate) struct Secret(
    // TODO: remove when Secret::verify lands (login/logout ticket in Milestone 1)
    #[allow(dead_code)] String,
);

impl Secret {
    pub(crate) fn new(s: String) -> Self {
        Self(s)
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secret(<redacted>)")
    }
}

/// Shared application state handed to every axum handler via `with_state`.
///
/// `Debug` is intentionally not derived so the password cannot be printed
/// by accident via a `#[derive(Debug)]` on an enclosing type.
pub(crate) struct AppState {
    /// Configured shared secret. Not read by this ticket's handlers;
    /// stashed so the future login handler has a stable home for the
    /// comparison.
    // TODO: remove when Secret::verify lands (login/logout ticket in Milestone 1)
    #[allow(dead_code)]
    pub(crate) password: Secret,
}

impl AppState {
    pub(crate) fn new(password: String) -> Self {
        Self {
            password: Secret::new(password),
        }
    }
}
