//! Signed update manifests (#179, DR 0018).
//!
//! Each release on GitHub Releases carries `manifest.json` and its signature,
//! `manifest.json.sig`: the hex of an ed25519 signature over the manifest's
//! exact bytes. The private key is the repository secret
//! `UPDATE_SIGNING_KEY`, separate from any platform signing. The app holds
//! the public key, so a compromised download host cannot push an update.
//!
//! This crate is headless and does no I/O: the release workflow signs with
//! it, and the app verifies with it before it trusts anything a manifest
//! says.

use std::collections::BTreeMap;
use std::fmt;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub use ed25519_dalek::{SigningKey as SecretKey, VerifyingKey as PublicKey};

/// Where every asset of a release must be: this repository's releases.
pub const RELEASES: &str = "https://github.com/backslashmatt/blueprint/releases/download/";

/// The largest manifest read, in bytes. A real one is a few hundred.
pub const MAX_MANIFEST: usize = 64 * 1024;

/// One release, as its manifest describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// The release's version, semver (`0.2.0`).
    pub version: String,
    /// What changed, for the user, in plain text.
    pub notes: String,
    /// The installer for each platform, keyed by platform
    /// (`linux-x86_64-deb`, `linux-x86_64-rpm`, `windows-x86_64`,
    /// `macos-aarch64`).
    pub assets: BTreeMap<String, Asset>,
}

/// One downloadable installer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Asset {
    /// Where to download it: under [`RELEASES`].
    pub url: String,
    /// The SHA-256 of its bytes, lower-case hex.
    pub sha256: String,
    /// Its size in bytes.
    pub size: u64,
}

impl Manifest {
    /// The manifest as the release workflow writes it: pretty JSON with a
    /// final newline. These exact bytes are what is signed.
    ///
    /// # Errors
    /// When it cannot be serialised, so that nothing empty is ever signed.
    pub fn to_json(&self) -> Result<Vec<u8>, UpdateError> {
        let mut out = serde_json::to_vec_pretty(self)
            .map_err(|e| UpdateError::Invalid(format!("the manifest cannot be written: {e}")))?;
        out.push(b'\n');
        Ok(out)
    }

    /// Whether every field is something the app may act on: a stable
    /// semver version (no prerelease, so an unstable build never reaches
    /// stable users; no build metadata, which semver would order after the
    /// same version), and assets only under [`RELEASES`] with a 64-digit
    /// hash.
    fn check(&self) -> Result<(), UpdateError> {
        let v = semver::Version::parse(&self.version)
            .map_err(|e| UpdateError::Format(format!("version {:?}: {e}", self.version)))?;
        if !v.pre.is_empty() || !v.build.is_empty() {
            return Err(UpdateError::Format(format!(
                "version {}: only stable releases are offered",
                self.version
            )));
        }
        for (platform, a) in &self.assets {
            if !release_url(&a.url) {
                return Err(UpdateError::Format(format!(
                    "{platform}: {} is not under {RELEASES}",
                    a.url
                )));
            }
            if a.sha256.len() != 64 || !a.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(UpdateError::Format(format!("{platform}: bad sha256")));
            }
        }
        Ok(())
    }
}

/// Whether `url` is a plain path under [`RELEASES`]: segments of letters,
/// digits, `.`, `_`, `-` and `+` only, none empty or `.`/`..`. So no
/// escapes, queries, fragments, userinfo or backslashes, whatever a URL
/// parser would make of them.
fn release_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix(RELEASES) else {
        return false;
    };
    !rest.is_empty()
        && rest.split('/').all(|seg| {
            !seg.is_empty()
                && seg != "."
                && seg != ".."
                && seg
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+'))
        })
}

/// Why an update was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    /// The signature does not match the manifest and the key.
    BadSignature,
    /// The manifest is signed but not one the app may act on.
    Format(String),
    /// A downloaded installer does not match its hash or size.
    BadDownload,
    /// A key or version that cannot be read.
    Invalid(String),
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UpdateError::BadSignature => {
                f.write_str("the update manifest's signature is not valid")
            }
            UpdateError::Format(why) => write!(f, "the update manifest is not usable: {why}"),
            UpdateError::BadDownload => {
                f.write_str("the downloaded installer does not match its manifest")
            }
            UpdateError::Invalid(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for UpdateError {}

/// The key pair from a 32-byte seed. The release workflow keeps the seed
/// as the secret `UPDATE_SIGNING_KEY`, in hex.
#[must_use]
pub fn keypair_from_seed(seed: &[u8; 32]) -> (SigningKey, VerifyingKey) {
    let secret = SigningKey::from_bytes(seed);
    let public = secret.verifying_key();
    (secret, public)
}

/// The secret key from its hex seed, as stored in the repository secret.
///
/// # Errors
/// When it is not 64 hex digits.
pub fn parse_secret_key(hex_seed: &str) -> Result<SigningKey, UpdateError> {
    let seed: [u8; 32] = hex::decode(hex_seed.trim())
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| UpdateError::Invalid("the signing key is not 64 hex digits".into()))?;
    Ok(SigningKey::from_bytes(&seed))
}

/// The public key as hex, as compiled into the app.
#[must_use]
pub fn public_key_hex(key: &VerifyingKey) -> String {
    hex::encode(key.as_bytes())
}

/// The public key from its hex.
///
/// # Errors
/// When it is not 64 hex digits of a valid ed25519 point.
pub fn parse_public_key(text: &str) -> Result<VerifyingKey, UpdateError> {
    let bytes: [u8; 32] = hex::decode(text.trim())
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| UpdateError::Invalid("the public key is not 64 hex digits".into()))?;
    VerifyingKey::from_bytes(&bytes)
        .map_err(|_| UpdateError::Invalid("the public key is not an ed25519 key".into()))
}

/// The signature over `manifest`, as the hex that goes in
/// `manifest.json.sig`.
#[must_use]
pub fn sign(manifest: &[u8], secret: &SigningKey) -> String {
    hex::encode(secret.sign(manifest).to_bytes())
}

/// The manifest, once its signature is checked against `public` and its
/// fields are checked. Nothing is parsed before the signature is.
///
/// # Errors
/// [`UpdateError::BadSignature`] for any signature that does not match, and
/// [`UpdateError::Format`] for a signed manifest the app must not act on.
pub fn verify(
    manifest: &[u8],
    signature_hex: &str,
    public: &VerifyingKey,
) -> Result<Manifest, UpdateError> {
    if manifest.len() > MAX_MANIFEST {
        return Err(UpdateError::Format(format!(
            "{} bytes, more than {MAX_MANIFEST}",
            manifest.len()
        )));
    }
    let bytes: [u8; 64] = hex::decode(signature_hex.trim())
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or(UpdateError::BadSignature)?;
    // Strict: no malleable or small-order signatures.
    public
        .verify_strict(manifest, &Signature::from_bytes(&bytes))
        .map_err(|_| UpdateError::BadSignature)?;
    let parsed: Manifest =
        serde_json::from_slice(manifest).map_err(|e| UpdateError::Format(e.to_string()))?;
    parsed.check()?;
    Ok(parsed)
}

/// Whether `manifest` is a newer release than `current`, the running
/// app's version. Only strictly newer counts, so a replayed old manifest
/// can never downgrade the app.
///
/// # Errors
/// When `current` is not semver; the manifest's version was checked by
/// [`verify`].
pub fn newer(current: &str, manifest: &Manifest) -> Result<bool, UpdateError> {
    let cur = semver::Version::parse(current)
        .map_err(|e| UpdateError::Invalid(format!("version {current:?}: {e}")))?;
    let new = semver::Version::parse(&manifest.version)
        .map_err(|e| UpdateError::Format(format!("version {:?}: {e}", manifest.version)))?;
    Ok(new > cur)
}

/// Whether a downloaded installer is the one the manifest names.
///
/// # Errors
/// [`UpdateError::BadDownload`] when its size or SHA-256 differ.
pub fn check_asset(bytes: &[u8], asset: &Asset) -> Result<(), UpdateError> {
    let ok = bytes.len() as u64 == asset.size
        && hex::encode(Sha256::digest(bytes)).eq_ignore_ascii_case(&asset.sha256);
    if ok {
        Ok(())
    } else {
        Err(UpdateError::BadDownload)
    }
}
