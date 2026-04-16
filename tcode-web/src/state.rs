use std::collections::HashSet;
use std::fmt;

use base64::Engine;
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Raw token byte length (32 = 256 bits of entropy).
const SESSION_TOKEN_BYTES: usize = 32;
/// Byte length of every minted token. Used as a cheap prefilter in
/// `verify_session` to reject obviously wrong-length cookie values
/// before the `HashSet` lookup.
pub(crate) const SESSION_TOKEN_B64_LEN: usize = 43;
// Compile-time sanity check tying the two constants together.
// base64url-NO-PAD length = ceil(bytes * 4 / 3).
const _: () = assert!(SESSION_TOKEN_B64_LEN == (SESSION_TOKEN_BYTES * 4).div_ceil(3));

/// A shared secret. Custom `Debug` impl redacts the value.
///
/// Intentionally does NOT derive `Clone` or implement `Display`.
/// No `as_str` / getter accessor is exposed — password comparison goes through
/// the constant-time [`Secret::verify`] method rather than leaking the inner
/// string.
///
/// The inner `String` is zeroized on drop via `zeroize::ZeroizeOnDrop`, so the
/// plaintext does not linger in freed heap for the lifetime of the process
/// (modulo pre-existing copies — see the startup `TCODE_REMOTE_PASSWORD`
/// env-var path which is outside this type's control).
#[derive(Zeroize, ZeroizeOnDrop)]
pub(crate) struct Secret(String);

impl Secret {
    pub(crate) fn new(s: String) -> Self {
        Self(s)
    }

    /// Constant-time byte-wise compare of the stored secret against `candidate`
    /// **for candidates of the same length**.
    ///
    /// `subtle::ConstantTimeEq::ct_eq` on `[u8]` short-circuits to `Choice(0)`
    /// when the input lengths differ, so a length mismatch is detectable via
    /// timing. For this PoC the stored secret is a single pre-configured
    /// password; candidate-length is not a secret worth protecting. What we
    /// DO protect against is correct-prefix-vs-wrong-prefix distinction among
    /// same-length candidates — that is the practical timing oracle the
    /// attacker tries to exploit, and `ct_eq` defeats it.
    pub(crate) fn verify(&self, candidate: &[u8]) -> bool {
        self.0.as_bytes().ct_eq(candidate).into()
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
    /// Configured shared secret. Compared against incoming login payloads via
    /// [`Secret::verify`]; never exposed directly.
    pub(crate) password: Secret,
    /// Live session tokens.
    ///
    /// PoC note: we store the raw token strings. A memory-dump attacker with
    /// access to this set already has access to the single configured password
    /// too, so hashing the stored tokens does not change the threat model.
    /// Hash-at-rest with a constant-time compare is a later hardening step.
    // TODO(poc-limitation): `sessions` grows unbounded on repeated logins.
    // Acceptable for single-user PoC; a later hardening pass should add a
    // per-token TTL or a cap with oldest-eviction.
    sessions: parking_lot::RwLock<HashSet<String>>,
}

impl AppState {
    /// Test-only convenience: construct from a raw `String`. Production
    /// code goes through [`AppState::from_secret`] so the `Secret` wrapper
    /// travels the whole pipeline from `RemoteConfig`.
    #[cfg(test)]
    pub(crate) fn new(password: String) -> Self {
        Self::from_secret(Secret::new(password))
    }

    pub(crate) fn from_secret(password: Secret) -> Self {
        Self {
            password,
            sessions: parking_lot::RwLock::new(HashSet::new()),
        }
    }

    /// Mint a fresh random session token, store it, return the base64url
    /// (unpadded, `SESSION_TOKEN_B64_LEN`-char) string for the cookie value.
    ///
    /// Returns an error if the OS CSPRNG is unavailable; the caller maps that
    /// to HTTP 500. Do NOT log the returned token.
    pub(crate) fn mint_session(&self) -> Result<String, getrandom::Error> {
        let mut buf = [0u8; SESSION_TOKEN_BYTES];
        getrandom::fill(&mut buf)?;
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf);
        self.sessions.write().insert(token.clone());
        Ok(token)
    }

    /// Verify that `candidate` names a live session.
    ///
    /// PoC note: `HashSet::contains` does a non-constant-time equality
    /// check, but every stored token carries 256 bits of CSPRNG entropy —
    /// a remote attacker cannot feasibly guess even the first byte, so
    /// the timing side-channel has nothing to act on. A later hardening
    /// pass may switch to hash-stored tokens with constant-time compare.
    /// Do NOT log `candidate`.
    pub(crate) fn verify_session(&self, candidate: &str) -> bool {
        // Fast reject on obviously wrong lengths — protects the hash path
        // from pathological inputs and keeps the happy path tight.
        if candidate.len() != SESSION_TOKEN_B64_LEN {
            return false;
        }
        self.sessions.read().contains(candidate)
    }

    /// Remove `candidate` from the live session set, if present. Idempotent.
    /// Do NOT log `candidate`.
    pub(crate) fn revoke_session(&self, candidate: &str) {
        self.sessions.write().remove(candidate);
    }
}
