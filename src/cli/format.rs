use super::{protect_output, read_sources, write_output};
use clap::{Args, ValueEnum};
use jsexec::format::{self, InputKind, Options};
use std::error::Error;
use std::path::PathBuf;

#[derive(Args)]
pub(super) struct FormatArgs {
    /// Input JS/TS/JSX/TSX or HTML file; '-' reads stdin (defaults to JavaScript)
    file: PathBuf,
    /// Override filename-based format detection
    #[arg(long, value_enum, default_value_t = Format::Auto)]
    input_type: Format,
    /// Replace the input file after successful formatting
    #[arg(short, long, conflicts_with_all = ["output", "check"])]
    write: bool,
    /// Fail if input needs formatting, without emitting or writing code
    #[arg(long, conflicts_with = "output")]
    check: bool,
    /// Save formatted source to a separate file instead of stdout
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Prefer single-quoted JavaScript strings
    #[arg(long)]
    single_quote: bool,
    /// Spaces per indentation level
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u8).range(1..=16))]
    indent_width: u8,
}
#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Auto,
    Js,
    Jsx,
    Ts,
    Tsx,
    Html,
}

pub(super) fn run(args: FormatArgs) -> Result<(), Box<dyn Error>> {
    if args.write && args.file.as_os_str() == "-" {
        return Err("--write requires an input file; use --output to save stdin".into());
    }
    protect_output(std::slice::from_ref(&args.file), args.output.as_deref())?;
    let sources = read_sources(vec![args.file.clone()])?;
    let source = &sources[0];
    let options = Options {
        input_kind: match args.input_type {
            Format::Auto => InputKind::Auto,
            Format::Js => InputKind::JavaScript,
            Format::Jsx => InputKind::Jsx,
            Format::Ts => InputKind::TypeScript,
            Format::Tsx => InputKind::Tsx,
            Format::Html => InputKind::Html,
        },
        single_quote: args.single_quote,
        indent_width: args.indent_width.into(),
    };
    let formatted = format::format(source, &options)?;
    if args.check {
        if formatted != source.code {
            return Err(format!("'{}' needs formatting", source.name).into());
        }
        return Ok(());
    }
    if args.write {
        if formatted != source.code {
            std::fs::write(&args.file, formatted)?;
        }
        Ok(())
    } else {
        write_output(args.output, formatted.into_bytes())
    }
}
