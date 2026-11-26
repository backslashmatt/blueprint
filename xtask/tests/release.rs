//! The release manifest (#179): written from the installers CI built, signed,
//! and accepted by the app's verifier.

use std::path::PathBuf;

use xtask::release::{platform_of, write_manifest};

/// A fresh directory for one test, which the test creates.
fn dir(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("xtask-release-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

#[test]
fn installers_are_named_by_platform() {
    assert_eq!(
        platform_of("blueprint_0.2.0_amd64.deb"),
        Some("linux-x86_64-deb")
    );
    assert_eq!(
        platform_of("blueprint-0.2.0-1.x86_64.rpm"),
        Some("linux-x86_64-rpm")
    );
    assert_eq!(
        platform_of("blueprint_0.2.0_x64-setup.exe"),
        Some("windows-x86_64")
    );
    assert_eq!(
        platform_of("Blueprint_0.2.0_aarch64.dmg"),
        Some("macos-aarch64")
    );
    assert_eq!(platform_of("THIRD-PARTY-NOTICES.txt"), None);
    assert_eq!(platform_of("notes.deb.txt"), None);
}

#[test]
fn a_written_and_signed_manifest_verifies_and_names_every_installer() {
    let d = dir("ok");
    std::fs::create_dir_all(&d).expect("temp dir");
    let files = [
        ("blueprint_0.2.0_amd64.deb", &b"deb bytes"[..]),
        ("blueprint-0.2.0-1.x86_64.rpm", b"rpm bytes"),
        ("blueprint_0.2.0_x64-setup.exe", b"exe bytes"),
        ("Blueprint_0.2.0_aarch64.dmg", b"dmg bytes"),
    ];
    let mut paths = Vec::new();
    for (name, bytes) in files {
        let p = d.join(name);
        std::fs::write(&p, bytes).expect("write");
        paths.push(p);
    }
    let m = write_manifest("0.2.0", "Notes.", &paths).expect("manifest");
    assert_eq!(m.assets.len(), 4);
    let deb = &m.assets["linux-x86_64-deb"];
    assert_eq!(
        deb.url,
        "https://github.com/backslashmatt/blueprint/releases/download/v0.2.0/blueprint_0.2.0_amd64.deb"
    );
    assert_eq!(blueprint_update::check_asset(b"deb bytes", deb), Ok(()));
    // Signed with a key, it passes the app's own checks.
    let (secret, public) = blueprint_update::keypair_from_seed(&[3; 32]);
    let bytes = m.to_json().expect("json");
    let sig = blueprint_update::sign(&bytes, &secret);
    assert_eq!(blueprint_update::verify(&bytes, &sig, &public), Ok(m));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_release_must_be_complete_and_unambiguous() {
    let d = dir("bad");
    std::fs::create_dir_all(&d).expect("temp dir");
    let one = d.join("blueprint_0.2.0_amd64.deb");
    std::fs::write(&one, b"x").expect("write");
    // Two installers for one platform.
    let two = d.join("blueprint_0.2.1_amd64.deb");
    std::fs::write(&two, b"y").expect("write");
    assert!(write_manifest("0.2.0", "", &[one.clone(), two]).is_err());
    // A file that is no installer.
    let txt = d.join("notes.txt");
    std::fs::write(&txt, b"z").expect("write");
    assert!(write_manifest("0.2.0", "", &[one.clone(), txt]).is_err());
    // A version the app would refuse.
    assert!(write_manifest("0.2.0-rc.1", "", std::slice::from_ref(&one)).is_err());
    // An installer whose name is not of this version.
    assert!(write_manifest("0.3.0", "", &[one]).is_err());
    let _ = std::fs::remove_dir_all(&d);
}
