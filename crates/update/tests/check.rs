//! The update check and download (#179), against a fake GitHub.

use std::collections::HashMap;

use blueprint_update::{
    Asset, Check, Fetch, LATEST, Manifest, UpdateError, check, download, keypair_from_seed, sign,
};

/// URLs and what they serve; anything else is not found.
struct Fake(HashMap<String, Vec<u8>>);

impl Fetch for Fake {
    fn get(&self, url: &str, limit: u64) -> Result<Option<Vec<u8>>, String> {
        match self.0.get(url) {
            Some(b) if b.len() as u64 > limit => Err(format!("{url}: over {limit} bytes")),
            Some(b) => Ok(Some(b.clone())),
            None => Ok(None),
        }
    }
}

const DEB: &str =
    "https://github.com/backslashmatt/blueprint/releases/download/v0.2.0/blueprint_0.2.0_amd64.deb";

fn release(seed: u8, installer: &[u8]) -> Fake {
    use sha2::{Digest, Sha256};
    let mut m = Manifest {
        version: "0.2.0".into(),
        notes: "New.".into(),
        assets: Default::default(),
    };
    m.assets.insert(
        "linux-x86_64-deb".into(),
        Asset {
            url: DEB.into(),
            sha256: hex::encode(Sha256::digest(b"the installer")),
            size: 13,
        },
    );
    // A failure leaves the fake empty, which every test notices.
    let Ok(bytes) = m.to_json() else {
        return Fake(HashMap::new());
    };
    let (secret, _) = keypair_from_seed(&[seed; 32]);
    let sig = sign(&bytes, &secret);
    Fake(HashMap::from([
        (LATEST.to_owned(), bytes),
        (format!("{LATEST}.sig"), sig.into_bytes()),
        (DEB.to_owned(), installer.to_vec()),
    ]))
}

#[test]
fn a_newer_signed_release_for_this_platform_is_offered() {
    let (_, public) = keypair_from_seed(&[1; 32]);
    let gh = release(1, b"the installer");
    let Ok(Check::Available { manifest, asset }) = check(&gh, "0.1.0", "linux-x86_64-deb", &public)
    else {
        panic!("no update offered");
    };
    assert_eq!(manifest.version, "0.2.0");
    assert_eq!(download(&gh, &asset), Ok(b"the installer".to_vec()));
}

#[test]
fn nothing_is_offered_when_up_to_date_unreleased_or_not_for_this_platform() {
    let (_, public) = keypair_from_seed(&[1; 32]);
    let gh = release(1, b"the installer");
    assert_eq!(
        check(&gh, "0.2.0", "linux-x86_64-deb", &public),
        Ok(Check::UpToDate)
    );
    assert_eq!(
        check(&gh, "0.1.0", "windows-x86_64", &public),
        Ok(Check::UpToDate)
    );
    // No release published yet: GitHub answers 404.
    assert_eq!(
        check(&Fake(HashMap::new()), "0.1.0", "linux-x86_64-deb", &public),
        Ok(Check::NoRelease)
    );
}

#[test]
fn a_release_signed_by_another_key_or_a_tampered_download_is_refused() {
    let (_, public) = keypair_from_seed(&[1; 32]);
    let forged = release(2, b"the installer");
    assert_eq!(
        check(&forged, "0.1.0", "linux-x86_64-deb", &public),
        Err(UpdateError::BadSignature)
    );
    // A manifest without its signature is refused too.
    let mut unsigned = release(1, b"the installer");
    unsigned.0.remove(&format!("{LATEST}.sig"));
    assert_eq!(
        check(&unsigned, "0.1.0", "linux-x86_64-deb", &public),
        Err(UpdateError::BadSignature)
    );
    // The right release, but the host serves other bytes.
    let swapped = release(1, b"evil bytes!!!");
    let Ok(Check::Available { asset, .. }) = check(&swapped, "0.1.0", "linux-x86_64-deb", &public)
    else {
        panic!("no update offered");
    };
    assert_eq!(download(&swapped, &asset), Err(UpdateError::BadDownload));
}

/// Serves the first manifest request from one release and every later
/// request from another: a release published between the two requests.
struct Racing {
    before: Fake,
    after: Fake,
    calls: std::cell::Cell<u32>,
}

impl Fetch for Racing {
    fn get(&self, url: &str, limit: u64) -> Result<Option<Vec<u8>>, String> {
        let n = self.calls.get();
        self.calls.set(n + 1);
        if n == 0 { &self.before } else { &self.after }.get(url, limit)
    }
}

#[test]
fn a_release_published_between_the_two_requests_is_read_again() {
    let (_, public) = keypair_from_seed(&[1; 32]);
    // The first request gets the previous release's manifest (other bytes);
    // the signature request already gets the new release's. The pair cannot
    // verify, so the check reads both again.
    let mut before = release(1, b"the installer");
    let mut old = Manifest {
        version: "0.1.5".into(),
        notes: "Older.".into(),
        assets: Default::default(),
    };
    old.assets.clear();
    if let Ok(bytes) = old.to_json() {
        before.0.insert(LATEST.to_owned(), bytes);
    }
    let racing = Racing {
        before,
        after: release(1, b"the installer"),
        calls: std::cell::Cell::new(0),
    };
    assert!(matches!(
        check(&racing, "0.1.0", "linux-x86_64-deb", &public),
        Ok(Check::Available { .. })
    ));
}
