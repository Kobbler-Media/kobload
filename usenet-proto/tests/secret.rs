use usenet_proto::{SecretError, check_secrets, keys_match};

const A: &str = "0123456789abcdef0123456789abcdef";
const B: &str = "fedcba9876543210fedcba9876543210";

#[test]
fn keys_match_only_the_same_key() {
    assert!(keys_match(A, A));
    assert!(!keys_match(B, A));
    assert!(!keys_match(&A[..31], A));
    assert!(!keys_match("", A));
}

#[test]
fn secrets_must_be_long_and_distinct() {
    assert_eq!(check_secrets(&[("C", A), ("S", B)]), Ok(()));
    assert_eq!(
        check_secrets(&[("C", A), ("S", "short")]),
        Err(SecretError::TooShort("S"))
    );
    assert_eq!(
        check_secrets(&[("C", A), ("S", B), ("M", A)]),
        Err(SecretError::Reused("C", "M"))
    );
}
