# 0011: Review state: keyed by the node's change, stored with the user's data

**Status:** accepted (agent decision, 2026-11-19). For #115 (REQUIREMENTS §5.4 view 5).

## Context

A reviewer marks nodes reviewed, at any level, with an optional note. §5.4 says the state:
- rolls up: a module is reviewed when its changed children are;
- persists locally;
- survives a rebase where the node's content is unchanged.

Three questions follow:
1. What does a mark attach to, so that a rebase keeps it?
2. Where is it stored?
3. Exactly when does a parent count as reviewed?

## Options

**Key.**
- (a) Commit ids and node id. A rebase changes the commits, so every mark is lost.
- (b) Node id and the node's content at head. This survives a rebase. It also carries a mark into any other review of the same head content, even one against a different base, where the node's change is a different change.
- (c) Node id and the node's content at **both** base and head.

**Store.**
- (a) The index store (DR 0007). The index is a cache that can always be deleted and rebuilt (DR 0005, DR 0007), and marks cannot be rebuilt.
- (b) A small file per repository in the platform *data* directory.

## Decision

**Key: option (c), plus the changed text.** A mark is stored as (node id, key, reviewed, note). The key is a hash of:
- the node's id;
- `KEY_VERSION`;
- the node's fingerprint in base and in head (zero where it is absent);
- the digest of its changed lines: a hash of the text of every line the diff counts against the node or below it, on each side, without line numbers. Each run of changed lines is hashed together with the unchanged line just above it, so the same text changed somewhere else in the file counts as a different change. The engine computes it with the change weight (`Weight::digest`), so keys exist only for a full diff.

A fingerprint is a hash of the node's kind, name, source file, visibility, confidence and code hashes, plus its children's fingerprints. It leaves out line numbers.

Fingerprints alone are not enough. A node with no code hashes still has lines of its own, for example a file no analyser reads (`Cargo.toml`) or a module's `use` lines. Its fingerprint is the same whatever the edit, so without the digest a mark on one change to `Cargo.toml` would carry to every later one.

With both, a mark applies wherever the node changed in exactly the way that was reviewed. That holds after a rebase that leaves the node's change alone, even if its lines moved, and in any review that contains the same change. Otherwise the mark stops matching and the node shows as unreviewed: a mark is never applied to a change nobody saw.

A moved or renamed node is reviewed under its head id, and so is every ancestor reached through its base id. Its base id is never a separate changed node: its removed lines count with the head id's, for the key and for its old parent's roll-up.

**Store: option (b).** `<data>/reviews/<hash of the canonical repository path>.json`, where `<data>` is `$BLUEPRINT_DATA_DIR` or the platform data directory (`~/.local/share/blueprint` on Linux).
- **Writes:** the whole file is rewritten on each change, through a temporary file and a rename, so a crash leaves the old file or the new one.
- **Damage:** a file that does not parse is moved aside to `.json.damaged` and reported. A later one goes to `.json.damaged.1`, never over an earlier one. The session carries on with no marks (§5.5).
- **Never overwritten:** if the file cannot be read, cannot be moved aside, or was written by a newer version, it is left alone. A newer file is recognised by its `format` field alone, whatever shape its marks have. Marks are not shown and new marks are not saved, and the reviewer is told why.
- **Clearing a mark:** a mark with neither reviewed nor a note is removed.

**Roll-up.** A changed node is reviewed when it is marked reviewed, or when:
- every changed child is reviewed, and
- nothing in the node itself changed: no lines of its own (its weight less its changed children's) and no move, rename or surface change of its own.

A changed node with no changed children needs its own mark. This keeps roll-up honest. For example, a type whose signature changed is not reviewed just because its methods are. Progress is counted over the nodes that need their own mark, so it reaches 100% only when every changed node is reviewed.

The logic is in `blueprint_model::review`, so it has no I/O and any front end, the CLI included, computes the same state. Storage is in `blueprint_engine::reviews`.

## Measured

Nothing on a hot path yet. Fingerprinting is one pass over each model, run when a diff arrives. That cost is measured when the UI uses it (#115, part 2).

## Revisit when

- The file grows large. Marks for old changes are never pruned. At about 100 bytes a mark, 10,000 marks is about 1 MB rewritten per change. Pruning marks whose nodes no longer exist in any recent commit would bound it.
- Two writers on one repository: two app windows, or the app and `bp review` at once. Each write reloads the file first, so a mark is lost only when two writes race, and then the last rename wins. A lock file would close the gap.
- Reviewers want a mark to carry over a change to its node that is only cosmetic. Fingerprints already ignore whitespace and comments through the code hashes (DR 0008), so this would have to be something more.
