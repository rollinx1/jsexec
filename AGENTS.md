# jsexec

Standalone Rust JavaScript analysis CLI and library with generic AST querying,
source formatting, offline chunk discovery, and source map extraction. Rust 1.89+ is required;
Oxc dependencies are pinned together.

## Structure

- `src/main.rs`: thin entrypoint and exit status.
- `src/cli.rs`: Clap subcommands, input reading, output writing.
- `src/lib.rs`: reusable analysis modules with no filesystem or network operations.
- `src/source.rs`: shared input, error, diagnostic, and source-location types.
- `src/chunks/mod.rs`: extractor registry, one JavaScript parse per source, source
  format detection, common validation/resolution, deterministic evidence grouping.
- `src/chunks/javascript.rs`: imports, direct references, Vite dependency maps.
- `src/chunks/webpack.rs`: bounded static runtime filename evaluation.
- `src/chunks/manifest.rs`: JSON manifests and HTML script/preload references.
- `src/sourcemaps/`: JavaScript comment annotations, inline data-URI decoding,
  regular/indexed version-3 map parsing, original content and missing-source metadata.
- `src/query/`: generic ESTree arena index, CSS-style selector grammar/evaluation,
  source hash, bounded evidence rendering, and AST navigation.
- `src/cli/sourcemaps_http.rs`: HTTP retrieval, scoped request headers, redirects, limits.
- `src/cli/query.rs`: AST discovery/query/node commands and pagination/projection.
- `src/format/`: Oxc JS/TS/JSX/TSX formatting and html5ever HTML serialization.
- `src/cli/format.rs`: formatting stdout/output/check/write modes.
- `tests/`: library behavior and CLI integration tests.

## Commands and conventions

```sh
cargo run -- chunks <files>... [--base-url URL] [--extractor NAME,...] [--list]
cargo run -- extractors
cargo run -- sourcemaps <files|URLs>... [-H "NAME: VALUE"] [--fetch | --no-fetch] [--sources-dir NEW_DIR] [--list]
cargo run -- ast <files>...
cargo run -- query 'CallExpression[callee.name="fetch"]' <files>... [--fields PATH,...]
cargo run -- node <file> <ID> [--relation context] [--expect-hash HASH]
cargo run -- format <file|-> [--input-type tsx] [--output PATH | --write | --check]
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

`extract` aliases `chunks`; `-` reads stdin. `--input-type` overrides source detection,
`--output` writes a result file, and `--strict` fails before output on diagnostics.
`sourcemaps` accepts JavaScript and map inputs as files, stdin, or HTTP(S) URLs;
`sourcemap`/`source-maps` are aliases. `--input-type auto|js|map` controls detection. Its `--list` lists external
map references. Source files are written only with `--sources-dir`, into a new root
with per-map/per-source namespaces and sanitized paths. Preserve null versus empty
content and duplicate paths. Indexed section maps have independent source roots;
never replace virtual webpack URLs with an invented HTTP origin. URL inputs fetch
primary linked maps by default; local/stdin inputs require `--fetch`. `--list` and
`--no-fetch` skip linked maps. Only the CLI performs HTTP requests; `analyze_response`
accepts a selected response header without network operations. `SourceMap` overrides
annotations; `X-SourceMap` is the legacy fallback. Resolve links against final URLs,
strip all custom headers on origin changes, reject URL credentials/HTTPS downgrades,
and bound redirects, timeouts, and decompressed response sizes. Request/I/O failures
precede all writes; map diagnostics retain `--strict` behavior. Missing original
files and external indexed-map sections stay metadata; no recursive fetch or VLQ lookup.

Keep CLI concerns outside the library. Add detectors through `ChunkExtractor` and
the `Engine` registry. Retain original references and all available source evidence.
Do not invent missing map values, strip dynamic path components, execute JavaScript,
fetch discovered chunk/source URLs, or silently treat syntax errors as complete analysis.
Sourcemap retrieval is limited to explicit URL inputs and their primary map links,
or an explicit `--fetch` for local inputs. Document static-evaluation limits in
README. Shared source types remain re-exported from `chunks` for compatibility.

Generic querying uses Oxc's TypeScript-inclusive ESTree serialization, not a list
of hand-picked node kinds or domain detectors. Keep arbitrary fields and parent/edge
relationships accessible. IDs are file-local and tied to unchanged input and parser
version; hashes detect source changes. Preserve unpaired UTF-16 strings explicitly.
Keep selectors/evaluation bounded, diagnostics visible, and rendering truncation
explicit. `context` is syntactic, not scope/data-flow analysis. Query commands return
per-file report arrays; wildcard field projections always return arrays. Document
supported syntax and rejected features when extending the grammar.

Formatting emits source text, not JSON. Detect JS source mode from the actual input
filename, preserving CommonJS versus module mode and TS versus TSX grammar. Stdin
defaults to module JS; explicit `--input-type` selects another grammar. Fail on all
JS parser errors before writing; do not format a recovered partial AST. Default to
two spaces/double quotes. Keep formatting separate from unminification/renaming.
Only `--write` overwrites input; it conflicts with `--output` and `--check`. HTML
uses browser document repair and conservative structural indentation, preserving
mixed/raw text, template contents, namespaces, and correctly escaped values. Keep
the RcDom owner alive during traversal. No replacement source maps are produced;
formatted files require fresh query IDs/hashes. Document Oxc comment limitations
and HTML normalization behavior.
