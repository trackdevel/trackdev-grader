//! Curriculum knowledge base: LaTeX slide parsing + template diffing.
//! Mirrors `src/curriculum/` in the Python reference.

pub mod latex_parser;
pub mod snapshot;
pub mod template_diff;

pub use latex_parser::{
    build_curriculum_db, get_allowed_concepts, parse_all_slides, parse_tex_file, CurriculumConcept,
};
pub use snapshot::{
    freeze_curriculum_for_sprint, get_allowed_concepts_with_snapshot, snapshot_exists,
};
pub use template_diff::{compute_template_diff, FileDiff};
