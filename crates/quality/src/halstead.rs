//! Halstead metrics + maintainability index. Mirrors `src/quality/halstead.py`.

use std::collections::HashMap;

use tree_sitter::Node;

const OPERATOR_NODE_TYPES: &[&str] = &[
    "+", "-", "*", "/", "%", "&&", "||", "&", "|", "^", "<<", ">>", ">>>", "==", "!=", "<", ">",
    "<=", ">=", "=", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "<<=", ">>=", "!", "~", "++",
    "--",
];

const OPERATOR_KEYWORDS: &[&str] = &[
    "new",
    "instanceof",
    "return",
    "throw",
    "if",
    "else",
    "for",
    "while",
    "switch",
    "case",
    "break",
    "continue",
    "try",
    "catch",
    "finally",
];

const OPERAND_NODE_TYPES: &[&str] = &[
    "identifier",
    "decimal_integer_literal",
    "hex_integer_literal",
    "octal_integer_literal",
    "binary_integer_literal",
    "decimal_floating_point_literal",
    "string_literal",
    "character_literal",
    "true",
    "false",
    "null_literal",
    "this",
    "super",
];

#[derive(Debug, Clone, Default)]
pub struct HalsteadMetrics {
    pub n1: i64,
    pub n2: i64,
    pub cap_n1: i64,
    pub cap_n2: i64,
    pub vocabulary: i64,
    pub length: i64,
    pub volume: f64,
    pub difficulty: f64,
    pub effort: f64,
    pub estimated_bugs: f64,
}

fn children(node: Node) -> Vec<Node> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect()
}

fn node_text(node: Node, source: &[u8]) -> String {
    let start = node.start_byte();
    let end = node.end_byte().min(source.len());
    String::from_utf8_lossy(&source[start..end]).into_owned()
}

pub fn compute_halstead(node: Node, source: &[u8]) -> HalsteadMetrics {
    let mut operators: HashMap<String, i64> = HashMap::new();
    let mut operands: HashMap<String, i64> = HashMap::new();

    fn walk(
        node: Node,
        source: &[u8],
        operators: &mut HashMap<String, i64>,
        operands: &mut HashMap<String, i64>,
    ) {
        let kind = node.kind();
        let text = node_text(node, source);
        // Tree-sitter exposes symbolic tokens as kind == "&&" etc., and exposes
        // keyword children with kind == "if" etc. The Python reference checks
        // both the node type and the text; we mirror the priority order.
        if OPERATOR_NODE_TYPES.contains(&kind)
            || OPERATOR_NODE_TYPES.contains(&text.as_str())
            || OPERATOR_KEYWORDS.contains(&kind)
            || OPERATOR_KEYWORDS.contains(&text.as_str())
        {
            *operators.entry(text).or_insert(0) += 1;
        } else if OPERAND_NODE_TYPES.contains(&kind) {
            *operands.entry(text).or_insert(0) += 1;
        }
        for c in children(node) {
            walk(c, source, operators, operands);
        }
    }
    walk(node, source, &mut operators, &mut operands);

    let n1 = operators.len() as i64;
    let n2 = operands.len() as i64;
    let cap_n1: i64 = operators.values().sum();
    let cap_n2: i64 = operands.values().sum();
    let vocab = n1 + n2;
    let length = cap_n1 + cap_n2;

    if vocab == 0 {
        return HalsteadMetrics::default();
    }

    let volume = length as f64 * (vocab as f64).log2();
    let difficulty = if n2 > 0 {
        (n1 as f64 / 2.0) * (cap_n2 as f64 / n2 as f64)
    } else {
        0.0
    };
    let effort = volume * difficulty;
    let estimated_bugs = volume / 3000.0;

    HalsteadMetrics {
        n1,
        n2,
        cap_n1,
        cap_n2,
        vocabulary: vocab,
        length,
        volume,
        difficulty,
        effort,
        estimated_bugs,
    }
}

/// Maintainability index, clamped to [0, 100].
///
/// `MI = 171 - 5.2 ln(V) - 0.23 CC - 16.2 ln(LOC) + 50 sin(sqrt(2.4*comment_pct))`
pub fn maintainability_index(halstead_volume: f64, cc: i64, loc: i64, comment_pct: f64) -> f64 {
    if loc <= 0 || halstead_volume <= 0.0 {
        return 100.0;
    }
    let mi = 171.0 - 5.2 * halstead_volume.ln() - 0.23 * cc as f64 - 16.2 * (loc as f64).ln()
        + 50.0 * (2.4 * comment_pct).sqrt().sin();
    mi.clamp(0.0, 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mi_zero_volume_returns_100() {
        assert!((maintainability_index(0.0, 5, 100, 0.0) - 100.0).abs() < 1e-9);
        assert!((maintainability_index(10.0, 5, 0, 0.0) - 100.0).abs() < 1e-9);
    }

    #[test]
    fn mi_is_bounded_0_100() {
        let mi = maintainability_index(1000.0, 100, 1000, 0.0);
        assert!((0.0..=100.0).contains(&mi));
    }
}
