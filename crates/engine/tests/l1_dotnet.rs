//! L1 .NET analysis over fixtures, as text snapshots in `tests/snapshots/`.
//! Set `UPDATE_SNAPSHOTS=1` to rewrite them.

#![allow(clippy::unwrap_used, clippy::panic)] // test code

mod common;

use blueprint_engine::l1::{Diagnostic, dotnet};
use blueprint_model::{Confidence, Model, RepoPath};

use common::{Files, files, render, snapshot};

fn run(files: &Files) -> (Model, Vec<Diagnostic>) {
    let paths: Vec<RepoPath> = files.keys().cloned().collect();
    let mut model = Model::default();
    let diags = dotnet::analyse(&paths, &|p| files.get(p).cloned(), &mut model);
    (model, diags)
}

fn edges(model: &Model) -> Vec<String> {
    model
        .edges()
        .map(|e| {
            format!(
                "{} -> {} {:?}",
                e.key.from.system_key().dir,
                e.key.to.system_key().dir,
                e.confidence
            )
        })
        .collect()
}

#[test]
fn project_references() {
    let (model, diags) = run(&files(&[
        (
            "src/App/App.csproj",
            "\u{feff}<Project\n    Sdk=\"Microsoft.NET.Sdk\">\n  <ItemGroup>\n    <ProjectReference Include=\"..\\Core\\Core.csproj\" />\n    <ProjectReference Include='../Data/Data.fsproj;..\\Ui\\Ui.vbproj' Condition=\"'$(X)' == 'y'\" />\n    <!-- <ProjectReference Include=\"..\\Old\\Old.csproj\" /> -->\n    <projectreference include=\"$(RepoRoot)/src/Shared/Shared.csproj\" />\n    <PackageReference Include=\"Newtonsoft.Json\" Version=\"13.0.1\" />\n  </ItemGroup>\n</Project>\n",
        ),
        (
            "src/Core/Core.csproj",
            "<Project Sdk=\"Microsoft.NET.Sdk\" />",
        ),
        (
            "src/Data/Data.fsproj",
            "<Project Sdk=\"Microsoft.NET.Sdk\"></Project>",
        ),
        ("src/Ui/Ui.vbproj", "<Project>\n</Project>"),
        ("src/Shared/Shared.csproj", "<Project/>"),
        ("src/Old/Old.csproj", "<Project/>"),
        ("src/App/obj/App.csproj.nuget.g.csproj", "<Project/>"),
    ]));
    snapshot("l1_dotnet_references", &render(&model, &diags));
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(
        edges(&model),
        vec![
            "src/App -> src/Core Exact",
            "src/App -> src/Data Exact",
            "src/App -> src/Shared Inferred",
            "src/App -> src/Ui Exact",
        ],
        "literal paths are exact, $(..) paths are matched by file name, comments and obj/ are skipped"
    );
}

#[test]
fn malformed_and_dangling() {
    let (model, diags) = run(&files(&[
        (
            "a/A.csproj",
            "<Project><ItemGroup><ProjectReference Include=\"../missing/M.csproj\" /><ProjectReference Include=\"../../../x.csproj\"/></ItemGroup></Project>",
        ),
        ("b/B.csproj", "not xml at all"),
        ("c/C.csproj", "<Project>"),
        ("c/C2.csproj", "<Project>"),
    ]));
    snapshot("l1_dotnet_malformed", &render(&model, &diags));
    let unknown: Vec<String> = model
        .nodes()
        .filter(|n| n.confidence == Confidence::Unknown)
        .map(|n| n.name.clone())
        .collect();
    assert_eq!(unknown, vec!["B"]);
    let messages: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
    assert!(
        messages.iter().any(|m| m.contains("names no project file")),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m.contains("outside the repository")),
        "{messages:?}"
    );
    assert!(
        messages.iter().any(|m| m.contains("comes first")),
        "{messages:?}"
    );
}

#[test]
fn mutated_project_files_never_panic() {
    let seed = files(&[
        (
            "a/A.csproj",
            "<Project Sdk=\"x\"><ItemGroup><ProjectReference Include=\"..\\b\\B.csproj;$(R)/c/C.csproj\" /></ItemGroup></Project>",
        ),
        ("b/B.csproj", "<Project><!-- c --></Project>"),
        ("c/C.csproj", "<Project/>"),
    ]);
    let mut state = 0x0F0F_1234_ABCD_9876u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let tokens: [&[u8]; 9] = [
        b"<",
        b">",
        b"\"",
        b"'",
        b"<!--",
        b"-->",
        b"Include=",
        b"\\..\\..",
        "é".as_bytes(),
    ];
    for _ in 0..3_000 {
        let mut f = seed.clone();
        for v in f.values_mut() {
            for _ in 0..(next() % 4) {
                let i = (next() as usize) % (v.len() + 1);
                match next() % 3 {
                    0 => v.truncate(i),
                    1 => {
                        let t = tokens[(next() % tokens.len() as u64) as usize];
                        v.splice(i..i, t.iter().copied());
                    }
                    _ => {
                        if i < v.len() {
                            v[i] = next() as u8;
                        }
                    }
                }
            }
        }
        let (model, _) = run(&f);
        let back: Model = serde_json::from_str(&serde_json::to_string(&model).unwrap()).unwrap();
        assert_eq!(back.nodes().count(), model.nodes().count());
    }
}

#[test]
fn review_findings_attributes_entities_case_remove_and_order() {
    let a = concat!(
        "<?xml version=\"1.0\"?>\n<Project Sdk=\"x\">\n<ItemGroup>\n",
        // `>` inside a quoted value does not end the tag.
        "<ProjectReference Condition=\"'$(V)' > '1'\" Include=\"..\\b\\B.csproj\"/>\n",
        // An attribute name inside another attribute's value is not an attribute.
        "<ProjectReference Label=\"x Include='..\\c\\C.csproj'\" Include=\"..\\d\\D.csproj\"/>\n",
        // Entities are decoded before splitting on `;`.
        "<ProjectReference Include=\"..&#x5C;e&#x5C;E.csproj;..\\f&amp;g\\F.csproj\"/>\n",
        // Case differs from the file on disk.
        "<ProjectReference Include=\"..\\H\\h.CSPROJ\"/>\n",
        // Included, then removed.
        "<ProjectReference Include=\"..\\c\\C.csproj\"/>\n<ProjectReference Remove=\"../c/C.csproj\"/>\n",
        // Wildcards are reported, not expanded; look-alikes are not references.
        "<ProjectReference Include=\"..\\*\\*.csproj\"/>\n<ProjectReferenceX Include=\"..\\c\\C.csproj\"/>\n",
        "<![CDATA[ <ProjectReference Include=\"..\\c\\C.csproj\"/> ]]>\n",
        // A second project file in a shared directory resolves to its node.
        "<ProjectReference Include=\"..\\s\\S2.csproj\"/>\n",
        "</ItemGroup>\n</Project>\n",
    );
    let entries = [
        ("a/A.csproj", a),
        ("b/B.csproj", "<Project/>"),
        ("c/C.csproj", "<Project/>"),
        ("d/D.csproj", "<Project/>"),
        ("e/E.csproj", "<Project/>"),
        ("f&g/F.csproj", "<Project/>"),
        ("h/H.csproj", "<Project/>"),
        ("s/S2.csproj", "<Project/>"),
        ("s/S1.csproj", "<Project/>"),
        ("t/T.csproj", "<project-like text, not a Project element/>"),
    ];
    let (model, diags) = run(&files(&entries));
    snapshot("l1_dotnet_review", &render(&model, &diags));
    assert_eq!(
        edges(&model),
        vec![
            "a -> b Exact",
            "a -> d Exact",
            "a -> e Exact",
            "a -> f&g Exact",
            "a -> h Exact",
            "a -> s Exact"
        ]
    );
    let messages: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
    assert!(
        messages.iter().any(|m| m.contains("wildcard reference")),
        "{messages:?}"
    );
    let s = model
        .nodes()
        .find(|n| n.id.system_key().dir.to_string() == "s")
        .unwrap();
    assert_eq!(
        s.name, "S1",
        "the first file by path wins, whatever the input order"
    );
    let t = model
        .nodes()
        .find(|n| n.id.system_key().dir.to_string() == "t")
        .unwrap();
    assert_eq!(t.confidence, Confidence::Unknown);
}
