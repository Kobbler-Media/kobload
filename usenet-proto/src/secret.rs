use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub const MIN_SECRET_BYTES: usize = 32;

// Comparing digests keeps the comparison fixed-length, so its timing leaks neither the
// expected key's length nor how much of a prefix matched.
#[must_use]
pub fn keys_match(presented: &str, expected: &str) -> bool {
    let presented = Sha256::digest(presented.as_bytes());
    let expected = Sha256::digest(expected.as_bytes());
    bool::from(presented.as_slice().ct_eq(expected.as_slice()))
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SecretError {
    #[error("{0} must be at least {MIN_SECRET_BYTES} bytes")]
    TooShort(&'static str),
    #[error("{0} and {1} must differ")]
    Reused(&'static str, &'static str),
}

pub fn check_secrets(secrets: &[(&'static str, &str)]) -> Result<(), SecretError> {
    for (name, value) in secrets {
        if value.len() < MIN_SECRET_BYTES {
            return Err(SecretError::TooShort(name));
        }
    }
    for (i, (a, va)) in secrets.iter().enumerate() {
        if let Some((b, _)) = secrets[i + 1..].iter().find(|(_, vb)| vb == va) {
            return Err(SecretError::Reused(a, b));
        }
    }
    Ok(())
}
