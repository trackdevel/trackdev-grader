//! Reporting: per-team Excel + cross-team summary + Markdown with inline SVG
//! charts. Mirrors `src/report/generate.py` (Excel sheets) and replaces
//! `src/report/word_report.py` (dropped `.docx` → Markdown per user
//! preference; the original plan called for HTML but the user requested
//! Markdown since it renders on GitHub/GitLab without a browser).

pub mod charts;
mod flag_details;
pub mod markdown;
pub mod url;
pub mod xlsx;

pub use markdown::{
    generate_markdown_report, generate_markdown_report_ex, generate_markdown_report_multi,
    generate_markdown_report_multi_to_path, generate_markdown_report_multi_to_path_ex,
    generate_markdown_report_multi_to_path_with_opts, generate_markdown_report_to_path_ex,
    generate_markdown_report_to_path_ex2, MultiReportOptions,
};
pub use xlsx::{generate_reports, generate_summary_report, generate_team_report};
