use super::{check_strict, protect_output, read_sources, write_output};
use clap::{Args, ValueEnum};
use jsexec::query::{Page, QueryIndex, Relation, RenderOptions, Selector};
use serde::Serialize;
use std::error::Error;
use std::path::PathBuf;

#[derive(Args)]
pub(super) struct AstArgs {
    /// Input JS/TS/JSX/TSX files; '-' reads stdin (parsed as TSX)
    #[arg(required = true, num_args = 1..)]
    files: Vec<PathBuf>,
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Fail before writing on syntax diagnostics
    #[arg(long)]
    strict: bool,
}

#[derive(Args)]
pub(super) struct QueryArgs {
    /// Selector, e.g. 'CallExpression[callee.name="fetch"]'
    selector: String,
    /// Input JS/TS/JSX/TSX files; '-' reads stdin (parsed as TSX)
    #[arg(required = true, num_args = 1..)]
    files: Vec<PathBuf>,
    #[command(flatten)]
    page: PageArgs,
}

#[derive(Args)]
pub(super) struct NodeArgs {
    /// Input file; '-' reads stdin
    file: PathBuf,
    /// File-local ID returned by ast/query/node, e.g. n42
    id: String,
    #[arg(long, value_enum, default_value_t = Navigation::SelfNode)]
    relation: Navigation,
    #[command(flatten)]
    page: PageArgs,
}

#[derive(Args)]
struct PageArgs {
    /// Maximum nodes per file; 0 returns counts only
    #[arg(long, default_value_t = 20)]
    limit: usize,
    #[arg(long, default_value_t = 0)]
    offset: usize,
    /// Maximum Unicode characters in each code excerpt
    #[arg(long, default_value_t = 500)]
    max_code: usize,
    /// Maximum Unicode characters in each string field
    #[arg(long, default_value_t = 500)]
    max_value: usize,
    /// Maximum entries in each array/object field
    #[arg(long, default_value_t = 20)]
    max_items: usize,
    /// Project dotted fields (repeat or comma-separate); default: all immediate fields
    #[arg(long, value_delimiter = ',')]
    fields: Vec<String>,
    /// Require this SHA-256 source hash to avoid inspecting stale node IDs
    #[arg(long)]
    expect_hash: Option<String>,
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Fail before writing on syntax diagnostics
    #[arg(long)]
    strict: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum Navigation {
    #[value(name = "self")]
    SelfNode,
    Parent,
    Children,
    Descendants,
    Ancestors,
    Siblings,
    /// Nearest enclosing function, class, static block, or program (including self)
    Context,
}
impl From<Navigation> for Relation {
    fn from(value: Navigation) -> Self {
        match value {
            Navigation::SelfNode => Self::SelfNode,
            Navigation::Parent => Self::Parent,
            Navigation::Children => Self::Children,
            Navigation::Descendants => Self::Descendants,
            Navigation::Ancestors => Self::Ancestors,
            Navigation::Siblings => Self::Siblings,
            Navigation::Context => Self::Context,
        }
    }
}

#[derive(Serialize)]
struct NodeReport<'a> {
    file: &'a str,
    hash: &'a str,
    partial: bool,
    diagnostics: &'a [jsexec::source::Diagnostic],
    #[serde(flatten)]
    page: Page,
}

pub(super) fn ast(args: AstArgs) -> Result<(), Box<dyn Error>> {
    protect_output(&args.files, args.output.as_deref())?;
    let sources = read_sources(args.files)?;
    let mut reports = Vec::new();
    for source in &sources {
        let index = QueryIndex::parse(source)?;
        check_strict(args.strict, &index.diagnostics)?;
        reports.push(serde_json::json!({"file": source.name, "hash": index.hash,
            "partial": !index.diagnostics.is_empty(), "diagnostics": index.diagnostics,
            "node_count": index.node_count(), "root": index.root_id(), "kinds": index.kinds()}));
    }
    output(args.output, &reports)
}

pub(super) fn query(args: QueryArgs) -> Result<(), Box<dyn Error>> {
    let selector = Selector::parse(&args.selector)?;
    execute(args.files, args.page, |index, args, render| {
        index.select(&selector, args.offset, args.limit, render)
    })
}

pub(super) fn node(args: NodeArgs) -> Result<(), Box<dyn Error>> {
    execute(vec![args.file], args.page, |index, page, render| {
        index.related(
            &args.id,
            args.relation.into(),
            page.offset,
            page.limit,
            render,
        )
    })
}

fn execute(
    files: Vec<PathBuf>,
    args: PageArgs,
    action: impl Fn(&QueryIndex<'_>, &PageArgs, &RenderOptions) -> Result<Page, jsexec::source::Error>,
) -> Result<(), Box<dyn Error>> {
    protect_output(&files, args.output.as_deref())?;
    let sources = read_sources(files)?;
    let render = RenderOptions {
        max_code: args.max_code,
        max_value: args.max_value,
        max_items: args.max_items,
        fields: args.fields.clone(),
    };
    let mut reports = Vec::new();
    for source in &sources {
        let index = QueryIndex::parse(source)?;
        check_strict(args.strict, &index.diagnostics)?;
        if let Some(hash) = &args.expect_hash
            && *hash != index.hash
        {
            return Err(format!(
                "source hash mismatch for '{}'; query the current file to obtain new IDs",
                source.name
            )
            .into());
        }
        let report = NodeReport {
            file: &source.name,
            hash: &index.hash,
            partial: !index.diagnostics.is_empty(),
            diagnostics: &index.diagnostics,
            page: action(&index, &args, &render)?,
        };
        reports.push(serde_json::to_value(report)?);
    }
    output(args.output, &reports)
}

fn output(path: Option<PathBuf>, reports: &impl Serialize) -> Result<(), Box<dyn Error>> {
    let mut bytes = serde_json::to_vec_pretty(reports)?;
    bytes.push(b'\n');
    write_output(path, bytes)
}
