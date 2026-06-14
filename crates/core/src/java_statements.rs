//! Shared tree-sitter Java statement node kinds (inventory + survival).

/// Executable / structural statement nodes inside method bodies.
///
/// Kept in sync with `survival::parser` and `project_inventory` scanners.
pub const JAVA_STATEMENT_KINDS: &[&str] = &[
    "expression_statement",
    "local_variable_declaration",
    "return_statement",
    "if_statement",
    "for_statement",
    "enhanced_for_statement",
    "while_statement",
    "do_statement",
    "switch_expression",
    "switch_statement",
    "throw_statement",
    "try_statement",
    "try_with_resources_statement",
    "assert_statement",
    "break_statement",
    "continue_statement",
    "yield_statement",
    "synchronized_statement",
];

/// Whether `kind` is a countable Java statement AST node.
pub fn is_java_statement_kind(kind: &str) -> bool {
    JAVA_STATEMENT_KINDS.contains(&kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_variable_declaration_is_statement() {
        assert!(is_java_statement_kind("local_variable_declaration"));
        assert!(!is_java_statement_kind("class_declaration"));
    }
}
