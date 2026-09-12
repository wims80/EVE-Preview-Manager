use std::collections::HashSet;
use std::time::{Duration, Instant};

use base64::Engine;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::Url;

pub const DISCOVERY_URL: &str =
    "https://login.eveonline.com/.well-known/oauth-authorization-server";
pub const REQUIRED_SCOPES: [&str; 2] = [
    "esi-location.read_location.v1",
    "esi-clones.read_implants.v1",
];

#[derive(Clone, Debug, Deserialize)]
pub struct Discovery {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub issuer: String,
    pub jwks_uri: String,
}

impl Discovery {
    pub fn validate(&self) -> Result<(), AuthError> {
        for raw in [
            &self.authorization_endpoint,
            &self.token_endpoint,
            &self.jwks_uri,
        ] {
            let url = Url::parse(raw).map_err(|_| AuthError::UnsafeMetadata)?;
            if url.scheme() != "https"
                || url.host_str().is_none()
                || url.username() != ""
                || url.password().is_some()
                || url.fragment().is_some()
            {
                return Err(AuthError::UnsafeMetadata);
            }
        }
        if self.issuer.trim().is_empty() {
            return Err(AuthError::UnsafeMetadata);
        }
        Ok(())
    }
}

pub struct AuthorizationAttempt {
    pub verifier: String,
    pub state: String,
    pub url: Url,
    pub expires_at: Instant,
    pub expected_character_id: Option<u64>,
    pub generation: u64,
}

pub fn begin_authorization(
    discovery: &Discovery,
    client_id: &str,
    callback: &str,
    expected_character_id: Option<u64>,
    generation: u64,
) -> Result<AuthorizationAttempt, AuthError> {
    discovery.validate()?;
    let callback = Url::parse(callback).map_err(|_| AuthError::InvalidCallback)?;
    if callback.scheme() != "http"
        || !matches!(callback.host_str(), Some("127.0.0.1" | "[::1]" | "::1"))
        || callback.port().is_none()
    {
        return Err(AuthError::InvalidCallback);
    }
    let verifier = random_urlsafe(64);
    let state = random_urlsafe(32);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    let mut url =
        Url::parse(&discovery.authorization_endpoint).map_err(|_| AuthError::UnsafeMetadata)?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", callback.as_str())
        .append_pair("client_id", client_id)
        .append_pair("scope", &REQUIRED_SCOPES.join(" "))
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state);
    Ok(AuthorizationAttempt {
        verifier,
        state,
        url,
        expires_at: Instant::now() + Duration::from_secs(300),
        expected_character_id,
        generation,
    })
}

fn random_urlsafe(bytes: usize) -> String {
    let mut data = vec![0_u8; bytes];
    rand::rng().fill_bytes(&mut data);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
}

#[derive(Debug, Deserialize)]
struct Claims {
    exp: u64,
    #[serde(rename = "iss")]
    _issuer: String,
    aud: Audiences,
    sub: String,
    scp: Scopes,
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Audiences {
    One(String),
    Many(Vec<String>),
}

impl Audiences {
    fn contains(&self, wanted: &str) -> bool {
        match self {
            Self::One(one) => one == wanted,
            Self::Many(many) => many.iter().any(|item| item == wanted),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Scopes {
    One(String),
    Many(Vec<String>),
}

impl Scopes {
    fn values(&self) -> HashSet<&str> {
        match self {
            Self::One(one) => one.split_ascii_whitespace().collect(),
            Self::Many(many) => many.iter().map(String::as_str).collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedCharacter {
    pub character_id: u64,
    pub name: String,
    pub expires_at_unix: u64,
}

pub fn validate_access_token(
    token: &str,
    jwks: &JwkSet,
    discovery_issuer: &str,
    client_id: &str,
    expected_character_id: Option<u64>,
) -> Result<VerifiedCharacter, AuthError> {
    let header = decode_header(token).map_err(|_| AuthError::MalformedToken)?;
    if header.alg != Algorithm::RS256 {
        return Err(AuthError::UnsupportedAlgorithm);
    }
    let kid = header.kid.ok_or(AuthError::UnknownKey)?;
    let jwk = jwks.find(&kid).ok_or(AuthError::UnknownKey)?;
    let key = DecodingKey::from_jwk(jwk).map_err(|_| AuthError::UnknownKey)?;
    let mut validation = Validation::new(Algorithm::RS256);
    validation.leeway = 0;
    validation.validate_exp = true;
    validation.validate_aud = false; // Both audiences are checked below; library audience checks are any-of.
    validation.set_issuer(&[
        discovery_issuer,
        "https://login.eveonline.com",
        "login.eveonline.com",
    ]);
    validation.required_spec_claims = ["exp", "iss", "aud", "sub"]
        .into_iter()
        .map(str::to_string)
        .collect();
    let claims = decode::<Claims>(token, &key, &validation)
        .map_err(|_| AuthError::InvalidToken)?
        .claims;
    if !claims.aud.contains("EVE Online") || !claims.aud.contains(client_id) {
        return Err(AuthError::WrongAudience);
    }
    let character_id = claims
        .sub
        .strip_prefix("CHARACTER:EVE:")
        .and_then(|id| id.parse().ok())
        .filter(|id: &u64| *id > 0)
        .ok_or(AuthError::WrongCharacter)?;
    if expected_character_id.is_some_and(|expected| expected != character_id) {
        return Err(AuthError::WrongCharacter);
    }
    if claims.name.trim().is_empty()
        || claims.name.len() > 128
        || claims.name.chars().any(char::is_control)
    {
        return Err(AuthError::WrongCharacter);
    }
    let scopes = claims.scp.values();
    if !REQUIRED_SCOPES.iter().all(|scope| scopes.contains(scope)) {
        return Err(AuthError::MissingScopes);
    }
    Ok(VerifiedCharacter {
        character_id,
        name: claims.name,
        expires_at_unix: claims.exp,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthError {
    Unavailable,
    UnsafeMetadata,
    InvalidCallback,
    MalformedToken,
    UnsupportedAlgorithm,
    UnknownKey,
    InvalidToken,
    WrongAudience,
    WrongCharacter,
    MissingScopes,
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{EncodingKey, Header, encode};
    use serde::Serialize;

    fn discovery() -> Discovery {
        Discovery {
            authorization_endpoint: "https://login.eveonline.com/v2/oauth/authorize".into(),
            token_endpoint: "https://login.eveonline.com/v2/oauth/token".into(),
            issuer: "https://login.eveonline.com".into(),
            jwks_uri: "https://login.eveonline.com/oauth/jwks".into(),
        }
    }

    #[test]
    fn pkce_and_state_are_random_and_exact_scopes_are_requested() {
        let a = begin_authorization(
            &discovery(),
            "client",
            "http://127.0.0.1:28761/callback",
            None,
            1,
        )
        .unwrap();
        let b = begin_authorization(
            &discovery(),
            "client",
            "http://127.0.0.1:28761/callback",
            None,
            1,
        )
        .unwrap();
        assert_ne!(a.verifier, b.verifier);
        assert_ne!(a.state, b.state);
        let pairs: std::collections::HashMap<_, _> = a.url.query_pairs().into_owned().collect();
        assert_eq!(pairs["scope"], REQUIRED_SCOPES.join(" "));
        assert_eq!(pairs["code_challenge_method"], "S256");
        assert!(!pairs.contains_key("client_secret"));
    }

    #[test]
    fn rejects_non_loopback_callback_and_unsafe_metadata() {
        assert!(matches!(
            begin_authorization(&discovery(), "c", "https://example.com/cb", None, 1),
            Err(AuthError::InvalidCallback)
        ));
        let mut bad = discovery();
        bad.token_endpoint = "http://login.eveonline.com/token".into();
        assert_eq!(bad.validate(), Err(AuthError::UnsafeMetadata));
    }

    #[test]
    fn refresh_form_encoding_preserves_special_characters() {
        let form = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("grant_type", "refresh_token")
            .append_pair("refresh_token", "+/=")
            .append_pair("client_id", "client")
            .finish();
        assert!(form.contains("refresh_token=%2B%2F%3D"));
    }

    #[derive(Serialize)]
    struct TestClaims<'a> {
        exp: u64,
        iss: &'a str,
        aud: Vec<&'a str>,
        sub: &'a str,
        scp: Vec<&'a str>,
        name: &'a str,
    }

    fn signed(claims: &TestClaims<'_>) -> (String, JwkSet) {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("rsa01".into());
        let private = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/esi-test-private.pem"
        ));
        let token = encode(
            &header,
            claims,
            &EncodingKey::from_rsa_pem(private).unwrap(),
        )
        .unwrap();
        let jwks: JwkSet = serde_json::from_value(serde_json::json!({"keys": [{
            "kty":"RSA", "kid":"rsa01", "alg":"RS256", "use":"sig", "e":"AQAB",
            "n":"yRE6rHuNR0QbHO3H3Kt2pOKGVhQqGZXInOduQNxXzuKlvQTLUTv4l4sggh5_CYYi_cvI-SXVT9kPWSKXxJXBXd_4LkvcPuUakBoAkfh-eiFVMh2VrUyWyj3MFl0HTVF9KwRXLAcwkREiS3npThHRyIxuy0ZMeZfxVL5arMhw1SRELB8HoGfG_AtH89BIE9jDBHZ9dLelK9a184zAf8LwoPLxvJb3Il5nncqPcSfKDDodMFBIMc4lQzDKL5gvmiXLXB1AGLm8KBjfE8s3L5xqi-yUod-j8MtvIj812dkS4QMiRVN_by2h3ZY8LYVGrqZXZTcgn2ujn8uKjXLZVD5TdQ"
        }]})).unwrap();
        (token, jwks)
    }

    fn valid_claims() -> TestClaims<'static> {
        TestClaims {
            exp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                + 600,
            iss: "https://login.eveonline.com",
            aud: vec!["EVE Online", "client"],
            sub: "CHARACTER:EVE:42",
            scp: REQUIRED_SCOPES.to_vec(),
            name: "Capsuleer",
        }
    }

    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn signed_token_requires_both_audiences_character_and_scopes() {
        let claims = valid_claims();
        let (token, jwks) = signed(&claims);
        assert_eq!(
            validate_access_token(&token, &jwks, claims.iss, "client", Some(42))
                .unwrap()
                .character_id,
            42
        );

        let mut wrong = valid_claims();
        wrong.aud = vec!["EVE Online"];
        let (token, jwks) = signed(&wrong);
        assert_eq!(
            validate_access_token(&token, &jwks, wrong.iss, "client", None),
            Err(AuthError::WrongAudience)
        );

        let mut wrong = valid_claims();
        wrong.scp.pop();
        let (token, jwks) = signed(&wrong);
        assert_eq!(
            validate_access_token(&token, &jwks, wrong.iss, "client", None),
            Err(AuthError::MissingScopes)
        );
        let (token, jwks) = signed(&valid_claims());
        assert_eq!(
            validate_access_token(
                &token,
                &jwks,
                "https://login.eveonline.com",
                "client",
                Some(9)
            ),
            Err(AuthError::WrongCharacter)
        );
    }

    #[test]
    fn signed_token_rejects_expiry_and_accepts_documented_issuer_variant() {
        let mut claims = valid_claims();
        claims.exp = 1;
        let (token, jwks) = signed(&claims);
        assert_eq!(
            validate_access_token(&token, &jwks, claims.iss, "client", None),
            Err(AuthError::InvalidToken)
        );
        let mut claims = valid_claims();
        claims.iss = "login.eveonline.com";
        let (token, jwks) = signed(&claims);
        assert!(
            validate_access_token(&token, &jwks, "https://login.eveonline.com", "client", None)
                .is_ok()
        );
    }
}
