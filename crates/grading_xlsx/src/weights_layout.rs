//! Fixed `Weights` worksheet cell map (must stay in sync with `workbook.rs`).

/// First data row for project weights (`w_doc`); Excel row 2.
pub const SCALAR_START_ROW: u32 = 1;
/// Row index for `k_crit` (Excel row 24).
pub const K_CRIT_ROW: u32 = 23;
/// First model-enum row (Excel row 33).
pub const MODEL_TABLE_START: u32 = 32;
/// First level-enum row (Excel row 49).
pub const LEVEL_TABLE_START: u32 = 48;

pub const VALUE_COL: u32 = 1;
pub const LABEL_COL: u32 = 0;
