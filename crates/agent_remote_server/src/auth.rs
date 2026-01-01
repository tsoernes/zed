use rand::Rng;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Authentication token for remote connections
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AuthToken(String);

impl AuthToken {
    /// Generate a new random authentication token
    pub fn generate() -> Self {
        let mut rng = rand::rng();
        let token: String = (0..32)
            .map(|_| {
                let idx = rng.random_range(0..62);
                match idx {
                    0..=9 => (b'0' + idx) as char,
                    10..=35 => (b'a' + idx - 10) as char,
                    36..=61 => (b'A' + idx - 36) as char,
                    _ => unreachable!(),
                }
            })
            .collect();
        Self(token)
    }

    /// Create token from string (for testing or manual entry)
    pub fn from_string(s: String) -> Self {
        Self(s)
    }

    /// Get token as string slice
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Validate token against expected value
    pub fn validate(&self, expected: &AuthToken) -> bool {
        // Constant-time comparison to prevent timing attacks
        self.0.len() == expected.0.len()
            && self
                .0
                .bytes()
                .zip(expected.0.bytes())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0
    }
}

impl fmt::Display for AuthToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Debug for AuthToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AuthToken(***)")
    }
}

/// Manages authentication tokens for remote connections
pub struct TokenManager {
    current_token: AuthToken,
}

impl TokenManager {
    /// Create new token manager with a random token
    pub fn new() -> Self {
        Self {
            current_token: AuthToken::generate(),
        }
    }

    /// Get current authentication token
    pub fn current_token(&self) -> &AuthToken {
        &self.current_token
    }

    /// Regenerate the authentication token
    pub fn regenerate(&mut self) -> &AuthToken {
        self.current_token = AuthToken::generate();
        &self.current_token
    }

    /// Validate an incoming token
    pub fn validate(&self, token: &AuthToken) -> bool {
        token.validate(&self.current_token)
    }
}

impl Default for TokenManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_generation() {
        let token = AuthToken::generate();
        assert_eq!(token.as_str().len(), 32);
    }

    #[test]
    fn test_token_validation() {
        let token1 = AuthToken::from_string("test123".to_string());
        let token2 = AuthToken::from_string("test123".to_string());
        let token3 = AuthToken::from_string("different".to_string());

        assert!(token1.validate(&token2));
        assert!(!token1.validate(&token3));
    }

    #[test]
    fn test_token_manager() {
        let mut manager = TokenManager::new();
        let token = manager.current_token().clone();

        assert!(manager.validate(&token));

        let new_token = manager.regenerate().clone();
        assert!(!manager.validate(&token));
        assert!(manager.validate(&new_token));
    }
}
