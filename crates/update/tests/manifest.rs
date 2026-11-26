//! The signed update manifest (#179): what the release workflow writes and
//! the app accepts.

use blueprint_update::{
    Asset, Manifest, UpdateError, check_asset, keypair_from_seed, newer, sign, verify,
};

const SEED: [u8; 32] = [7; 32];

fn manifest() -> Manifest {
    let mut m = Manifest {
        version: "0.2.0".into(),
        notes: "Faster maps.".into(),
        assets: Default::default(),
    };
    m.assets.insert(
        "linux-x86_64-deb".into(),
        Asset {
            url: "https://github.com/backslashmatt/blueprint/releases/download/v0.2.0/blueprint_0.2.0_amd64.deb".into(),
            sha256: hex_sha256(b"the package"),
            size: 11,
        },
    );
    m
}

fn hex_sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

#[test]
fn a_signed_manifest_verifies_and_round_trips() {
    let (secret, public) = keypair_from_seed(&SEED);
    let bytes = manifest().to_json().expect("json");
    let sig = sign(&bytes, &secret);
    let got = verify(&bytes, &sig, &public).expect("verifies");
    assert_eq!(got, manifest());
}

#[test]
fn any_change_to_the_bytes_or_signature_is_refused() {
    let (secret, public) = keypair_from_seed(&SEED);
    let bytes = manifest().to_json().expect("json");
    let sig = sign(&bytes, &secret);
    // One byte of the manifest changed: a version, a URL or a hash.
    let mut tampered = bytes.clone();
    let i = tampered.iter().position(|b| *b == b'2').unwrap_or(0);
    tampered[i] = b'9';
    assert!(matches!(
        verify(&tampered, &sig, &public),
        Err(UpdateError::BadSignature)
    ));
    // Another key's signature.
    let (other, _) = keypair_from_seed(&[8; 32]);
    assert!(matches!(
        verify(&bytes, &sign(&bytes, &other), &public),
        Err(UpdateError::BadSignature)
    ));
    // Not a signature at all.
    assert!(verify(&bytes, "zz", &public).is_err());
    assert!(verify(&bytes, &"00".repeat(64), &public).is_err());
}

#[test]
fn a_verified_manifest_must_also_be_well_formed() {
    let (secret, public) = keypair_from_seed(&SEED);
    // Signed, but not a manifest.
    let junk = b"{\"version\": 3}".to_vec();
    assert!(matches!(
        verify(&junk, &sign(&junk, &secret), &public),
        Err(UpdateError::Format(_))
    ));
    // Signed, but an asset outside this repository's releases.
    let mut m = manifest();
    if let Some(a) = m.assets.get_mut("linux-x86_64-deb") {
        a.url = "https://example.com/blueprint.deb".into();
    }
    let bytes = m.to_json().expect("json");
    assert!(matches!(
        verify(&bytes, &sign(&bytes, &secret), &public),
        Err(UpdateError::Format(_))
    ));
    // Signed, but the version is not semver.
    let mut m = manifest();
    m.version = "two".into();
    let bytes = m.to_json().expect("json");
    assert!(matches!(
        verify(&bytes, &sign(&bytes, &secret), &public),
        Err(UpdateError::Format(_))
    ));
    // Too large to be a manifest: refused before parsing.
    let huge = vec![b' '; 2 << 20];
    assert!(matches!(
        verify(&huge, &sign(&huge, &secret), &public),
        Err(UpdateError::Format(_))
    ));
}

#[test]
fn only_stable_releases_under_this_repositorys_downloads_are_accepted() {
    let (secret, public) = keypair_from_seed(&SEED);
    let signed = |m: &Manifest| {
        let bytes = m.to_json().expect("json");
        let sig = sign(&bytes, &secret);
        verify(&bytes, &sig, &public)
    };
    // Found in review (#180): a prerelease would reach stable users, and
    // build metadata would make the same version look newer.
    for version in ["0.3.0-alpha.1", "0.2.0+build.7"] {
        let mut m = manifest();
        m.version = version.into();
        assert!(
            matches!(signed(&m), Err(UpdateError::Format(_))),
            "{version}"
        );
    }
    // URLs that only look like they are under the releases.
    for url in [
        "https://github.com/backslashmatt/blueprint/releases/download/../../evil/x",
        "https://github.com/backslashmatt/blueprint/releases/download/%2e%2e/%2e%2e/evil/x",
        "https://github.com/backslashmatt/blueprint/releases/download/@evil.com/x",
        "https://github.com/backslashmatt/blueprint/releases/download/v1/x?y=1",
        "https://github.com/backslashmatt/blueprint/releases/download/v1/x#y",
        "https://github.com/backslashmatt/blueprint/releases/download/v1\\x",
        "https://github.com/backslashmatt/blueprint/releases/download//x",
        "https://github.com/backslashmatt/blueprint/releases/download/",
    ] {
        let mut m = manifest();
        if let Some(a) = m.assets.get_mut("linux-x86_64-deb") {
            a.url = url.into();
        }
        assert!(matches!(signed(&m), Err(UpdateError::Format(_))), "{url}");
    }
    assert!(signed(&manifest()).is_ok());
}

#[test]
fn only_a_strictly_newer_version_is_an_update() {
    let m = manifest();
    assert_eq!(newer("0.1.0", &m), Ok(true));
    assert_eq!(newer("0.1.9", &m), Ok(true));
    assert_eq!(newer("0.2.0", &m), Ok(false), "the same version");
    assert_eq!(newer("0.3.0", &m), Ok(false), "never a downgrade");
    assert_eq!(
        newer("0.2.0-rc.1", &m),
        Ok(true),
        "a release after its candidate"
    );
    assert!(newer("not a version", &m).is_err());
}

#[test]
fn a_download_must_match_its_hash_and_size() {
    let a = &manifest().assets["linux-x86_64-deb"];
    assert_eq!(check_asset(b"the package", a), Ok(()));
    assert_eq!(
        check_asset(b"the packagE", a),
        Err(UpdateError::BadDownload)
    );
    assert_eq!(
        check_asset(b"the package!", a),
        Err(UpdateError::BadDownload)
    );
}

#[test]
fn the_public_key_is_written_and_read_as_hex() {
    let (_, public) = keypair_from_seed(&SEED);
    let text = blueprint_update::public_key_hex(&public);
    assert_eq!(text.len(), 64);
    assert_eq!(blueprint_update::parse_public_key(&text), Ok(public));
    assert!(blueprint_update::parse_public_key("abc").is_err());
}
