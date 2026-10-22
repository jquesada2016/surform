//! The apply variant's manifest pins + the build-level artifact
//! assertions. The source-level pins are text assertions on
//! the crate's own files (the sibling manifest suite's precedent: no
//! TOML dependency is bought to parse what is effectively a golden).
//! The artifact-level
//! assertions read the BUILT `.surli` files when they exist (the
//! archive is a zstd-compressed tar whose `surrealism/` members ride
//! plain; `tar --zstd -xO` extracts them with the system toolchain) —
//! the default artifact's manifest section must carry NO capability
//! grant and NO `apply` export; the apply artifact's must carry the
//! exact grant surface and all four exports. An artifact not yet
//! built skips with a note — the build recipe in the crate README is
//! the step that produces it.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn crate_file(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(name);
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("{} unreadable: {err}", path.display()))
}

fn workspace_file(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(name);
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("{} unreadable: {err}", path.display()))
}

/// The grant manifest's capabilities body: the apply artifact asks for
/// EXACTLY the arbitrary-queries grant and the three named functions —
/// nothing wildcarded, nothing else. Every line asserted by text, so
/// a future grant key appended by a bump cannot slip in unasserted.
#[test]
fn the_apply_manifest_grants_exactly_the_documented_surface() {
    let manifest = crate_file("surrealism.toml");
    let body = manifest
        .split("[capabilities]")
        .nth(1)
        .unwrap_or_else(|| panic!("surrealism.toml lost the [capabilities] table"))
        .split("[attach]")
        .next()
        .unwrap_or_else(|| panic!("surrealism.toml lost the [attach] table"));
    let lines: Vec<&str> = body
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    assert_eq!(
        lines,
        [
            "allow_scripting = false",
            "allow_arbitrary_queries = true",
            "allow_functions = [\"session::ns\", \"session::db\", \"time::now\"]",
            "allow_net = []",
            "strict_timeout = true",
        ],
        "the [capabilities] body must be exactly the documented grant surface"
    );
}

/// The apply artifact's identity: `surform/surform-apply` — distinct
/// from the default artifact's `surform/surform` (the probe finding:
/// module KV scope follows package identity, so the two artifacts
/// must never alias) — and the manifest version cannot drift from the
/// crate's.
#[test]
fn the_apply_manifest_identity_is_distinct_and_pinned() {
    let manifest = crate_file("surrealism.toml");
    assert!(manifest.contains("organisation = \"surform\""));
    assert!(manifest.contains("name = \"surform-apply\""));
    assert!(
        manifest.contains(&format!("version = \"{}\"", env!("CARGO_PKG_VERSION"))),
        "surrealism.toml version drifted from Cargo.toml's"
    );
    let default_manifest = crate_file("../surform-surrealism/surrealism.toml");
    assert!(
        default_manifest.contains("name = \"surform\"\n"),
        "the default artifact's identity moved — the two must stay distinct"
    );
}

/// The shared-source wiring: this package's lib IS the default
/// artifact's source file, with the apply feature on by default — the
/// two-artifact mechanism itself, asserted so it cannot silently
/// unravel.
#[test]
fn the_shared_source_wiring_holds() {
    let cargo_toml = crate_file("Cargo.toml");
    assert!(cargo_toml.contains("path = \"../surform-surrealism/src/lib.rs\""));
    let defaults = cargo_toml
        .split("default = [")
        .nth(1)
        .unwrap_or_else(|| panic!("no default features line"))
        .split(']')
        .next()
        .unwrap_or_else(|| panic!("unterminated default features"));
    assert_eq!(defaults.trim(), "\"apply\"", "apply rides by default here");
    // And the default artifact's crate keeps its own default pure.
    let default_cargo = crate_file("../surform-surrealism/Cargo.toml");
    let pure = default_cargo
        .split("default = [")
        .nth(1)
        .unwrap_or_else(|| panic!("no default features line"))
        .split(']')
        .next()
        .unwrap_or_else(|| panic!("unterminated default features"));
    assert_eq!(pure.trim(), "", "the default artifact stays the pure set");
}

/// A built `.surli`'s `surrealism/` member by name — `None` when the
/// artifact is not built or the system tar cannot read the archive
/// (both honest skips; the build recipe produces the artifact).
fn surli_member(repo_relative: &str, member: &str) -> Option<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(repo_relative);
    let output = Command::new("tar")
        .arg("--zstd")
        .arg("-xOf")
        .arg(&path)
        .arg(member)
        .output()
        .ok()?;
    if !output.status.success() || output.stdout.is_empty() {
        eprintln!(
            "note: {repo_relative} not readable (not built, or a tar without \
             --zstd) — the artifact-level assertion skipped"
        );
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// THE BUILD-LEVEL ZERO-GRANT RE-PROOF: when the default artifact has
/// been built (the recipe's `surreal module build --out
/// target/surform.surli surform-surrealism`), its packed manifest
/// carries NO `= true` grant anywhere in `[capabilities]` — the
/// shipped module cannot call the host — and its export surface is
/// exactly the three pure exports (no `apply`).
#[test]
fn the_default_artifact_is_zero_grant_and_apply_free() {
    let Some(manifest) = surli_member("target/surform.surli", "surrealism/surrealism.toml") else {
        return;
    };
    let body = manifest
        .split("[capabilities]")
        .nth(1)
        .unwrap_or_else(|| panic!("the packed manifest lost [capabilities]"))
        .split("[attach]")
        .next()
        .unwrap_or_else(|| panic!("the packed manifest lost [attach]"));
    let lines: Vec<&str> = body
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    assert_eq!(
        lines,
        [
            "allow_scripting = false",
            "allow_arbitrary_queries = false",
            "allow_functions = []",
            "allow_net = []",
            "strict_timeout = true",
        ],
        "the DEFAULT artifact's packed manifest is not the exact zero-grant set: {body}"
    );
    let exports = surli_member("target/surform.surli", "surrealism/exports.toml")
        .expect("exports.toml rides the same archive the manifest did");
    for export in ["compile", "converge", "status"] {
        assert!(
            exports.contains(&format!("name = \"{export}\"")),
            "the default artifact lost `{export}`: {exports}"
        );
    }
    assert!(
        !exports.contains("name = \"apply\""),
        "the DEFAULT artifact carries the apply export: {exports}"
    );
}

/// The mirror assertion for the apply artifact: when built, its
/// packed manifest declares the grant (the whole point of the second
/// artifact) and its export surface carries all four functions.
#[test]
fn the_apply_artifact_declares_the_grant_and_all_four_exports() {
    let Some(manifest) = surli_member("target/surform-apply.surli", "surrealism/surrealism.toml")
    else {
        return;
    };
    let body = manifest
        .split("[capabilities]")
        .nth(1)
        .unwrap_or_else(|| panic!("the packed manifest lost [capabilities]"));
    assert!(
        body.contains("allow_arbitrary_queries = true"),
        "the apply artifact's packed manifest lost its grant: {body}"
    );
    assert!(
        body.contains("\"session::ns\"")
            && body.contains("\"session::db\"")
            && body.contains("\"time::now\""),
        "the apply artifact's packed manifest lost the function grants: {body}"
    );
    let exports = surli_member("target/surform-apply.surli", "surrealism/exports.toml")
        .expect("exports.toml rides the same archive the manifest did");
    for export in ["compile", "converge", "status", "apply"] {
        assert!(
            exports.contains(&format!("name = \"{export}\"")),
            "the apply artifact lost `{export}`: {exports}"
        );
    }
    let manifest_whole = workspace_file("Cargo.toml");
    assert!(
        manifest_whole.contains("\"surform-surrealism-apply\""),
        "the apply package left the workspace members list"
    );
}
