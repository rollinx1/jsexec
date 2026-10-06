# jsexec

Command-line JavaScript and TypeScript analysis. Query syntax trees, inspect node
relationships, discover JavaScript chunks, recover source files from source maps,
and format code.

Queries use a CSS-style selector syntax and return JSON with node fields, code
excerpts, and source locations. You can inspect a function call, object property,
type annotation, or JSX element through the same interface. This makes jsexec useful
for investigating bundles, writing analysis scripts, and giving coding agents access
to specific parts of a source file.

JavaScript is parsed, never executed. Analysis accepts local files and stdin.
The `sourcemaps` command also retrieves HTTP(S) scripts and maps. Chunk discovery
returns URLs as data; it does not download assets.

## Contents

- [Installation](#installation)
- [Quick start](#quick-start)
- [AST queries](#ast-queries)
- [Chunk discovery](#chunk-discovery)
- [Source maps](#source-maps)
- [Formatting](#formatting)
- [Input, output, and errors](#input-output-and-errors)
- [Memory use](#memory-use)
- [Library](#library)
- [Development](#development)
- [Limits](#limits)

## Installation

Requires **Rust 1.89 or newer**. From the repository root:

```sh
cargo install --path . --locked
jsexec --version
```

Cargo installs the binary in its bin directory, usually `~/.cargo/bin`. Make sure
that directory is on your `PATH`.

To build without installing:

```sh
cargo build --release --locked
./target/release/jsexec --help
```

During development, use `cargo run --` in place of `jsexec`:

```sh
cargo run -- query 'CallExpression' examples/query.ts
```

## Quick start

The repository includes a TypeScript example, a bundle-runtime example, and a
source map with embedded original files.

```sh
# List node kinds and fields present in a file.
jsexec ast examples/query.ts

# Find fetch calls and inspect the callee and argument types.
jsexec query 'CallExpression[callee.name="fetch"]' examples/query.ts \
  --fields 'callee.name,arguments.*.type'

# Find functions containing those calls.
jsexec query 'FunctionDeclaration:has(CallExpression[callee.name="fetch"])' \
  examples/query.ts

# Discover chunk references and resolve them against the original script URL.
jsexec chunks examples/runtime.js \
  --base-url https://example.com/assets/runtime.js --list

# Read original source content from a saved source map.
jsexec sourcemaps examples/source-map.json

# Print a readable version of the runtime.
jsexec format examples/runtime.js
```

| Command | Purpose | Default output |
| --- | --- | --- |
| `ast` | List node kinds, counts, and available fields | JSON array of file reports |
| `query` | Select nodes by kind, fields, and tree relationships | JSON array of file reports |
| `node` | Inspect a node ID or navigate its relationships | JSON array of file reports |
| `chunks` | Discover imports, asset references, and runtime chunk filenames | JSON object |
| `sourcemaps` | Discover map references and recover embedded sources | JSON object |
| `format` | Format JavaScript, TypeScript, JSX, TSX, or HTML | Source text |
| `extractors` | List registered chunk extractors | One name per line |

Run `jsexec COMMAND --help` for command-line options. `extract` aliases `chunks`;
`sourcemap` and `source-maps` alias `sourcemaps`.

## AST queries

### Inspect the tree

```sh
jsexec ast app.js
jsexec ast main.js vendor.js --strict
```

Each report includes the filename, a SHA-256 source hash, the root node ID, the total
node count, and a `kinds` array. Each kind lists its count and the fields observed on
nodes of that kind.

Node names and fields follow Oxc's TypeScript-inclusive ESTree representation.
For example, strings, numbers, bigint, and regex literals use `Literal`;
TypeScript nodes retain names such as `TSNumberKeyword`. Use `ast` to check which
types and fields actually occur in a file before writing a query.

### Select nodes

```sh
# A specific function call.
jsexec query 'CallExpression[callee.name="fetch"]' app.js

# A member call such as client.post(...).
jsexec query 'CallExpression[callee.property.name="post"]' app.js

# Calls with a particular literal argument.
jsexec query 'CallExpression[arguments.0.value="/api/users"]' app.js

# Object properties, including their values.
jsexec query 'Property[key.name="path"]' app.js --fields 'key.name,value.value'

# Calls anywhere beneath a function declaration.
jsexec query 'FunctionDeclaration CallExpression' app.js

# Both function declarations and arrow functions.
jsexec query ':is(FunctionDeclaration, ArrowFunctionExpression)' app.js

# TypeScript syntax can be queried directly.
jsexec query 'TSNumberKeyword' app.ts
```

Quote selectors at the shell so brackets, spaces, and wildcard characters reach
jsexec unchanged. Selectors and field names are case-sensitive.

### Follow a result

Every returned node has an `id`, a parent ID, and a `field` describing its edge from
the parent. Fields such as `callee`, `arguments.0`, or `body.2` identify where a node
sits in that parent's syntax.

```sh
jsexec node app.js n42
jsexec node app.js n42 --relation children
jsexec node app.js n42 --relation ancestors
jsexec node app.js n42 --relation context --max-code 3000
```

Here `n42` is a placeholder: use an ID returned for the same file. Available
relations are:

| Relation | Returns |
| --- | --- |
| `self` | The selected node; this is the default |
| `parent` | Its immediate parent |
| `children` | Its immediate child nodes |
| `descendants` | All nodes below it, excluding itself |
| `ancestors` | Parents from nearest to outermost |
| `siblings` | Other nodes with the same immediate parent |
| `context` | The nearest function, class, static block, or program, including itself |

Children, descendants, and siblings follow source tree order. Context is syntactic:
it does not resolve lexical bindings, imports, or data flow.

IDs are local to a file and deterministic for unchanged source with the same tool
and parser version. Formatting or editing a file can change its IDs. Use the report's
`hash` with `--expect-hash` to reject a changed file before inspecting a saved ID.

With `jq` installed, a query result can feed directly into navigation:

```sh
jsexec query 'CallExpression[callee.name="fetch"]' examples/query.ts -o matches.json

node_id=$(jq -r '.[0].nodes[0].id' matches.json)
source_hash=$(jq -r '.[0].hash' matches.json)

jsexec node examples/query.ts "$node_id" \
  --relation context --expect-hash "$source_hash" --max-code 3000
```

`query` also accepts `--expect-hash`. For a report containing multiple files, retain
the filename alongside each ID and hash; pagination and IDs are independent per file.

### Selector reference

The syntax is inspired by [esquery](https://github.com/estools/esquery), with field
paths and source metadata extensions. The supported grammar is described below;
it is not a complete esquery implementation.

| Selector | Meaning |
| --- | --- |
| `Identifier` | Nodes of a specific kind |
| `*` | Every node |
| `[computed]` | A field exists |
| `[async=true]` | A field equals a typed value |
| `[callee.property.name="post"]` | Follow a nested node/object field path |
| `[arguments.0.value="/api"]` | Read an array entry by zero-based index |
| `[arguments.*.value^="/api"]` | Match any entry through a wildcard path |
| `[arguments.length>=2]` | Compare an array's length |
| `[value*="api"]` | String contains |
| `[value^="/api"]` | String starts with |
| `[value$=".js"]` | String ends with |
| `[value!=null]` | A present scalar value differs from null |
| `[value>2]` | Numeric comparison; `<`, `<=`, and `>=` are also supported |
| `[value=/^api/i]` | Regex match |
| `FunctionDeclaration CallExpression` | Match descendants of the preceding selector |
| `CallExpression > Identifier.callee` | Match a direct child on the `callee` edge |
| `FunctionDeclaration > Identifier.params.*` | Match direct children on indexed parameter edges |
| `Identifier, Literal` | Union of selectors, deduplicated |
| `:is(FunctionDeclaration, ArrowFunctionExpression)` | Match any enclosed selector |
| `CallExpression:not([callee.name="fetch"])` | Exclude nodes matching the enclosed selector |
| `FunctionDeclaration:has(CallExpression)` | Has a matching descendant |
| `CallExpression:has(> Identifier.callee)` | Has a matching direct child |
| `Identifier[@line>=10]` | Match source metadata |

Combine attributes to require several conditions:

```sh
jsexec query 'CallExpression[callee.property.name="post"][arguments.length>=2]' app.js
jsexec query 'Literal[value^="/api/"]:not([value$=".js"])' app.js
```

Attribute values accept single- or double-quoted strings with JSON escapes, numbers,
booleans, null, and bare strings. Equality compares scalar values; it does not compare
whole objects, arrays, or node references.

Existence tests include fields holding `false` or `null`. All comparisons require
the path to exist, including `!=`; to select nodes without a field, use `:not([field])`.
For wildcard paths, any resolved scalar value can satisfy a comparison. Edge
selectors match a dotted prefix, and `*` matches one edge component.

`:has()` accepts compound selectors, optionally comma-separated. An initial `>`
restricts all its alternatives to direct children. Descendant/child chains inside
`:has()` are rejected; use an outer tree relation or nested `:has()` instead.
`:is()` and `:not()` accept full selectors.

Regex attributes support `=` and `!=`, with `i`, `m`, and `s` flags. They use Rust
regex syntax; lookaround and backreferences are unsupported. Sibling combinators,
positional selectors, subject selectors, and other pseudo-selectors are also
unsupported. Invalid syntax fails rather than being silently ignored.

Source metadata is available in attributes and `--fields` projections:

| Field | Value |
| --- | --- |
| `@id` | File-local node ID |
| `@field` | Edge path from the parent, or null for the root |
| `@text` | Complete source text covered by the node |
| `@start` | Inclusive UTF-8 byte offset |
| `@end` | Exclusive UTF-8 byte offset |
| `@line` | One-based starting line |

For example, `CallExpression[@text*="fetch"]` searches call source text. Type and
field filters are preferable when the structure is known: broad `@text` queries
also scan overlapping parent expressions.

### Fields, pagination, and output

Use `--fields` to return selected paths instead of every immediate field:

```sh
jsexec query 'CallExpression' app.js --fields 'callee.name,arguments.*.type'
jsexec query 'Literal' app.js --fields value --fields raw
```

Nested nodes are represented by references such as
`{"node":"n123","kind":"Identifier"}`. Follow those IDs with `node` to inspect the
referenced nodes. Wildcard projections always return arrays; a missing non-wildcard
projection is null. Selector matching uses complete field values even when output
is truncated.

```sh
jsexec query 'Identifier' app.js --limit 20 --offset 20
jsexec query 'CallExpression' app.js --limit 0
jsexec node app.js n42 --relation descendants --limit 50 --max-code 200
```

`query` and `node` share these options:

| Option | Default | Purpose |
| --- | --- | --- |
| `--limit` | `20` | Maximum nodes returned per file; `0` returns counts only |
| `--offset` | `0` | Number of matching nodes to skip |
| `--max-code` | `500` | Maximum Unicode characters in each code excerpt |
| `--max-value` | `500` | Maximum Unicode characters in each string field |
| `--max-items` | `20` | Maximum entries in each nested array/object field |
| `--fields` | All immediate fields | Dotted paths, repeated or comma-separated |
| `--expect-hash` | None | Require a particular SHA-256 source hash |
| `-o`, `--output` | stdout | Write the JSON report to a file |
| `--strict` | Off | Fail on parser diagnostics before writing a report |

An output limit of zero omits the corresponding content. `code_truncated` and
`truncated_fields` identify clipped evidence. The total `match_count` is independent
of the returned page. Use `next_offset` to continue while `has_more` is true;
count-only queries leave `next_offset` null. An offset past the last match returns
an empty page with the complete count.

For example:

```sh
printf 'fetch("/api/users");\n' | jsexec query 'CallExpression[callee.name="fetch"]' - \
  --fields 'callee.name,arguments.*.value'
```

```json
[
  {
    "file": "<stdin>",
    "hash": "5e4bb9544c5030608e2650ca3cf635930554cc0bcc418f5ebae316ebde02aff2",
    "partial": false,
    "diagnostics": [],
    "match_count": 1,
    "offset": 0,
    "has_more": false,
    "next_offset": null,
    "nodes": [
      {
        "id": "n2",
        "kind": "CallExpression",
        "location": { "start": 0, "end": 19, "line": 1, "column": 1 },
        "parent": "n3",
        "field": "expression",
        "child_count": 2,
        "code": "fetch(\"/api/users\")",
        "code_truncated": false,
        "fields": {
          "callee.name": "fetch",
          "arguments.*.value": ["/api/users"]
        },
        "truncated_fields": []
      }
    ]
  }
]
```

Locations refer to the input file. Offsets are UTF-8 bytes with an exclusive end;
lines and columns are one-based, with columns counted in Unicode characters.
The source hash covers the complete input text, including whitespace.

JavaScript strings can contain unpaired UTF-16 surrogates. Such values are retained
as `{"encoding":"utf16","units":[...]}` rather than replaced or discarded. Their
code units can be queried through paths such as `value.units.0`.

## Chunk discovery

```sh
jsexec chunks main.js runtime.js
jsexec chunks runtime.js --base-url https://example.com/assets/runtime.js --list
jsexec chunks runtime.js --extractor webpack,imports
jsexec chunks manifest.json --input-type json
jsexec chunks page.html --input-type html
jsexec extractors
```

All six extractors run by default. Select them with `--extractor`, repeated or
comma-separated:

| Extractor | Recognizes |
| --- | --- |
| `imports` | Static/dynamic imports and re-exports with static JavaScript paths |
| `references` | URL-like JavaScript asset strings and templates without interpolation |
| `vite` | Dependency arrays in `__vite__mapDeps` and `__vitePreload` arguments |
| `webpack` | `.u` filename functions, static maps/concatenation/templates/fallbacks, known `.e(id)` calls, and unambiguous static `.p` public paths |
| `manifest` | Nested JavaScript asset strings in JSON and HTML `application/json` scripts |
| `html` | Script `src`, module-preload links, and script-preload links |

Recognized asset extensions are `.js`, `.mjs`, and `.cjs`, including query strings
and fragments. Import extraction accepts ordinary relative filenames such as
`./login.js`. Generic literal scanning excludes bare filenames to reduce false
positives. CSS, source maps, data URLs, and paths with unresolved dynamic components
are excluded from chunk results.

Input type is detected from the filename and content. Override it with
`--input-type auto|js|json|html`, including for stdin:

```sh
cat manifest.json | jsexec chunks - --input-type json -o chunks.json
```

The report is an object with `chunks` and `diagnostics`. Each chunk has a `value`
and an `evidence` array. Evidence retains the filename, extractor, and original
reference; JavaScript evidence also includes a source location. JSON/HTML references
do not carry locations in this implementation.

Without `--base-url`, values retain the discovered references. With it, references
are resolved to HTTP(S) URLs. Use the original script URL, or a directory URL ending
in `/`. One base URL applies to every input in that invocation. Webpack public paths
are used where they can be determined statically.

Results are sorted and repeated values are grouped across inputs and extractors,
while all distinct evidence is retained. `--list` prints one deduplicated value per
line and sends diagnostics to stderr. Invalid extractor names and non-HTTP(S) base
URLs fail before analysis.

Webpack evaluation is limited to supported single-expression returns. Map-only
canonical numeric IDs follow Webpack's numeric-ID convention; explicit lazy-load
calls retain the distinction between numeric and string IDs. Runtime aliases,
arbitrary executable expressions, and bundled-module reconstruction are outside
this extractor's scope.

## Source maps

`sourcemaps` accepts local files, stdin, and HTTP(S) URLs. It reads version-3 maps,
JavaScript `sourceMappingURL` comments, and HTTP `SourceMap` response headers. It
decodes base64 or percent-encoded inline maps, flattens indexed maps, and recovers
original content from `sourcesContent`.

```sh
# Report an external map reference relative to the script's original URL.
jsexec sourcemaps bundle.js --base-url https://example.com/assets/bundle.js --list

# Retrieve a script and its linked map, then recover embedded source files.
jsexec sourcemaps https://example.com/assets/bundle.js --sources-dir recovered

# Retrieve a map directly with authentication headers.
jsexec sourcemaps https://example.com/assets/bundle.js.map \
  -H "Authorization: Bearer $TOKEN" \
  -H 'Cookie: session=example' --sources-dir authenticated-sources

# Fetch the HTTP map referenced by a saved script.
jsexec sourcemaps bundle.js --base-url https://example.com/assets/bundle.js --fetch

# Read a saved map or an inline map embedded in a script.
jsexec sourcemaps bundle.js.map
jsexec sourcemaps inline-bundle.js

# Recover files from the included example and save the report.
jsexec sourcemaps examples/source-map.json --sources-dir recovered -o sources.json

# Read a saved map from stdin and require clean input.
cat bundle.js.map | jsexec sourcemaps - --input-type map --strict
```

Use `--input-type auto|js|map` to control detection; `json` is an alias for `map`.
The final matching JavaScript annotation is authoritative when no response header
is present. `SourceMap` takes precedence over annotations; legacy `X-SourceMap` is
accepted when `SourceMap` is absent. Header names are case-insensitive. This follows
[ECMA-426's HTTP linking rules](https://tc39.es/ecma426/2024/#sec-linking-through-http-headers).
Annotation-like text inside strings, templates, regexes, or ordinary comments is
ignored. References resolve against the final script URL after redirects; original
source paths resolve against the final downloaded map URL.

The meaning of `--base-url` depends on the input:

| Input | Supply |
| --- | --- |
| JavaScript containing an annotation | The original script URL |
| Saved source map | The original map URL |

For URL inputs, the downloaded response URL supplies the base automatically;
`--base-url` applies to local files and stdin. A URL can point to either a script or
a map. Automatic detection uses the URL path without its query string; use
`--input-type map` for map endpoints with an ambiguous name or response body.

### Retrieval options

| Option | Behavior |
| --- | --- |
| `-H`, `--header 'NAME: VALUE'` | Add a request header; repeat for multiple headers, including repeated names |
| `--fetch` | Download linked HTTP(S) maps for local files or stdin |
| `--no-fetch` | Report linked maps without downloading them; URL inputs themselves are still retrieved |
| `--list` | List external map references/URLs without downloading linked maps |
| `--timeout SECONDS` | Request timeout, including its redirect chain; default 30, range 1–86400 |
| `--max-bytes BYTES` | Maximum decompressed body size per response; default 52428800 (50 MiB) |

URL inputs download linked maps by default. Local files and stdin remain offline
unless `--fetch` is supplied; a relative reference then requires `--base-url`.
`--fetch` and `--no-fetch` conflict. `--list` skips linked-map downloads even when
`--fetch` is present. It emits deduplicated references, including URL sections in
indexed maps. A direct map URL is still downloaded and parsed with `--list`.

Custom headers apply to each explicitly supplied URL and same-origin linked maps.
For local inputs with `--fetch`, the `--base-url` origin determines where headers
may be sent. Without a base URL, an absolute map reference supplies that origin.
A different scheme, host, or port counts as a different origin. All custom headers are removed when a redirect leaves that origin and remain removed
for the rest of the redirect chain. Cross-origin map links are retrieved without
custom headers; pass an authenticated map URL directly when it needs its own
credentials. Headers are neither included in reports nor echoed in validation
errors. There is no cookie jar; use an explicit `Cookie` header when needed.

Retrieval follows at most 10 redirects per request, verifies HTTPS certificates,
and rejects HTTPS-to-HTTP redirects and map links. URL credentials are rejected;
use `--header` for authentication. Gzip, Brotli, and deflate responses are decoded
before the size limit is checked. HTTP failures, timeouts, oversized bodies, and
non-UTF-8 responses fail before reports or recovered files are written. Malformed
maps produce diagnostics; `--strict` makes those diagnostics fail before writes.

Only the primary linked map is downloaded. External indexed-map sections and
sources without embedded content remain metadata; they are not recursively fetched.
No map filenames are guessed when a script has no header or annotation.

### Reports and recovered files

The JSON report contains `maps` and `diagnostics`. Each map records its `input`,
`kind` (`file`, `inline`, or `external`), sources, and external section references.
`file` includes directly supplied map URLs; a linked map keeps `external` after it
is downloaded. Its `url` records the final map URL and `reference` retains the
original link. Header references have no annotation location.
Depending on the input, it also includes the original map reference/resolved URL,
annotation location, and generated filename. Inline data payloads are not repeated.

Source entries include:

| Field | Meaning |
| --- | --- |
| `index` | Position in the section's original source array |
| `section` | Indexed-map section ancestry |
| `path` | Original source path, including null entries |
| `source_root` | Original `sourceRoot`, when present |
| `resolved_url` | Resolved source URL, when available |
| `content` | Embedded source text, or null when unavailable |
| `ignored` | Whether the source belongs to the map's ignore list |
| `extracted_path` | Path relative to the recovery directory, after extraction |

Missing content is null; an empty string represents an embedded empty file. Duplicate
source names and array positions are preserved. `sourceRoot` prefixes source names
as a directory; virtual URLs such as `webpack://` retain their scheme. Embedded
sections have independent roots.

`--sources-dir` must name a new directory whose parent already exists. Files use
per-map/per-source namespaces:

```text
recovered/
  map-0001/
    source-0001/src/main.ts
    source-0002/src/empty.ts
```

This is the layout produced by `examples/source-map.json`. The missing third source
is reported but creates no file. Paths are sanitized so absolute paths, parent
traversal, virtual schemes, and unsafe filename characters cannot select locations
outside the recovery directory. Existing directories and report/source collisions
are rejected. A filesystem or output failure can leave files already recovered.

Recovered files can be passed to `ast`, `query`, and `format`:

```sh
jsexec ast recovered/map-0001/source-0001/src/main.ts
jsexec query 'Literal' recovered/map-0001/source-0001/src/main.ts --fields value
```

Invalid map versions, malformed fields or encodings, and source/content length
mismatches produce diagnostics. Available valid sources are recovered by default;
`--strict` fails before extraction on any diagnostic. Recovery does not decode VLQ
mappings or translate generated positions into original positions.

## Formatting

```sh
jsexec format bundle.js
jsexec format bundle.js -o readable.js
jsexec format app.ts --write
jsexec format app.js --check
cat app.tsx | jsexec format - --input-type tsx --single-quote --indent-width 4
jsexec format page.html
```

Formatting emits source text. The input is changed only with `--write`; `--output`
saves a separate copy. `--check` emits no source and exits nonzero if formatting would
change the input. These three options are mutually exclusive. `--write` requires a
file; use `--output` to save stdin.

JavaScript mode is selected from the actual filename: `.js`, `.mjs`, `.cjs`, `.jsx`,
`.ts`, `.tsx`, `.mts`, and `.cts` are supported. Types and JSX syntax are retained.
Unknown extensions require an explicit `--input-type js|jsx|ts|tsx|html`. Stdin
defaults to module JavaScript. Explicit `js` also selects module JavaScript, while
automatic `.cjs`/`.cts` detection preserves CommonJS mode.

Defaults are two spaces and double quotes. `--indent-width` accepts 1–16, and
`--single-quote` changes JavaScript string quoting. Formatting prints parsed syntax
without symbol renaming or unminification transformations. JavaScript parser errors
always fail before output or writes. Supported statement, license, JSDoc, and
import/pure annotation comments are retained by Oxc; arbitrary expression-level
comments may be omitted.

HTML mode is inferred from `.html`/`.htm`, or selected with `--input-type html`.
Browser-style document parsing repairs malformed markup and can insert `html`,
`head`, and `body` wrappers. Structural containers receive indentation; inline/mixed
text and raw script/style/preformatted contents retain their parsed whitespace.
Attributes, escaping, comments, void tags, namespaces, template contents, and legacy
doctype IDs are retained. Embedded JS, JSON, and CSS are preserved as content.

HTML serialization can normalize tag/entity spellings and change whitespace between
structural elements; it does not consult CSS. It adds no trailing newline, which
could otherwise move into the body when the document is parsed again. The obsolete
`plaintext` element is rejected because serializing closing tags changes its content.
`--single-quote` applies only to JavaScript.

Formatting changes source offsets, hashes, and node IDs. Query the formatted file
again when navigating it. No replacement source map is generated; original maps
still describe the original source bytes.

## Input, output, and errors

All file inputs are UTF-8. Use `-` to read stdin; it can appear at most once in a
multi-file command. `ast`, `query`, `chunks`, and `sourcemaps` accept multiple inputs.
`node` and `format` accept one input. HTTP(S) URL inputs and request headers are
supported only by `sourcemaps`.

For `ast`, `query`, and `node`, recognized JavaScript/TypeScript extensions select
the parser mode. Stdin and unknown extensions are parsed as TSX. Chunk and source
map commands have their own automatic detection and `--input-type` options;
formatting uses the filename and defaults stdin to module JavaScript.

Analysis reports go to stdout unless `-o`/`--output` is supplied. `--list` changes
chunk and source map output to one value per line; `extractors` lists names.
Errors go to stderr, so stdout can be piped into JSON-processing tools. Output paths
cannot target an input file; `format --write` is the explicit in-place operation.

AST queries and extraction can return recovered findings alongside diagnostics.
`ast`, `query`, and `node` mark such reports `partial: true`. A successful exit alone
does not mean the input had valid syntax: check diagnostics, or use `--strict` to
require a clean report. Formatting always rejects JavaScript parser errors.

| Exit status | Meaning |
| --- | --- |
| `0` | The command completed successfully |
| `1` | Analysis/I/O failure, strict-mode diagnostics, or a formatting check that differs |
| `2` | CLI parsing failure, such as an unknown option or a missing argument |

## Memory use

Multi-file commands read and analyze one input at a time. AST indexes use shared
strings and sorted flat field storage. Query pagination retains the requested page
rather than a vector of every matching node ID, while still reporting exact counts.

Each query still parses and indexes the entire current file. Result limits and
field projections control output size; they do not make parsing incremental. The
ESTree bridge serializes one top-level subtree at a time. A bundle inside one large
function can therefore require a large intermediate subtree buffer.

JSON and list reports are staged in a buffered temporary file before being copied
to stdout or `--output`. This avoids accumulating reports and their serialized
copies in memory, and keeps late analysis failures from producing partial output.
The operating system's temporary directory must have room for the report; temporary
storage can itself be backed by memory depending on the system. Unix temporary
files are unlinked immediately; other platforms remove them on normal exit.

Sourcemap parsing validates JSON while skipping unused `mappings`, `names`, and
vendor fields without allocating their values. Embedded source strings are moved
into the report. Recovered content remains in memory because the JSON report
includes it; the CLI does not fetch missing sources to fill that content.

To measure peak process memory on Linux:

```sh
cargo build --release --locked
python3 scripts/benchmark_memory.py --binary target/release/jsexec

# Compare two builds using identical generated inputs.
python3 scripts/benchmark_memory.py --binary target/release/jsexec \
  --compare-binary /path/to/previous/jsexec
```

The benchmark uses generated JavaScript, a bundle wrapped in one function, multiple
files, and a map with embedded content. Results depend on node density and allocator
behavior; file size alone is not a reliable predictor of memory use.

## Library

The Rust library exposes the same analysis primitives without filesystem or network
operations. Callers provide a `Source` with a filename and source text.

```rust
use jsexec::query::{QueryIndex, RenderOptions, Selector};
use jsexec::source::Source;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source = Source {
        name: "app.js".into(),
        code: "fetch('/api/users');".into(),
    };
    let index = QueryIndex::parse(&source)?;
    let selector = Selector::parse("CallExpression[callee.name='fetch']")?;
    let page = index.select(&selector, 0, 20, &RenderOptions::default())?;

    for node in page.nodes {
        println!("{}:{}: {}", node.location.line, node.location.column, node.code);
    }
    Ok(())
}
```

| Module | Entry points |
| --- | --- |
| `query` | `QueryIndex`, `Selector`, `RenderOptions`, `Relation` |
| `chunks` | `Engine`, `Options`, `ChunkExtractor` |
| `sourcemaps` | `analyze`, `analyze_response`, `Options`, `Report` |
| `format` | `format`, `Options`, `InputKind` |
| `source` | `Source`, `Location`, `Diagnostic`, `Error` |

For custom chunk detectors, implement `ChunkExtractor` and register it on an
`Engine`. `Engine::new()` creates an empty registry; `Engine::default()` includes
the built-in detectors. Selected extractors share one JavaScript parse per source;
the engine handles asset validation, URL resolution, evidence grouping, and output
ordering. `Engine::analyze_iter` accepts fallible iterators of owned or borrowed
sources for callers that read inputs individually.

## Development

```sh
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked
```

CLI input/output lives in `src/cli.rs` and `src/cli/`. Analysis and formatting live
in separate library modules under `src/`. Integration tests cover library behavior,
command-line output, pagination, diagnostics, file writes, and HTTP retrieval using
local test servers. HTTP tests require permission to bind loopback sockets.

Parsing and JavaScript generation use [Oxc](https://oxc.rs). HTML parsing and
serialization use [html5ever](https://github.com/servo/html5ever).

## Limits

- Queries inspect saved syntax. There is no runtime evaluation, lexical binding
  resolution, cross-file symbol resolution, or data-flow analysis.
- Comments are not queryable AST nodes. Source map annotations are handled separately
  by `sourcemaps`.
- Each query invocation parses and indexes the entire file, even when `--limit`
  returns only a few nodes. There is no persistent index or cross-command cache.
- Selectors are limited to 16 KiB, 64 compounds, and 16 levels of pseudo nesting.
  Evaluation is bounded to 10 million node/compound checks and 256 MB of `@text`
  comparisons. Exceeding a budget returns an error; narrow or split the query.
- Chunk discovery does not download assets, unpack bundled modules, enumerate
  dynamic import globs, or analyze executable inline HTML scripts. Unknown runtime
  values remain unresolved.
- Source map recovery requires embedded content to write original files. Missing
  sources and external indexed-map section URLs remain metadata. Retrieval follows
  only the primary map link; VLQ position lookup is not implemented.
- Formatting is separate from unminification, deobfuscation, and symbol renaming.
