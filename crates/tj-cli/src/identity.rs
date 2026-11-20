//! Who is calling (docs/access-control.md): sign-in tokens from OpenID Connect providers,
//! API keys, `tj dev --as`, and anonymous callers. A credential that is present but invalid
//! gets 401; it never falls back to anonymous.

use jsonwebtoken::jwk::{AlgorithmParameters, EllipticCurve, Jwk, JwkSet};
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use ring::digest::{digest, SHA256};
use serde_json::{json, Value as Json};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tj_core::runtime::protocol::Caller;

pub const KEYS_ENV: &str = "TJ_KEYS";
pub const ISSUER_ENV: &str = "TJ_ISSUER";
pub const AUDIENCE_ENV: &str = "TJ_AUDIENCE";
pub const ROLES_CLAIM_ENV: &str = "TJ_ROLES_CLAIM";
/// Clock difference tolerated between this server and the sign-in provider.
const LEEWAY_S: u64 = 60;
/// A token naming an unknown key refetches the provider's keys, at most this often.
const REFETCH_EVERY: Duration = Duration::from_secs(60);
const MAX_DOCUMENT: u64 = 1 << 20;
const KEY_PREFIX: &str = "tjk_";
const RELOAD_EVERY: Duration = Duration::from_secs(30);
/// Words that rules use, which no credential may claim as a role.
const NOT_ROLES: &[&str] = &["system", "anyone", "signedIn", "owner"];

#[derive(Clone)]
struct Key {
    name: String,
    roles: Vec<String>,
    sha256: String,
}

struct KeyFile {
    path: PathBuf,
    keys: Vec<Key>,
    loaded: Option<Instant>,
}

/// Sign-in tokens (JWTs) from one OpenID Connect provider: its signing keys come from its
/// discovery document, over HTTPS, and are cached.
struct Oidc {
    issuer: String,
    audience: String,
    /// A dotted path to the roles in the token, e.g. `realm_access.roles`; none by default.
    roles_claim: Option<String>,
    keys: Mutex<(JwkSet, Option<Instant>)>,
    agent: ureq::Agent,
}

/// HTTPS only, except for this machine (local test issuers).
fn safe_url(url: &str) -> Result<(), String> {
    let local = ["http://127.0.0.1", "http://localhost", "http://[::1]"].iter().any(|p| url.starts_with(p));
    if url.starts_with("https://") || local {
        Ok(())
    } else {
        Err(format!("{url} must be https"))
    }
}

impl Oidc {
    fn new(issuer: &str, audience: &str, roles_claim: Option<String>) -> Result<Oidc, String> {
        let issuer = issuer.trim_end_matches('/').to_string();
        safe_url(&issuer).map_err(|e| format!("{ISSUER_ENV}: {e}"))?;
        let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(10)).build();
        Ok(Oidc { issuer, audience: audience.to_string(), roles_claim, keys: Mutex::new((JwkSet { keys: Vec::new() }, None)), agent })
    }

    fn get_json(&self, url: &str) -> Result<Json, String> {
        safe_url(url)?;
        let resp = self.agent.get(url).call().map_err(|e| format!("cannot fetch {url}: {e}"))?;
        let mut text = String::new();
        resp.into_reader().take(MAX_DOCUMENT).read_to_string(&mut text).map_err(|e| format!("cannot read {url}: {e}"))?;
        serde_json::from_str(&text).map_err(|e| format!("{url} is not JSON: {e}"))
    }

    /// The provider's current signing keys, from its discovery document.
    fn fetch(&self) -> Result<JwkSet, String> {
        let doc = self.get_json(&format!("{}/.well-known/openid-configuration", self.issuer))?;
        if doc["issuer"].as_str().map(|i| i.trim_end_matches('/')) != Some(self.issuer.as_str()) {
            return Err(format!("the discovery document of {} names another issuer", self.issuer));
        }
        let uri = doc["jwks_uri"].as_str().ok_or("the discovery document has no jwks_uri")?;
        serde_json::from_value(self.get_json(uri)?).map_err(|e| format!("unreadable signing keys at {uri}: {e}"))
    }

    fn prefetch(&self) -> Result<(), String> {
        let set = self.fetch()?;
        let mut cache = self.keys.lock().map_err(|_| "key cache poisoned")?;
        *cache = (set, Some(Instant::now()));
        Ok(())
    }

    /// The key a token names; an unknown one refetches the keys (at most once a minute).
    fn key(&self, kid: Option<&str>) -> Result<Jwk, String> {
        let mut cache = self.keys.lock().map_err(|_| "key cache poisoned")?;
        let find = |set: &JwkSet| match kid {
            Some(k) => set.find(k).cloned(),
            None if set.keys.len() == 1 => set.keys.first().cloned(),
            None => None,
        };
        if let Some(k) = find(&cache.0) {
            return Ok(k);
        }
        if cache.1.is_some_and(|t| t.elapsed() < REFETCH_EVERY) {
            return Err("the token names an unknown signing key".into());
        }
        cache.1 = Some(Instant::now());
        cache.0 = self.fetch()?;
        find(&cache.0).ok_or_else(|| "the token names an unknown signing key".to_string())
    }

    fn verify(&self, token: &str) -> Result<Verified, String> {
        let header = decode_header(token).map_err(|e| format!("not a sign-in token: {e}"))?;
        if !matches!(header.alg, Algorithm::RS256 | Algorithm::ES256) {
            return Err("only RS256 and ES256 tokens are accepted".into());
        }
        let jwk = self.key(header.kid.as_deref())?;
        // The key decides the algorithm: an RSA key verifies RS256 only, a P-256 key ES256 only.
        let fits = match (&jwk.algorithm, header.alg) {
            (AlgorithmParameters::RSA(_), Algorithm::RS256) => true,
            (AlgorithmParameters::EllipticCurve(p), Algorithm::ES256) => p.curve == EllipticCurve::P256,
            _ => false,
        };
        if !fits {
            return Err("the token's algorithm does not match its key".into());
        }
        let key = DecodingKey::from_jwk(&jwk).map_err(|e| format!("unusable signing key: {e}"))?;
        let mut v = Validation::new(header.alg);
        v.leeway = LEEWAY_S;
        v.validate_nbf = true;
        v.set_issuer(&[&self.issuer]);
        v.set_audience(&[&self.audience]);
        v.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        let claims = decode::<Json>(token, &key, &v).map_err(|e| format!("token rejected: {e}"))?.claims;
        // A token for several audiences must name this one as the party it was issued to.
        if claims["aud"].as_array().is_some_and(|a| a.len() > 1) && claims["azp"].as_str() != Some(self.audience.as_str()) {
            return Err("token rejected: issued to another party".into());
        }
        let sub = claims["sub"].as_str().filter(|s| !s.is_empty()).ok_or("token rejected: no subject")?;
        let roles = match &self.roles_claim {
            Some(path) => {
                let found = path.split('.').fold(&claims, |c, part| &c[part]);
                found.as_array().map(|a| a.iter().filter_map(|r| r.as_str().map(str::to_string)).collect()).unwrap_or_default()
            }
            None => Vec::new(),
        };
        let expires = claims["exp"].as_u64().map(|s| UNIX_EPOCH + Duration::from_secs(s));
        Ok(Verified { caller: Caller::Remote { id: format!("user:{sub}"), roles: clean_roles(roles) }, expires })
    }
}

pub struct Identity {
    oidc: Option<Oidc>,
    keys: Option<Mutex<KeyFile>>,
    /// `tj dev --as name:role,role`: the identity of requests without credentials.
    dev_as: Option<Caller>,
}

/// A verified caller, and when its credential stops being valid (re-checked on streams).
pub struct Verified {
    pub caller: Caller,
    pub expires: Option<SystemTime>,
}

pub fn sha256_hex(text: &str) -> String {
    hex(digest(&SHA256, text.as_bytes()).as_ref())
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Bytes from the system's secure random source.
pub fn random<const N: usize>() -> Result<[u8; N], String> {
    let mut bytes = [0u8; N];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes).map_err(|_| "no secure random source".to_string())?;
    Ok(bytes)
}

fn clean_roles(roles: Vec<String>) -> Vec<String> {
    roles.into_iter().filter(|r| !NOT_ROLES.contains(&r.as_str())).collect()
}

fn read_keys(path: &Path) -> Result<Vec<Key>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read keys file {}: {e}", path.display()))?;
    let list: Json = serde_json::from_str(&text).map_err(|e| format!("keys file {} is not JSON: {e}", path.display()))?;
    let mut keys = Vec::new();
    for k in list.as_array().ok_or(format!("keys file {} must be a JSON array", path.display()))? {
        let field = |f: &str| k[f].as_str().map(str::to_string).ok_or(format!("a key in {} has no \"{f}\"", path.display()));
        let roles = k["roles"].as_array().map(|a| a.iter().filter_map(|r| r.as_str().map(str::to_string)).collect()).unwrap_or_default();
        keys.push(Key { name: field("name")?, roles: clean_roles(roles), sha256: field("sha256")?.to_lowercase() });
    }
    Ok(keys)
}

impl Identity {
    /// From the environment (`TJ_KEYS`) and `tj dev --as`.
    pub fn new(dev_as: Option<&str>) -> Result<Identity, String> {
        let keys = match std::env::var(KEYS_ENV) {
            Ok(p) if !p.is_empty() => {
                let path = PathBuf::from(p);
                let keys = read_keys(&path)?;
                Some(Mutex::new(KeyFile { path, keys, loaded: Some(Instant::now()) }))
            }
            _ => None,
        };
        let dev_as = dev_as.map(parse_as).transpose()?;
        let oidc = match std::env::var(ISSUER_ENV) {
            Ok(issuer) if !issuer.is_empty() => {
                let audience = std::env::var(AUDIENCE_ENV).ok().filter(|a| !a.is_empty()).ok_or(format!("{ISSUER_ENV} needs {AUDIENCE_ENV}: the client id tokens must be issued for"))?;
                let oidc = Oidc::new(&issuer, &audience, std::env::var(ROLES_CLAIM_ENV).ok().filter(|r| !r.is_empty()))?;
                // Fetch the keys now, so a wrong issuer shows at startup; tokens retry later.
                if let Err(e) = oidc.prefetch() {
                    eprintln!("[tj] sign-in: {e}");
                }
                Some(oidc)
            }
            _ => None,
        };
        Ok(Identity { oidc, keys, dev_as })
    }

    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(o) = &self.oidc {
            parts.push(format!("sign-in tokens from {}", o.issuer));
        }
        if self.keys.is_some() {
            parts.push("API keys".to_string());
        }
        if let Some(Caller::Remote { id, .. }) = &self.dev_as {
            parts.push(format!("requests without credentials act as {id}"));
        }
        if parts.is_empty() {
            "anonymous only (set TJ_KEYS for API keys)".into()
        } else {
            parts.join("; ")
        }
    }

    /// Who sent a request with this `Authorization` header.
    pub fn verify(&self, authorization: Option<&str>) -> Result<Verified, (u16, String)> {
        let Some(header) = authorization else {
            let caller = self.dev_as.clone().unwrap_or(Caller::Remote { id: String::new(), roles: Vec::new() });
            return Ok(Verified { caller, expires: None });
        };
        let Some(credential) = header.strip_prefix("Bearer ").map(str::trim) else {
            return Err((401, "send credentials as `Authorization: Bearer <credential>`".into()));
        };
        if credential.starts_with(KEY_PREFIX) {
            return self.api_key(credential).ok_or((401, "unknown or revoked API key".into()));
        }
        match &self.oidc {
            Some(o) => o.verify(credential).map_err(|e| (401, e)),
            None => Err((401, format!("unknown credential (this server accepts API keys; set {ISSUER_ENV} for sign-in tokens)"))),
        }
    }

    fn api_key(&self, key: &str) -> Option<Verified> {
        let mut file = self.keys.as_ref()?.lock().ok()?;
        if file.loaded.map_or(true, |t| t.elapsed() >= RELOAD_EVERY) {
            match read_keys(&file.path) {
                Ok(keys) => file.keys = keys,
                Err(e) => eprintln!("[tj] {e}; keeping the keys loaded before"),
            }
            file.loaded = Some(Instant::now());
        }
        let hash = sha256_hex(key);
        let k = file.keys.iter().find(|k| k.sha256 == hash)?;
        Some(Verified { caller: Caller::Remote { id: format!("key:{}", k.name), roles: k.roles.clone() }, expires: None })
    }
}

/// `name:role,role` for `tj dev --as`.
fn parse_as(spec: &str) -> Result<Caller, String> {
    let (name, roles) = spec.split_once(':').unwrap_or((spec, ""));
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(format!("--as needs a name like ana or ana:staff, got {spec:?}"));
    }
    let roles = clean_roles(roles.split(',').filter(|r| !r.is_empty()).map(str::to_string).collect());
    Ok(Caller::Remote { id: format!("dev:{name}"), roles })
}

/// `tj key new <name> --role r --keys file`: makes a key, prints it once, stores its hash.
pub fn new_key(name: &str, roles: &[String], file: &Path) -> Result<String, String> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(format!("a key name is letters, digits, - and _, got {name:?}"));
    }
    let mut list: Vec<Json> = match std::fs::read_to_string(file) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{} is not a JSON array: {e}", file.display()))?,
        Err(_) => Vec::new(),
    };
    if list.iter().any(|k| k["name"] == name) {
        return Err(format!("{} already has a key named {name}; remove its line to revoke it first", file.display()));
    }
    let key = format!("{KEY_PREFIX}{}", hex(&random::<32>()?));
    list.push(json!({ "name": name, "roles": clean_roles(roles.to_vec()), "sha256": sha256_hex(&key) }));
    let text = serde_json::to_string_pretty(&list).expect("JSON serializes");
    std::fs::write(file, text + "\n").map_err(|e| format!("cannot write {}: {e}", file.display()))?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(v: Verified) -> (String, Vec<String>) {
        match v.caller {
            Caller::Remote { id, roles } => (id, roles),
            Caller::Trusted(_) => panic!("never trusted"),
        }
    }

    #[test]
    fn api_keys_anonymous_and_dev_identities() {
        let dir = std::env::temp_dir().join(format!("tj-keys-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let file = dir.join("keys.json");
        let key = new_key("sync", &["staff".into(), "system".into()], &file).expect("made");
        assert!(key.starts_with("tjk_") && key.len() == 68);
        assert!(!std::fs::read_to_string(&file).unwrap().contains(&key), "only the hash is stored");
        assert!(new_key("sync", &[], &file).is_err(), "names are unique");

        let id = Identity { oidc: None, keys: Some(Mutex::new(KeyFile { path: file.clone(), keys: read_keys(&file).unwrap(), loaded: Some(Instant::now()) })), dev_as: None };
        assert_eq!(remote(id.verify(Some(&format!("Bearer {key}"))).ok().unwrap()), ("key:sync".into(), vec!["staff".into()]), "system cannot be granted");
        assert_eq!(id.verify(Some("Bearer tjk_nope")).err().unwrap().0, 401);
        assert_eq!(id.verify(Some("Bearer eyJhbGciOi")).err().unwrap().0, 401, "a bad credential never becomes anonymous");
        assert_eq!(remote(id.verify(None).ok().unwrap()), (String::new(), vec![]));

        let dev = Identity { oidc: None, keys: None, dev_as: Some(parse_as("ana:staff,owner").unwrap()) };
        assert_eq!(remote(dev.verify(None).ok().unwrap()), ("dev:ana".into(), vec!["staff".into()]), "rule words are not roles");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A local test issuer: discovery and keys over HTTP, tokens signed with a fresh P-256 key.
    struct Issuer {
        url: String,
        pkcs8: Vec<u8>,
    }

    fn b64url(data: &[u8]) -> String {
        crate::mcp::base64(data).replace('+', "-").replace('/', "_").trim_end_matches('=').to_string()
    }

    fn issuer() -> Issuer {
        use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap().as_ref().to_vec();
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &pkcs8, &rng).unwrap();
        let point = pair.public_key().as_ref().to_vec(); // 0x04 || x || y
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}", server.server_addr().to_ip().unwrap().port());
        let jwks = json!({ "keys": [{ "kty": "EC", "crv": "P-256", "x": b64url(&point[1..33]), "y": b64url(&point[33..]), "kid": "k1", "alg": "ES256", "use": "sig" }] });
        let discovery = json!({ "issuer": url, "jwks_uri": format!("{url}/keys") });
        std::thread::spawn(move || {
            for req in server.incoming_requests() {
                let body = if req.url().ends_with("/keys") { jwks.to_string() } else { discovery.to_string() };
                let _ = req.respond(tiny_http::Response::from_string(body));
            }
        });
        Issuer { url, pkcs8 }
    }

    impl Issuer {
        fn token(&self, kid: &str, claims: Json) -> String {
            let mut header = jsonwebtoken::Header::new(Algorithm::ES256);
            header.kid = Some(kid.into());
            jsonwebtoken::encode(&header, &claims, &jsonwebtoken::EncodingKey::from_ec_der(&self.pkcs8)).unwrap()
        }
    }

    fn now() -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
    }

    #[test]
    fn sign_in_tokens_from_a_provider() {
        let iss = issuer();
        let oidc = Oidc::new(&iss.url, "shop-app", Some("realm_access.roles".into())).unwrap();
        let id = Identity { oidc: Some(oidc), keys: None, dev_as: None };
        let claims = |aud: Json, exp: u64| json!({ "iss": iss.url, "sub": "ana", "aud": aud, "exp": exp, "realm_access": { "roles": ["staff", "system"] } });
        let bearer = |t: String| format!("Bearer {t}");

        let ok = id.verify(Some(&bearer(iss.token("k1", claims(json!("shop-app"), now() + 600))))).expect("a valid token");
        assert!(ok.expires.is_some());
        assert_eq!(remote(ok), ("user:ana".into(), vec!["staff".into()]), "ids are prefixed; `system` cannot be granted");

        for (why, token) in [
            ("another audience", iss.token("k1", claims(json!("other-app"), now() + 600))),
            ("expired", iss.token("k1", claims(json!("shop-app"), now() - 3600))),
            ("unknown key", iss.token("k9", claims(json!("shop-app"), now() + 600))),
            ("several audiences without azp", iss.token("k1", claims(json!(["shop-app", "other-app"]), now() + 600))),
            ("not a token", "eyJ.nope.x".to_string()),
        ] {
            assert_eq!(id.verify(Some(&bearer(token))).err().map(|e| e.0), Some(401), "{why}");
        }
        // An unsigned token (`alg: none`) is refused before any key is looked at.
        let none = format!("{}.{}.", b64url(br#"{"alg":"none","typ":"JWT"}"#), b64url(claims(json!("shop-app"), now() + 600).to_string().as_bytes()));
        assert_eq!(id.verify(Some(&bearer(none))).err().map(|e| e.0), Some(401));
        assert!(Oidc::new("http://idp.example.com", "a", None).is_err(), "issuers must use https");
    }
}
