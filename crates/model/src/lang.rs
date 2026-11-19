//! The language plug-in boundary (REQUIREMENTS §3.4, DR 0008).
//!
//! A [`LanguageAnalyser`] sees one file at a time and returns its
//! [`FileFacts`]: the L3 and L4 items the file defines, and the references it
//! makes, still unresolved. The engine stores facts per blob, so an unchanged
//! file is never parsed twice. A separate pass in the engine turns the
//! references into edges across files. Analysers do no I/O.

use serde::{Deserialize, Serialize};

use crate::{RepoPath, Visibility};

/// One language's analyser.
pub trait LanguageAnalyser: Sync {
    /// A short, stable name, such as `rust`. It is part of the cache key.
    fn name(&self) -> &'static str;

    /// The version of this analyser's output. Bump it whenever the facts for
    /// the same bytes would change, so that stored facts are not reused.
    fn version(&self) -> u32;

    /// Whether this analyser reads the file at `path`.
    fn handles(&self, path: &RepoPath) -> bool;

    /// The facts of one file. Must never panic, whatever the bytes: anything
    /// that cannot be understood makes [`FileFacts::clean`] false and is
    /// otherwise skipped (§5.5).
    ///
    /// The facts must depend on the bytes alone, not on `path`: the engine
    /// stores them by blob id, so two files with the same content share them.
    fn analyse(&self, path: &RepoPath, bytes: &[u8]) -> FileFacts;

    /// The tokens of one file to colour in the code view (§5.4, L4), sorted
    /// by start and not overlapping. Must never panic, whatever the bytes;
    /// the default colours nothing.
    fn highlight(&self, bytes: &[u8]) -> Vec<Highlight> {
        let _ = bytes;
        Vec::new()
    }
}

/// What kind of token a [`Highlight`] is, for colouring code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TokenClass {
    /// A keyword, such as `fn` or `pub`.
    Keyword,
    /// A type name, including primitive types.
    Type,
    /// The name of a function or macro where it is defined or called.
    Function,
    /// A string or character literal.
    String,
    /// A number or boolean literal.
    Number,
    /// A comment.
    Comment,
}

/// One coloured token: a byte range of the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Highlight {
    /// First byte.
    pub start: u32,
    /// One past the last byte.
    pub end: u32,
    /// What it is.
    pub class: TokenClass,
}

/// What one file defines and refers to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileFacts {
    /// Every item, in source order (by where it starts, attributes included).
    pub items: Vec<Item>,
    /// Every reference, unresolved, in source order.
    pub references: Vec<Reference>,
    /// False if the parser had to recover from errors. Items found are kept,
    /// but the engine marks them `inferred`.
    pub clean: bool,
}

/// One L3 or L4 item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    /// What it is.
    pub kind: ItemKind,
    /// Its name as written, such as `Manifest` or `parse`.
    pub name: String,
    /// The inline modules it sits in within the file (`mod a { mod b { … } }`
    /// gives `["a", "b"]`); empty at the top of the file.
    pub module: Vec<String>,
    /// First and last line, 1-based and inclusive.
    pub lines: (u32, u32),
    /// The line its body starts on, if it has one. Lines before it are its
    /// signature (decides the split of change weight).
    pub body_line: Option<u32>,
    /// As declared. `None` where the language has no such idea.
    pub visibility: Option<Visibility>,
    /// Hash of the tokens outside the body: visibility, generics,
    /// parameters, return type, fields, variants. Whitespace and comments do
    /// not change it.
    pub signature_hash: u64,
    /// Hash of the body's tokens, likewise. `None` for an item with no body
    /// (a struct, a required trait method), so that the diff never pairs two
    /// bodiless items as a move or rename.
    pub body_hash: Option<u64>,
    /// The paths a function's body calls, in source order, without
    /// repeats: `foo()`, `a::b::foo()`, `Type::new()`. Method calls
    /// (`x.foo()`) are not included: they need types to resolve.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<Vec<String>>,
}

/// The kind of an [`Item`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum ItemKind {
    /// A type: struct, enum, union, trait or type alias (L3).
    Type {
        /// `struct`, `enum`, `union`, `trait` or `type`.
        keyword: String,
    },
    /// A free function (L4, under its module's group of functions).
    Function,
    /// A function in an `impl` or `trait` block (L4, under its type).
    Method {
        /// The type the block is for, as written, without generics.
        self_type: String,
        /// The trait, for `impl Trait for Type`; also set for a trait's own
        /// provided and required methods.
        r#trait: Option<String>,
    },
}

/// A reference that the engine resolves into an edge (#62).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum Reference {
    /// A `use` path, with its segments.
    Use {
        /// The path, such as `["crate", "l1", "Diagnostic"]`.
        path: Vec<String>,
        /// The name it is bound to when renamed (`use a::B as C` gives `C`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        alias: Option<String>,
        /// The inline module the `use` sits in.
        module: Vec<String>,
    },
    /// `impl Trait for Type`: the type implements the trait (§3.2).
    Impl {
        /// The type's path as written, without generic arguments or `&`.
        self_type: Vec<String>,
        /// The trait's path as written, without generic arguments.
        r#trait: Vec<String>,
        /// The inline module the `impl` sits in.
        module: Vec<String>,
    },
}
