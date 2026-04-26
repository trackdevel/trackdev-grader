//! Stage 2 — code survival analysis.
//!
//! Pipeline: parse → normalize → fingerprint → git blame → compute survival.
//! Also provides LAT/LAR/LS line metrics via `diff_lines`.

pub mod blame;
pub mod cross_team;
pub mod diff_lines;
pub mod estimation;
pub mod fingerprint;
pub mod normalizer;
pub mod parser;
pub mod rewrite_detector;
pub mod survival;
pub mod types;

pub use fingerprint::{
    fingerprint_file, FileFingerprints, MethodFingerprint, StatementFingerprint,
};
pub use parser::{parse_file, parse_java_file, parse_xml_file};
pub use types::{Method, ParseResult, Statement, VariableDecl};
