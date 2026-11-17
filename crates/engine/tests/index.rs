//! The persisted index: round trips, keys that differ by tree, and reopening.

use blueprint_engine::git::ObjectId;
use blueprint_engine::index::{Index, Indexed};
use blueprint_engine::l1::Diagnostic;
use blueprint_model::{Model, RepoPath};

type R<T> = Result<T, Box<dyn std::error::Error>>;

fn dir(name: &str) -> std::path::PathBuf {
    let d = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("index-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

#[test]
fn stores_and_reopens_the_model_of_a_tree() -> R<()> {
    let d = dir("roundtrip");
    let tree = ObjectId::from_hex(b"0123456789abcdef0123456789abcdef01234567")?;
    let other = ObjectId::from_hex(b"89abcdef0123456789abcdef0123456789abcdef")?;
    let diag = Diagnostic {
        path: RepoPath::parse("Cargo.toml")?,
        message: "malformed".into(),
    };
    let indexed = Indexed::new(Model::default(), std::slice::from_ref(&diag));

    let mut index = Index::open_at(&d)?;
    assert_eq!(index.get(tree), None);
    index.put(tree, &indexed)?;
    assert_eq!(index.get(tree).as_ref(), Some(&indexed));
    assert_eq!(index.get(other), None, "keys are per tree");

    let reopened = Index::open_at(&d)?;
    let got = reopened.get(tree).ok_or("not persisted")?;
    assert_eq!(got.diagnostics(), vec![diag]);
    assert!(reopened.dropped().is_empty());
    Ok(())
}

#[test]
fn many_puts_are_compacted() -> R<()> {
    let d = dir("compact");
    let mut index = Index::open_at(&d)?;
    for i in 0..40u8 {
        let mut hex = [b'0'; 40];
        hex[0] = b"0123456789abcdef"[usize::from(i % 16)];
        hex[1] = b"0123456789abcdef"[usize::from(i / 16)];
        index.put(
            ObjectId::from_hex(&hex)?,
            &Indexed::new(Model::default(), &[]),
        )?;
    }
    let segments = std::fs::read_dir(&d)?.count();
    assert!(segments <= 17, "{segments} segments");
    let reopened = Index::open_at(&d)?;
    let first = ObjectId::from_hex(&[b'0'; 40])?;
    assert!(
        reopened.get(first).is_some(),
        "compaction keeps every entry"
    );
    Ok(())
}
