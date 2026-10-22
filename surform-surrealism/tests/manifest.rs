//! The manifest + feature scaffolding pins: the DEFAULT
//! artifact is pure — zero capabilities declared — and the `apply`
//! feature exists as the second artifact's scaffold without adding
//! surface. These are text assertions on the crate's own files
//! (committed, static): no TOML dependency is bought to parse what is
//! effectively a golden (the hand-rolled-reader precedent).

use std::fs;
use std::path::PathBuf;

fn crate_file(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(name);
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("{} unreadable: {err}", path.display()))
}

/// The default artifact declares ZERO capabilities: the `[capabilities]`
/// section body — bounded by the next header, comments and blanks
/// stripped — is EXACTLY the five zero-grant lines, so a future grant
/// key appended by a bump cannot slip in unasserted. Still text-only:
/// no TOML dependency is bought to parse what is effectively a golden.
/// (the reference module's all-off shape; the pure artifact stays
/// reproducible from the SDL source.)
#[test]
fn default_manifest_declares_zero_capabilities() {
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
            "allow_arbitrary_queries = false",
            "allow_functions = []",
            "allow_net = []",
            "strict_timeout = true",
        ],
        "the [capabilities] body must be exactly the five zero-grant lines"
    );
}

/// The scaffold's head keys: the component ABI the 3.3 engine loads.
/// TOML top-level keys must precede the first table header — the
/// assertion pins that placement, not byte 0 (comments lead the
/// file).
#[test]
fn manifest_carries_the_abi_head_keys() {
    let manifest = crate_file("surrealism.toml");
    let head = manifest.split("[package]").next().unwrap_or_default();
    assert!(
        head.contains("target = \"rust\""),
        "surrealism.toml lost `target`"
    );
    assert!(head.contains("abi = 2"), "surrealism.toml lost `abi = 2`");
}

/// The package identity: the module registers as
/// `mod::surform` under the `surform` organisation, and the manifest
/// version cannot drift from the crate's.
#[test]
fn manifest_identity_matches_the_crate() {
    let manifest = crate_file("surrealism.toml");
    assert!(manifest.contains("organisation = \"surform\""));
    assert!(manifest.contains("name = \"surform\""));
    assert!(
        manifest.contains(&format!("version = \"{}\"", env!("CARGO_PKG_VERSION"))),
        "surrealism.toml version drifted from Cargo.toml's"
    );
}

/// The dependency pins: the SDK is EXACT-pinned (floor-move style —
/// the pin rides the surform SurrealDB floor), and the core rides by
/// path.
#[test]
fn cargo_pins_the_sdk_exactly() {
    let cargo_toml = crate_file("Cargo.toml");
    assert!(
        cargo_toml.contains("surrealism = \"=0.6.0\""),
        "the surrealism SDK pin must stay exact (=0.6.0) until a floor-move-shaped bump"
    );
    assert!(cargo_toml.contains("surform = { path = \"..\" }"));
}

/// The feature scaffolding: default = the pure set (empty), `apply`
/// declared and EMPTY — its existence is load-bearing (the second
/// artifact), its emptiness is the default artifact's boundary (the
/// apply package owns the behavior).
#[test]
fn feature_scaffolding_is_pure_by_default() {
    let cargo_toml = crate_file("Cargo.toml");
    let defaults = cargo_toml
        .split("default = [")
        .nth(1)
        .unwrap_or_else(|| panic!("no default features line"))
        .split(']')
        .next()
        .unwrap_or_else(|| panic!("unterminated default features"));
    assert_eq!(
        defaults.trim(),
        "",
        "default features must stay the pure set"
    );
    assert!(
        cargo_toml.contains("apply = []"),
        "the apply scaffold feature went missing"
    );
}
