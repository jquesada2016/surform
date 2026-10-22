//! The playground page: the one page that is
//! an app, not rendered markdown. The article HTML is built here —
//! the docs builder stamps in the keyword set (from the compiler's
//! own table, so highlighting cannot drift from the parser) and the
//! `docs/examples/*.surdl` files (run through the compiler right here:
//! a clean example that parses dirty, or `broken.surdl` parsing clean,
//! is a build error). The wasm bundle itself is a committed artifact
//! of `docs/editor/build.mjs`; a missing one is a build error
//! ([`require_bundle`], checked before the site writes).

use crate::site::{PageSpec, SiteCtx, json};
use crate::verify::BuildError;

/// The page's slug (nav href `playground.html`).
pub const SLUG: &str = "playground";

/// The synthetic spec the shell renders — top of the "Start" group;
/// no content file exists (the article is this module's product), so
/// the shell's "Edit this page" link is skipped for it.
pub const SPEC: PageSpec = PageSpec {
    slug: SLUG,
    file: "",
    title: "Playground",
    group: "Start",
};

/// One example the loader offers: the file's verified bytes plus what
/// the compiler said about them (the label announces a broken one).
struct Example {
    label: String,
    content: String,
    ok: bool,
}

/// Verifies and collects `docs/examples/*.surdl`. The files double as
/// session-fence inputs, so they are already on the verified path;
/// this re-check pins the loader's own contract: `broken.surdl` exists
/// to show diagnostics and must stay broken, everything else must
/// parse clean.
fn collect_examples(ctx: &SiteCtx, errors: &mut Vec<BuildError>) -> Vec<Example> {
    let mut names: Vec<String> = std::fs::read_dir(&ctx.examples_dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.file_name().to_string_lossy().to_string())
                .filter(|name| name.ends_with(".surdl"))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    let mut examples = Vec::new();
    for name in names {
        let path = ctx.examples_dir.join(&name);
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(reason) => {
                errors.push(BuildError::new(
                    "playground",
                    0,
                    format!("the example file {} is missing: {reason}", path.display()),
                ));
                continue;
            }
        };
        let (_, diags) = surform::parser::parse_file(&content);
        let broken = name.strip_suffix(".surdl") == Some("broken");
        if broken && diags.is_empty() {
            errors.push(BuildError::new(
                "playground",
                0,
                "examples/broken.surdl parses clean — it is supposed to be \
                 broken (the playground's diagnostics demo); fix the file \
                 or rename it"
                    .to_string(),
            ));
        }
        if !broken && !diags.is_empty() {
            errors.push(BuildError::new(
                "playground",
                0,
                format!(
                    "the example file {name} does not parse ({} diagnostic{s}): \
                     the loader only ships verified examples",
                    diags.len(),
                    s = if diags.len() == 1 { "" } else { "s" },
                ),
            ));
        }
        let stem = name.trim_end_matches(".surdl");
        examples.push(Example {
            label: if diags.is_empty() {
                stem.to_string()
            } else {
                format!("{stem} — broken on purpose, see the diagnostics")
            },
            content,
            ok: diags.is_empty(),
        });
    }
    // The editor's first document is EXAMPLES[0]: a clean schema
    // should land, so the broken demo sorts last (stable — each group
    // stays alphabetical).
    examples.sort_by_key(|ex| if ex.ok { 0 } else { 1 });
    examples
}

/// A JSON string literal safe for a `<script type="application/json">`
/// island: on top of [`json`]'s escaping, `<` becomes `\u003c` so a
/// literal can never close the tag early.
pub(crate) fn island_json(s: &str) -> String {
    json(s).replace('<', "\\u003c")
}

/// The compiler's keyword table as a JSON-array island body — shared
/// by the playground page and every page with interactive examples
/// (the editors' highlighting rides the compiler's own table).
pub(crate) fn keywords_island() -> String {
    format!(
        "[{}]",
        surform::surrealql_keywords()
            .iter()
            .map(|word| island_json(word))
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// The playground's article HTML: static markup the bundle wires up,
/// plus the two JSON islands.
pub fn build(ctx: &SiteCtx, errors: &mut Vec<BuildError>) -> String {
    let examples = collect_examples(ctx, errors);

    let mut examples_island = String::from("[");
    for (i, ex) in examples.iter().enumerate() {
        if i > 0 {
            examples_island.push(',');
        }
        examples_island.push_str(&format!(
            "{{\"label\":{},\"content\":{},\"ok\":{}}}",
            island_json(&ex.label),
            island_json(&ex.content),
            ex.ok,
        ));
    }
    examples_island.push(']');

    let keywords_island = keywords_island();

    format!(
        r#"<p>This page runs the Surform compiler in your browser, compiled to WebAssembly ({version} grammar) — the same parser and codegen the <code>surform</code> CLI runs. Edit the schema; the diagnostics, the DDL, and the canonical form update as you type. Nothing you type leaves this tab.</p>
<div class="pg-toolbar">
  <label class="pg-example"><span>Examples</span>
    <select id="pg-example" aria-label="Load an example schema"></select>
  </label>
  <button id="pg-compile" class="pg-compile" type="button">Compile now <kbd>⌘/Ctrl</kbd>+<kbd>↵</kbd></button>
  <button id="pg-format" class="pg-compile" type="button">Format</button>
  <span class="badge" id="pg-status">loading the compiler…</span>
  <span class="pg-version" id="pg-version"></span>
</div>
<div class="pg-editor"><div id="pg-editor" aria-label="SDL schema editor"></div></div>
<div id="pg-diags" class="pg-diags" aria-live="polite"></div>
<div class="pg-outputs">
  <div class="ex-block"><div class="ex-label">surform build — the DDL the engine would run</div>
    <div class="codebox"><span class="lang-chip">sql</span><button class="copy" type="button" aria-label="Copy DDL">Copy</button><pre><code id="pg-ddl-code"></code></pre></div>
  </div>
  <div class="ex-block"><div class="ex-label">surform fmt — the same schema, canonical form</div>
    <div class="codebox"><span class="lang-chip">sdl</span><button class="copy" type="button" aria-label="Copy canonical SDL">Copy</button><pre><code id="pg-canon-code"></code></pre></div>
  </div>
</div>
<p class="pg-foot">On a clean parse the emission is re-checked through the official SurrealQL parser; if the diagnostics card shows <code>SDL-E1060</code>, the compiler emitted DDL the parser rejects — a Surform bug (see <a href="verification.html">how this site is verified</a>). The editor's SDL highlighting uses the compiler's keyword table; SurrealQL itself — the DDL pane, and the expressions, function bodies, and views embedded in the SDL — is inked by SurrealDB's official SurrealQL grammar (the same one their editors ship).</p>
<script type="application/json" id="surform-keywords">{keywords_island}</script>
<script type="application/json" id="surform-examples">{examples_island}</script>
<script type="module" src="assets/editor/{bundle_script}"></script>
"#,
        version = surform::SURREALQL_VERSION,
        keywords_island = keywords_island,
        examples_island = examples_island,
        bundle_script = ctx.bundle_script,
    )
}

/// The committed wasm bundle this site loads — checked so a build
/// without its compiler fails loudly instead of shipping a dead
/// playground, and named so pages (and the browser) always fetch the
/// exact bytes the build verified. `build.mjs` writes `bundle.txt`
/// with the content-hashed bundle name and renames the wasm to match
/// its own hash; the wasm EMBEDS the compiler, so this rebuild is what
/// carries compiler changes (a lint flip, an emission fix) into the
/// site's interactive examples — commit the renamed artifacts with any
/// unit that touches the compiler, the binding, or the app.
pub fn require_bundle(editor_assets: &std::path::Path, errors: &mut Vec<BuildError>) -> String {
    let manifest = editor_assets.join("bundle.txt");
    let bundle = match std::fs::read_to_string(&manifest) {
        Ok(name) => name.trim().to_string(),
        Err(reason) => {
            errors.push(BuildError::new(
                "playground",
                0,
                format!(
                    "the bundle manifest docs/assets/editor/bundle.txt is missing — \
                     build it with `cd docs/editor && npm install && node build.mjs`: {reason}"
                ),
            ));
            return "editor.js".to_string();
        }
    };
    for required in [bundle.as_str(), "surform_editor.js"] {
        if !editor_assets.join(required).exists() {
            errors.push(BuildError::new(
                "playground",
                0,
                format!(
                    "the playground bundle is missing docs/assets/editor/{required} — \
                     build it with `cd docs/editor && npm install && node build.mjs`"
                ),
            ));
        }
    }
    let wasm_present = std::fs::read_dir(editor_assets)
        .map(|entries| {
            entries.filter_map(|entry| entry.ok()).any(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                name.starts_with("surform_editor_bg.") && name.ends_with(".wasm")
            })
        })
        .unwrap_or(false);
    if !wasm_present {
        errors.push(BuildError::new(
            "playground",
            0,
            "the playground bundle is missing the hashed docs/assets/editor/\
             surform_editor_bg.*.wasm — build it with `cd docs/editor && npm install \
             && node build.mjs`"
                .to_string(),
        ));
    }
    bundle
}
