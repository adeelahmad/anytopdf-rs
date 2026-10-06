use clap::{Subcommand, builder::TypedValueParser};
use std::path::PathBuf;

/// Annotation kinds the index records, plus `chunk` (a unit's whole searchable text).
pub(crate) const ENTRY_KINDS: [&str; 12] = [
    "chunk",
    "ocr",
    "caption",
    "transcript",
    "metadata",
    "object",
    "face",
    "scene",
    "timestamp",
    "location",
    "barcode",
    "custom",
];

#[derive(Debug, clap::Args)]
pub(crate) struct SearchArgs {
    /// Words to find (all must match); "quoted words" match as a phrase and a
    /// trailing * matches a prefix. Optional when a filter is given.
    pub(crate) query: Vec<String>,
    /// Only results of this kind (repeatable).
    #[arg(long, value_parser = ENTRY_KINDS)]
    pub(crate) kind: Vec<String>,
    /// Only faces recognised as this person.
    #[arg(long)]
    pub(crate) person: Option<String>,
    /// Only PDFs indexed into this collection.
    #[arg(long)]
    pub(crate) collection: Option<String>,
    /// Maximum results.
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=10_000).map(|n| n as usize))]
    pub(crate) limit: usize,
    /// Index database [default: ANYTOPDF_INDEX, else index.sqlite in the user data directory].
    #[arg(long, value_name = "PATH")]
    pub(crate) index_db: Option<PathBuf>,
    /// Emit one JSON document (anytopdf.search/1) on stdout.
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Debug, Subcommand)]
pub(crate) enum IndexCommand {
    /// Index PDFs made by anytopdf from their embedded manifest and chunks.
    Add {
        /// PDFs to index; one already indexed is replaced.
        #[arg(required = true)]
        pdfs: Vec<PathBuf>,
        /// Tag the PDFs with this collection name.
        #[arg(long)]
        collection: Option<String>,
        /// Index database [default: ANYTOPDF_INDEX, else index.sqlite in the user data directory].
        #[arg(long, value_name = "PATH")]
        index_db: Option<PathBuf>,
        /// Emit one JSON document (anytopdf.index/1) on stdout.
        #[arg(long)]
        json: bool,
    },
    /// List the indexed PDFs.
    List {
        /// Only PDFs in this collection.
        #[arg(long)]
        collection: Option<String>,
        /// Index database [default: ANYTOPDF_INDEX, else index.sqlite in the user data directory].
        #[arg(long, value_name = "PATH")]
        index_db: Option<PathBuf>,
        /// Emit one JSON document (anytopdf.index/1) on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Remove PDFs from the index (the files are not touched).
    Remove {
        /// PDFs to forget.
        #[arg(required = true)]
        pdfs: Vec<PathBuf>,
        /// Index database [default: ANYTOPDF_INDEX, else index.sqlite in the user data directory].
        #[arg(long, value_name = "PATH")]
        index_db: Option<PathBuf>,
    },
}
