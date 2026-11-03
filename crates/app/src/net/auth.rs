//! Shared-secret auth: SHA-256(password) hex (empty when password not required).

use sha2::{Digest, Sha256};

pub fn password_token(password: &str, required: bool) -> String {
    if !required || password.is_empty() {
        return String::new();
    }
    hex::encode(Sha256::digest(password.as_bytes()))
}

pub fn tokens_match(ours: &str, theirs: &str, required: bool) -> bool {
    if !required {
        return true;
    }
    !ours.is_empty() && ours == theirs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_sha256_hex() {
        // sha256("secret")
        assert_eq!(
            password_token("secret", true),
            "2bb80d537b1da3e38bd30361aa855686bde0eacd7162fef6a25fe97bf527a25b"
        );
        assert_eq!(password_token("secret", true).len(), 64);
    }

    #[test]
    fn token_empty_when_not_required_or_no_password() {
        assert_eq!(password_token("secret", false), "");
        assert_eq!(password_token("", true), "");
    }

    #[test]
    fn matching_rules() {
        let ours = password_token("secret", true);
        assert!(tokens_match(&ours, &password_token("secret", true), true));
        assert!(!tokens_match(&ours, &password_token("Secret", true), true));
        assert!(!tokens_match(&ours, "", true));
        // Required but our own password empty → nobody gets in (no empty==empty match).
        assert!(!tokens_match("", "", true));
        // Not required → anything goes.
        assert!(tokens_match("", "whatever", false));
        assert!(tokens_match("", "", false));
    }
}
