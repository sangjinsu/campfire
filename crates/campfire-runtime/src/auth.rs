use serde_json::Value;
use std::{fmt, future::Future, pin::Pin, time::Duration};

/// Verified host data; Debug intentionally omits all identity fields.
#[derive(Clone, PartialEq)]
pub struct AccountIdentity {
    pub issuer: String,
    pub subject: String,
    pub claims: Value,
}
impl fmt::Debug for AccountIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AccountIdentity { [redacted] }")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PlayerIdentity {
    Guest,
    Account(AccountIdentity),
}

/// Lifetime measured from the start of the authentication request.
#[derive(Clone, Debug)]
pub struct AccountGrant {
    pub identity: AccountIdentity,
    pub valid_for: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthError {
    InvalidCredential,
    Unavailable,
}
impl fmt::Display for AuthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidCredential => "credential was rejected",
            Self::Unavailable => "account authentication is unavailable",
        })
    }
}
impl std::error::Error for AuthError {}

/// Trusted host verifier. Credentials must never be persisted or logged.
pub trait AccountAuthenticator: Send + Sync + 'static {
    fn authenticate<'a>(
        &'a self,
        game_id: &'a str,
        credential: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<AccountGrant, AuthError>> + Send + 'a>>;
}
