use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Purpose {
    Play,
    Cache,
    Warm,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamClaims {
    #[serde(rename = "k")]
    pub release_key: String,
    #[serde(rename = "o")]
    pub output: usize,
    #[serde(rename = "p")]
    pub purpose: Purpose,
    #[serde(rename = "e")]
    pub expires_at: i64,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TokenError {
    #[error("malformed token")]
    Malformed,
    #[error("bad signature")]
    Signature,
    #[error("token expired")]
    Expired,
}

fn mac(secret: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut mac =
        <Hmac<Sha256> as KeyInit>::new_from_slice(secret).expect("hmac takes any key length");
    mac.update(payload);
    mac.finalize().into_bytes().to_vec()
}

pub fn sign(secret: &[u8], claims: &StreamClaims) -> String {
    let payload = serde_json::to_vec(claims).expect("claims serialize");
    let sig = mac(secret, &payload);
    format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(payload),
        URL_SAFE_NO_PAD.encode(sig)
    )
}

pub fn verify(secret: &[u8], token: &str, now: i64) -> Result<StreamClaims, TokenError> {
    let (payload, sig) = token.split_once('.').ok_or(TokenError::Malformed)?;
    let payload = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| TokenError::Malformed)?;
    let sig = URL_SAFE_NO_PAD
        .decode(sig)
        .map_err(|_| TokenError::Malformed)?;
    if !bool::from(mac(secret, &payload).ct_eq(&sig)) {
        return Err(TokenError::Signature);
    }
    let claims: StreamClaims =
        serde_json::from_slice(&payload).map_err(|_| TokenError::Malformed)?;
    if claims.expires_at <= now {
        return Err(TokenError::Expired);
    }
    Ok(claims)
}
