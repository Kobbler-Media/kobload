use usenet_proto::{Purpose, StreamClaims, TokenError, UsenetServer, sign, verify};

fn claims(expires_at: i64) -> StreamClaims {
    StreamClaims {
        release_key: "nzb-ab".into(),
        output: 2,
        purpose: Purpose::Cache,
        expires_at,
    }
}

#[test]
fn a_signed_token_round_trips_its_claims() {
    let token = sign(b"secret", &claims(200));
    assert_eq!(verify(b"secret", &token, 100), Ok(claims(200)));
}

#[test]
fn tokens_are_refused_when_forged_tampered_or_expired() {
    let token = sign(b"secret", &claims(200));
    assert_eq!(verify(b"other", &token, 100), Err(TokenError::Signature));
    let (_, sig) = token.split_once('.').unwrap();
    let forged = sign(
        b"secret",
        &StreamClaims {
            purpose: Purpose::Play,
            ..claims(200)
        },
    );
    let (payload, _) = forged.split_once('.').unwrap();
    assert_eq!(
        verify(b"secret", &format!("{payload}.{sig}"), 100),
        Err(TokenError::Signature)
    );
    assert_eq!(verify(b"secret", &token, 200), Err(TokenError::Expired));
    assert_eq!(verify(b"secret", "nodot", 100), Err(TokenError::Malformed));
}

#[test]
fn server_debug_never_prints_credentials() {
    let server: UsenetServer = serde_json::from_value(serde_json::json!({
        "id": "a", "name": "n", "host": "h", "port": 563, "tls": true, "verify_cert": true,
        "connections": 10, "priority": 0, "username": "user-secret", "password": "pass-secret"
    }))
    .unwrap();
    let shown = format!("{server:?}");
    assert!(!shown.contains("pass-secret") && !shown.contains("user-secret"));
}
