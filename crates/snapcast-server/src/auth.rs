//! Streaming client authentication.
//!
//! Implement [`AuthValidator`] for custom authentication (database, LDAP, etc.)
//! or use [`StaticAuthValidator`] for config-file-based users/roles.
//!
//! Implement [`ClientFilter`] for connection-level filtering (MAC allowlist, etc.)

use std::collections::HashMap;
use std::sync::Arc;

use subtle::ConstantTimeEq;

use snapcast_proto::message::hello::Hello;

/// Constant-time byte comparison to prevent timing attacks.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.ct_eq(b).into()
}

/// Connection-level client filter — called after Hello, before authentication.
///
/// Use this to implement MAC allowlists, IP-based filtering, or rate limiting.
/// Return `true` to accept the client, `false` to disconnect immediately.
///
/// # Example: MAC allowlist
///
/// ```
/// use snapcast_server::auth::ClientFilter;
/// use snapcast_server::Hello;
///
/// struct MacAllowlist(Vec<String>);
///
/// impl ClientFilter for MacAllowlist {
///     fn accept(&self, hello: &Hello) -> bool {
///         self.0.is_empty() || self.0.iter().any(|m| m.eq_ignore_ascii_case(&hello.mac))
///     }
/// }
/// ```
pub trait ClientFilter: Send + Sync {
    /// Decide whether to accept a client based on its Hello message.
    fn accept(&self, hello: &Hello) -> bool;
}

/// Result of successful authentication.
#[derive(Debug, Clone)]
pub struct AuthResult {
    /// Authenticated username.
    pub username: String,
    /// Granted permissions (e.g. "Streaming", "Control").
    pub permissions: Vec<String>,
}

/// Authentication error.
#[derive(Debug, Clone)]
pub enum AuthError {
    /// 401 — invalid or missing credentials.
    Unauthorized(String),
    /// 403 — authenticated but lacking required permission.
    Forbidden(String),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthorized(msg) => write!(f, "Unauthorized: {msg}"),
            Self::Forbidden(msg) => write!(f, "Forbidden: {msg}"),
        }
    }
}

impl std::error::Error for AuthError {}

impl AuthError {
    /// HTTP-style error code.
    pub fn code(&self) -> i32 {
        match self {
            Self::Unauthorized(_) => 401,
            Self::Forbidden(_) => 403,
        }
    }

    /// Error message.
    pub fn message(&self) -> &str {
        match self {
            Self::Unauthorized(msg) | Self::Forbidden(msg) => msg,
        }
    }
}

///
/// The server calls [`validate`](AuthValidator::validate) after receiving a Hello message.
/// Return [`AuthResult`] on success or [`AuthError`] on failure.
///
/// # Example: Custom validator
///
/// ```
/// use snapcast_server::auth::{AuthValidator, AuthResult, AuthError};
///
/// struct MyValidator;
///
/// impl AuthValidator for MyValidator {
///     fn validate(&self, scheme: &str, param: &str) -> Result<AuthResult, AuthError> {
///         // Look up in database, LDAP, etc.
///         Ok(AuthResult {
///             username: "user".into(),
///             permissions: vec!["Streaming".into()],
///         })
///     }
/// }
/// ```
/// Trait for validating streaming client credentials.
pub trait AuthValidator: Send + Sync {
    /// Validate credentials from the Hello message's auth field.
    fn validate(&self, scheme: &str, param: &str) -> Result<AuthResult, AuthError>;
}

/// Permission required for streaming clients.
pub const PERM_STREAMING: &str = "Streaming";

/// A role with named permissions.
#[derive(Debug, Clone)]
pub struct Role {
    /// Role name.
    pub name: String,
    /// Granted permissions.
    pub permissions: Vec<String>,
}

/// A user with credentials and role assignment.
#[derive(Debug, Clone)]
pub struct User {
    /// Username.
    pub name: String,
    /// Password (plaintext — hashing is the deployer's responsibility).
    pub password: String,
    /// Role name.
    pub role: String,
}

/// Config-file-based authentication matching the C++ implementation.
///
/// Validates Basic auth (`base64(user:password)`) against a static user/role list.
///
/// Construction behavior:
/// - Users that reference an unknown role are assigned an empty permission set.
/// - Duplicate usernames are resolved by last-write-wins insertion into the map.
#[derive(Debug, Clone)]
pub struct StaticAuthValidator {
    users: HashMap<String, (String, Arc<Role>)>, // name → (password, role)
}

impl StaticAuthValidator {
    /// Create from user and role lists.
    pub fn new(users: Vec<User>, roles: Vec<Role>) -> Self {
        let role_map: HashMap<String, Arc<Role>> = roles
            .into_iter()
            .map(|r| (r.name.clone(), Arc::new(r)))
            .collect();
        let empty_role = Arc::new(Role {
            name: String::new(),
            permissions: vec![],
        });
        let user_map = users
            .into_iter()
            .map(|u| {
                let role = role_map
                    .get(&u.role)
                    .cloned()
                    .unwrap_or_else(|| empty_role.clone());
                (u.name, (u.password, role))
            })
            .collect();
        Self { users: user_map }
    }
}

impl AuthValidator for StaticAuthValidator {
    fn validate(&self, scheme: &str, param: &str) -> Result<AuthResult, AuthError> {
        if !scheme.eq_ignore_ascii_case("basic") {
            return Err(AuthError::Unauthorized(format!(
                "Unsupported auth scheme: {scheme}"
            )));
        }

        // Decode base64(user:password)
        use base64::Engine;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(param)
            .map_err(|_| AuthError::Unauthorized("Invalid base64".into()))?;
        let decoded = String::from_utf8(decoded)
            .map_err(|_| AuthError::Unauthorized("Invalid UTF-8".into()))?;
        let (username, password) = decoded
            .split_once(':')
            .ok_or_else(|| AuthError::Unauthorized("Expected user:password".into()))?;

        let (stored_pw, role) = self
            .users
            .get(username)
            .ok_or_else(|| AuthError::Unauthorized("Unknown user".into()))?;

        if !constant_time_eq(stored_pw.as_bytes(), password.as_bytes()) {
            return Err(AuthError::Unauthorized("Wrong password".into()));
        }

        Ok(AuthResult {
            username: username.to_string(),
            permissions: role.permissions.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_validator() -> StaticAuthValidator {
        StaticAuthValidator::new(
            vec![
                User {
                    name: "admin".into(),
                    password: "secret".into(),
                    role: "full".into(),
                },
                User {
                    name: "player".into(),
                    password: "play".into(),
                    role: "streaming".into(),
                },
            ],
            vec![
                Role {
                    name: "full".into(),
                    permissions: vec!["Streaming".into(), "Control".into()],
                },
                Role {
                    name: "streaming".into(),
                    permissions: vec!["Streaming".into()],
                },
            ],
        )
    }

    fn basic(user: &str, pass: &str) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"))
    }

    #[test]
    fn valid_credentials() {
        let v = test_validator();
        let result = v.validate("Basic", &basic("admin", "secret")).unwrap();
        assert_eq!(result.username, "admin");
        assert!(result.permissions.contains(&"Streaming".into()));
        assert!(result.permissions.contains(&"Control".into()));
    }

    #[test]
    fn wrong_password() {
        let v = test_validator();
        let err = v.validate("Basic", &basic("admin", "wrong")).unwrap_err();
        assert_eq!(err.code(), 401);
    }

    #[test]
    fn unknown_user() {
        let v = test_validator();
        let err = v.validate("Basic", &basic("nobody", "x")).unwrap_err();
        assert_eq!(err.code(), 401);
    }

    #[test]
    fn unsupported_scheme() {
        let v = test_validator();
        let err = v.validate("Bearer", "token123").unwrap_err();
        assert_eq!(err.code(), 401);
    }

    #[test]
    fn scheme_is_case_insensitive() {
        let v = test_validator();
        let result = v.validate("bAsIc", &basic("admin", "secret")).unwrap();
        assert_eq!(result.username, "admin");
    }

    #[test]
    fn streaming_only_user() {
        let v = test_validator();
        let result = v.validate("Basic", &basic("player", "play")).unwrap();
        assert_eq!(result.username, "player");
        assert!(result.permissions.contains(&"Streaming".into()));
        assert!(!result.permissions.contains(&"Control".into()));
    }

    #[test]
    fn invalid_base64_is_rejected() {
        let v = test_validator();
        let err = v.validate("Basic", "not-base64%%%").unwrap_err();
        assert_eq!(err.code(), 401);
        assert_eq!(err.message(), "Invalid base64");
    }

    #[test]
    fn invalid_utf8_is_rejected() {
        use base64::Engine;
        let v = test_validator();
        let invalid_utf8 = base64::engine::general_purpose::STANDARD.encode([0xFF, 0xFE, 0xFD]);
        let err = v.validate("Basic", &invalid_utf8).unwrap_err();
        assert_eq!(err.code(), 401);
        assert_eq!(err.message(), "Invalid UTF-8");
    }

    #[test]
    fn missing_colon_is_rejected() {
        use base64::Engine;
        let v = test_validator();
        let no_colon = base64::engine::general_purpose::STANDARD.encode("adminsecret");
        let err = v.validate("Basic", &no_colon).unwrap_err();
        assert_eq!(err.code(), 401);
        assert_eq!(err.message(), "Expected user:password");
    }

    #[test]
    fn unknown_role_gets_empty_permissions() {
        let v = StaticAuthValidator::new(
            vec![User {
                name: "ghost".into(),
                password: "pw".into(),
                role: "missing-role".into(),
            }],
            vec![],
        );
        let result = v.validate("Basic", &basic("ghost", "pw")).unwrap();
        assert_eq!(result.username, "ghost");
        assert!(result.permissions.is_empty());
    }

    #[test]
    fn duplicate_username_last_entry_wins() {
        let v = StaticAuthValidator::new(
            vec![
                User {
                    name: "dup".into(),
                    password: "first".into(),
                    role: "streaming".into(),
                },
                User {
                    name: "dup".into(),
                    password: "second".into(),
                    role: "full".into(),
                },
            ],
            vec![
                Role {
                    name: "full".into(),
                    permissions: vec!["Streaming".into(), "Control".into()],
                },
                Role {
                    name: "streaming".into(),
                    permissions: vec!["Streaming".into()],
                },
            ],
        );
        // First password should fail.
        assert!(v.validate("Basic", &basic("dup", "first")).is_err());
        // Last entry should succeed with full role permissions.
        let result = v.validate("Basic", &basic("dup", "second")).unwrap();
        assert!(result.permissions.contains(&"Streaming".into()));
        assert!(result.permissions.contains(&"Control".into()));
    }

    #[test]
    fn auth_error_message_returns_inner_text() {
        let e1 = AuthError::Unauthorized("u-msg".into());
        assert_eq!(e1.message(), "u-msg");
        let e2 = AuthError::Forbidden("f-msg".into());
        assert_eq!(e2.message(), "f-msg");
    }
}
