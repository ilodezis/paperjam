use secrecy::SecretString;
use serde::{Deserialize, Serialize};

use crate::ids::{PasskeyId, PersonId, TokenId, UserId};

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct User {
    pub id: UserId,
    pub username: String,
    pub display_name: String,
    pub locale: String,
    pub person_id: Option<PersonId>,
    pub is_admin: bool,
    pub created_at: String,
    pub disabled_at: Option<String>,
}

#[derive(Debug)]
pub struct NewUser {
    pub username: String,
    pub display_name: String,
    pub password: SecretString,
    pub locale: String,
    pub is_admin: bool,
}

/// An API token as shown in lists. The secret value is never stored or
/// returned again after creation.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct TokenView {
    pub id: TokenId,
    pub user_id: UserId,
    pub name: String,
    pub prefix: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
}

/// Opaque value for the `pj_session` cookie: `<session id>.<signature>`.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionToken(pub String);

impl std::fmt::Debug for SessionToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionToken(..)")
    }
}

/// A passkey as shown in the list of the signed-in user. The key material
/// stays in the database.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct PasskeyView {
    pub id: PasskeyId,
    /// The WebAuthn credential id, base64url as the browser spells it.
    pub credential_id: String,
    pub label: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
}
