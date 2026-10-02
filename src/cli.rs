use clap::{Args, Parser, Subcommand, ValueEnum};
use jsexec::chunks::{Engine, InputKind, Options, Source};
use jsexec::sourcemaps;
use std::collections::BTreeSet;
use std::error::Error;
use std::fs;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use url::Url;

mod format;
mod query;

#[derive(Parser)]
#[command(version, about = "Extensible offline JavaScript analysis")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Beautify JavaScript, TypeScript, JSX, TSX, or HTML source
    Format(format::FormatArgs),
    /// Discover AST node kinds and their available fields
    Ast(query::AstArgs),
    /// Select AST nodes with CSS-style selectors over arbitrary fields
    Query(query::QueryArgs),
    /// Inspect a node ID or navigate its AST relationships
    Node(query::NodeArgs),
    /// Discover JavaScript chunk references and runtime-generated filenames
    #[command(alias = "extract")]
    Chunks(ChunkArgs),
    /// Discover source maps and recover embedded original source files
    #[command(aliases = ["sourcemap", "source-maps"])]
    Sourcemaps(SourceMapArgs),
    /// List available chunk extractors
    Extractors,
}

#[derive(Args)]
struct ChunkArgs {
    /// Input JS/TS, JSON, or HTML files; use '-' to read stdin
    #[arg(required = true, num_args = 1..)]
    files: Vec<PathBuf>,
    /// Resolve references against this HTTP(S) URL (prefer the original bundle URL)
    #[arg(long)]
    base_url: Option<Url>,
    /// Select extractors by name; repeat or separate names with commas (default: all)
    #[arg(long = "extractor", value_delimiter = ',')]
    extractors: Vec<String>,
    /// Override automatic source-format detection, including stdin
    #[arg(long, value_enum, default_value_t = Format::Auto)]
    input_type: Format,
    /// Print one deduplicated reference/URL per line
    #[arg(long)]
    list: bool,
    /// Save results to this file instead of stdout
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Fail on syntax diagnostics rather than emitting partial results
    #[arg(long)]
    strict: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Auto,
    Js,
    Json,
    Html,
}

#[derive(Args)]
struct SourceMapArgs {
    /// Input JavaScript or saved .map/JSON files; '-' reads stdin
    #[arg(required = true, num_args = 1..)]
    files: Vec<PathBuf>,
    /// Original script URL for JS input, or original map URL for a saved map
    #[arg(long)]
    base_url: Option<Url>,
    /// Override automatic JavaScript/map detection
    #[arg(long, value_enum, default_value_t = SourceMapFormat::Auto)]
    input_type: SourceMapFormat,
    /// Write embedded source files into a new directory (must not already exist)
    #[arg(long)]
    sources_dir: Option<PathBuf>,
    /// Print unique external map references/URLs, including URL sections
    #[arg(long)]
    list: bool,
    /// Save the report to a file instead of stdout
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Fail before writing on syntax or source map diagnostics
    #[arg(long)]
    strict: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum SourceMapFormat {
    Auto,
    Js,
    #[value(alias = "json")]
    Map,
}

pub fn run() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    let engine = Engine::default();
    match cli.command {
        Command::Format(args) => format::run(args)?,
        Command::Ast(args) => query::ast(args)?,
        Command::Query(args) => query::query(args)?,
        Command::Node(args) => query::node(args)?,
        Command::Extractors => {
            let mut output = io::stdout().lock();
            for name in engine.extractor_names() {
                writeln!(output, "{name}")?;
            }
        }
        Command::Chunks(args) => chunks(args, &engine)?,
        Command::Sourcemaps(args) => source_maps(args)?,
    }
    Ok(())
}

fn chunks(args: ChunkArgs, engine: &Engine) -> Result<(), Box<dyn Error>> {
    let options = Options {
        base_url: args.base_url,
        extractors: args.extractors.into_iter().collect::<BTreeSet<_>>(),
        input_kind: match args.input_type {
            Format::Auto => InputKind::Auto,
            Format::Js => InputKind::JavaScript,
            Format::Json => InputKind::Json,
            Format::Html => InputKind::Html,
        },
    };
    // Validate options before reading stdin or creating output files.
    engine.analyze(&[], &options)?;
    protect_output(&args.files, args.output.as_deref())?;
    let sources = read_sources(args.files)?;
    let report = engine.analyze(&sources, &options)?;
    check_strict(args.strict, &report.diagnostics)?;
    let mut result = Vec::new();
    if args.list {
        for diagnostic in &report.diagnostics {
            eprintln!("{}: {}", diagnostic.file, diagnostic.message);
        }
        for chunk in &report.chunks {
            writeln!(result, "{}", chunk.value)?;
        }
    } else {
        serde_json::to_writer_pretty(&mut result, &report)?;
        result.push(b'\n');
    }
    write_output(args.output, result)
}

fn read_sources(files: Vec<PathBuf>) -> Result<Vec<Source>, Box<dyn Error>> {
    let stdin_count = files.iter().filter(|path| path.as_os_str() == "-").count();
    if stdin_count > 1 {
        return Err("stdin ('-') can only be supplied once".into());
    }
    let mut sources = Vec::new();
    for path in files {
        let (name, code) = if path.as_os_str() == "-" {
            let mut code = String::new();
            io::stdin().read_to_string(&mut code)?;
            ("<stdin>".into(), code)
        } else {
            let code = fs::read_to_string(&path)
                .map_err(|error| format!("cannot read '{}': {error}", path.display()))?;
            (path.to_string_lossy().into_owned(), code)
        };
        sources.push(Source { name, code });
    }
    Ok(sources)
}

fn protect_output(
    files: &[PathBuf],
    output: Option<&std::path::Path>,
) -> Result<(), Box<dyn Error>> {
    if let Some(output) = output {
        for input in files {
            if input.as_os_str() != "-"
                && (input == output
                    || fs::canonicalize(input)
                        .ok()
                        .zip(fs::canonicalize(output).ok())
                        .is_some_and(|(input, output)| input == output))
            {
                return Err("output must not overwrite an input file".into());
            }
        }
    }
    Ok(())
}

fn check_strict(
    strict: bool,
    diagnostics: &[jsexec::source::Diagnostic],
) -> Result<(), Box<dyn Error>> {
    if strict && !diagnostics.is_empty() {
        return Err(format!(
            "{} source diagnostic(s); first: {}: {}",
            diagnostics.len(),
            diagnostics[0].file,
            diagnostics[0].message
        )
        .into());
    }
    Ok(())
}

fn source_maps(args: SourceMapArgs) -> Result<(), Box<dyn Error>> {
    let options = sourcemaps::Options {
        base_url: args.base_url,
        input_kind: match args.input_type {
            SourceMapFormat::Auto => sourcemaps::InputKind::Auto,
            SourceMapFormat::Js => sourcemaps::InputKind::JavaScript,
            SourceMapFormat::Map => sourcemaps::InputKind::Map,
        },
    };
    sourcemaps::analyze(&[], &options)?;
    protect_output(&args.files, args.output.as_deref())?;
    if let Some(directory) = &args.sources_dir
        && fs::symlink_metadata(directory).is_ok()
    {
        return Err(format!(
            "sources directory '{}' already exists; choose a new directory",
            directory.display()
        )
        .into());
    }
    let sources = read_sources(args.files)?;
    let mut report = sourcemaps::analyze(&sources, &options)?;
    check_strict(args.strict, &report.diagnostics)?;
    if let Some(directory) = &args.sources_dir {
        if report.embedded_count() == 0 {
            return Err("no embedded sources to write; supply a saved .map file or a script with an inline map".into());
        }
        write_embedded_sources(&mut report, directory)?;
        let extracted: Vec<PathBuf> = report
            .maps
            .iter()
            .flat_map(|map| &map.sources)
            .filter_map(|source| {
                source
                    .extracted_path
                    .as_ref()
                    .map(|path| directory.join(path))
            })
            .collect();
        protect_output(&extracted, args.output.as_deref())?;
    }
    let mut result = Vec::new();
    if args.list {
        for diagnostic in &report.diagnostics {
            eprintln!("{}: {}", diagnostic.file, diagnostic.message);
        }
        let mut references = BTreeSet::new();
        for map in &report.maps {
            if map.kind == sourcemaps::MapKind::External
                && let Some(reference) = map.url.as_ref().or(map.reference.as_ref())
            {
                references.insert(reference);
            }
            for section in &map.external_sections {
                references.insert(section.url.as_ref().unwrap_or(&section.reference));
            }
        }
        for reference in references {
            writeln!(result, "{reference}")?;
        }
    } else {
        serde_json::to_writer_pretty(&mut result, &report)?;
        result.push(b'\n');
    }
    write_output(args.output, result)
}

fn write_output(output: Option<PathBuf>, result: Vec<u8>) -> Result<(), Box<dyn Error>> {
    if let Some(path) = output {
        fs::write(&path, result)?;
    } else {
        io::stdout().lock().write_all(&result)?;
    }
    Ok(())
}

fn write_embedded_sources(
    report: &mut sourcemaps::Report,
    directory: &std::path::Path,
) -> Result<(), Box<dyn Error>> {
    // Exclusive creation prevents extraction through pre-existing symlinks or collisions.
    fs::create_dir(directory)?;
    for (map_index, map) in report.maps.iter_mut().enumerate() {
        for (source_index, source) in map.sources.iter_mut().enumerate() {
            let Some(content) = &source.content else {
                continue;
            };
            let relative = PathBuf::from(format!(
                "map-{:04}/source-{:04}",
                map_index + 1,
                source_index + 1
            ))
            .join(safe_source_path(source.path.as_deref()));
            let destination = directory.join(&relative);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)?;
            file.write_all(content.as_bytes())?;
            source.extracted_path = Some(relative.to_string_lossy().into_owned());
        }
    }
    Ok(())
}

fn safe_source_path(path: Option<&str>) -> PathBuf {
    let path = path
        .unwrap_or("anonymous-source.txt")
        .split(['?', '#'])
        .next()
        .unwrap_or_default()
        .replace('\\', "/");
    let mut result = PathBuf::new();
    for component in path.split('/') {
        if matches!(component, "" | "." | "..") {
            continue;
        }
        let clean: String = component
            .chars()
            .take(80)
            .map(|c| {
                if c.is_alphanumeric() || matches!(c, '.' | '_' | '-') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        if !clean.is_empty() {
            result.push(clean);
        }
    }
    if result.as_os_str().is_empty() {
        result.push("anonymous-source.txt");
    }
    result
}
