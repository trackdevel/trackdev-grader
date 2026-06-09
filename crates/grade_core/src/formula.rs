//! Formula AST, evaluator, and explain-tree nodes.

use std::collections::{BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

pub type Scope = HashMap<String, f64>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Expr {
    Num {
        value: f64,
    },
    Var {
        name: String,
    },
    Add {
        terms: Vec<Expr>,
    },
    Sub {
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Mul {
        factors: Vec<Expr>,
    },
    Div {
        num: Box<Expr>,
        den: Box<Expr>,
    },
    Min {
        args: Vec<Expr>,
    },
    Max {
        args: Vec<Expr>,
    },
    Clamp {
        x: Box<Expr>,
        lo: Box<Expr>,
        hi: Box<Expr>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub label: String,
    pub expr: String,
    pub value: f64,
    pub children: Vec<Node>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvalError {
    UnknownVar { name: String },
    DivByZero,
    Domain { message: String },
}

impl std::fmt::Display for EvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownVar { name } => write!(f, "unknown variable: {name}"),
            Self::DivByZero => write!(f, "division by zero"),
            Self::Domain { message } => write!(f, "domain error: {message}"),
        }
    }
}

impl std::error::Error for EvalError {}

pub fn eval(expr: &Expr, scope: &Scope, label: &str, infix: &str) -> Result<Node, EvalError> {
    let (value, children) = eval_inner(expr, scope)?;
    Ok(Node {
        label: label.to_string(),
        expr: infix.to_string(),
        value,
        children,
    })
}

fn eval_inner(expr: &Expr, scope: &Scope) -> Result<(f64, Vec<Node>), EvalError> {
    match expr {
        Expr::Num { value } => Ok((*value, Vec::new())),
        Expr::Var { name } => scope
            .get(name)
            .copied()
            .map(|v| (v, Vec::new()))
            .ok_or_else(|| EvalError::UnknownVar { name: name.clone() }),
        Expr::Add { terms } => {
            let mut sum = 0.0;
            let mut kids = Vec::new();
            for t in terms {
                let (v, c) = eval_inner(t, scope)?;
                sum += v;
                kids.extend(child_nodes(t, v, c));
            }
            Ok((sum, kids))
        }
        Expr::Sub { lhs, rhs } => {
            let (lv, lc) = eval_inner(lhs, scope)?;
            let (rv, rc) = eval_inner(rhs, scope)?;
            let mut kids = child_nodes(lhs, lv, lc);
            kids.extend(child_nodes(rhs, rv, rc));
            Ok((lv - rv, kids))
        }
        Expr::Mul { factors } => {
            let mut prod = 1.0;
            let mut kids = Vec::new();
            for f in factors {
                let (v, c) = eval_inner(f, scope)?;
                prod *= v;
                kids.extend(child_nodes(f, v, c));
            }
            Ok((prod, kids))
        }
        Expr::Div { num, den } => {
            let (nv, nc) = eval_inner(num, scope)?;
            let (dv, dc) = eval_inner(den, scope)?;
            let value = div(nv, dv)?;
            let mut kids = child_nodes(num, nv, nc);
            kids.extend(child_nodes(den, dv, dc));
            Ok((value, kids))
        }
        Expr::Min { args } => eval_nary(args, scope, f64::min),
        Expr::Max { args } => eval_nary(args, scope, f64::max),
        Expr::Clamp { x, lo, hi } => {
            let (xv, xc) = eval_inner(x, scope)?;
            let (lov, loc) = eval_inner(lo, scope)?;
            let (hiv, hic) = eval_inner(hi, scope)?;
            let mut kids = child_nodes(x, xv, xc);
            kids.extend(child_nodes(lo, lov, loc));
            kids.extend(child_nodes(hi, hiv, hic));
            Ok((xv.clamp(lov, hiv), kids))
        }
    }
}

fn eval_nary(
    args: &[Expr],
    scope: &Scope,
    pick: fn(f64, f64) -> f64,
) -> Result<(f64, Vec<Node>), EvalError> {
    if args.is_empty() {
        return Err(EvalError::Domain {
            message: "min/max requires at least one argument".into(),
        });
    }
    let mut best = eval_inner(&args[0], scope)?.0;
    let mut kids = Vec::new();
    for a in args {
        let (v, c) = eval_inner(a, scope)?;
        best = pick(best, v);
        kids.extend(child_nodes(a, v, c));
    }
    Ok((best, kids))
}

fn div(num: f64, den: f64) -> Result<f64, EvalError> {
    if den == 0.0 {
        if num == 0.0 {
            Ok(0.0)
        } else {
            Err(EvalError::DivByZero)
        }
    } else {
        Ok(num / den)
    }
}

fn child_nodes(expr: &Expr, value: f64, children: Vec<Node>) -> Vec<Node> {
    if children.is_empty() {
        vec![leaf_node(expr, value)]
    } else {
        children
    }
}

fn leaf_node(expr: &Expr, value: f64) -> Node {
    match expr {
        Expr::Num { value: v } => Node {
            label: format!("{v}"),
            expr: format!("{v}"),
            value: *v,
            children: vec![],
        },
        Expr::Var { name } => Node {
            label: name.clone(),
            expr: name.clone(),
            value,
            children: vec![],
        },
        _ => Node {
            label: format!("{value}"),
            expr: String::new(),
            value,
            children: vec![],
        },
    }
}

pub fn free_vars(expr: &Expr) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    collect_free_vars(expr, &mut out);
    out
}

fn collect_free_vars(expr: &Expr, out: &mut BTreeSet<String>) {
    match expr {
        Expr::Num { .. } => {}
        Expr::Var { name } => {
            out.insert(name.clone());
        }
        Expr::Add { terms } => {
            for t in terms {
                collect_free_vars(t, out);
            }
        }
        Expr::Min { args } | Expr::Max { args } => {
            for a in args {
                collect_free_vars(a, out);
            }
        }
        Expr::Sub { lhs, rhs } => {
            collect_free_vars(lhs, out);
            collect_free_vars(rhs, out);
        }
        Expr::Mul { factors } => {
            for f in factors {
                collect_free_vars(f, out);
            }
        }
        Expr::Div { num, den } => {
            collect_free_vars(num, out);
            collect_free_vars(den, out);
        }
        Expr::Clamp { x, lo, hi } => {
            collect_free_vars(x, out);
            collect_free_vars(lo, out);
            collect_free_vars(hi, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(pairs: &[(&str, f64)]) -> Scope {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn div_zero_over_zero_is_zero() {
        let e = Expr::Div {
            num: Box::new(Expr::Num { value: 0.0 }),
            den: Box::new(Expr::Num { value: 0.0 }),
        };
        let n = eval(&e, &scope(&[]), "t", "0/0").unwrap();
        assert!((n.value - 0.0).abs() < 1e-12);
    }

    #[test]
    fn div_nonzero_over_zero_errors() {
        let e = Expr::Div {
            num: Box::new(Expr::Num { value: 1.0 }),
            den: Box::new(Expr::Num { value: 0.0 }),
        };
        assert!(matches!(
            eval(&e, &scope(&[]), "t", "1/0"),
            Err(EvalError::DivByZero)
        ));
    }

    #[test]
    fn unknown_var_errors() {
        let e = Expr::Var {
            name: "missing".into(),
        };
        assert!(matches!(
            eval(&e, &scope(&[]), "t", "missing"),
            Err(EvalError::UnknownVar { .. })
        ));
    }

    #[test]
    fn clamp_bounds() {
        let e = Expr::Clamp {
            x: Box::new(Expr::Var { name: "x".into() }),
            lo: Box::new(Expr::Num { value: 0.0 }),
            hi: Box::new(Expr::Num { value: 10.0 }),
        };
        let hi = eval(&e, &scope(&[("x", 15.0)]), "c", "clamp").unwrap();
        assert!((hi.value - 10.0).abs() < 1e-12);
        let lo = eval(&e, &scope(&[("x", -3.0)]), "c", "clamp").unwrap();
        assert!((lo.value - 0.0).abs() < 1e-12);
    }

    #[test]
    fn keep_formula_cap_a() {
        let keep_expr = Expr::Sub {
            lhs: Box::new(Expr::Num { value: 1.0 }),
            rhs: Box::new(Expr::Mul {
                factors: vec![
                    Expr::Sub {
                        lhs: Box::new(Expr::Num { value: 1.0 }),
                        rhs: Box::new(Expr::Var {
                            name: "floor_keep".into(),
                        }),
                    },
                    Expr::Var {
                        name: "ai_strength".into(),
                    },
                    Expr::Var {
                        name: "model_m".into(),
                    },
                    Expr::Var {
                        name: "level_l".into(),
                    },
                ],
            }),
        };
        let s = scope(&[
            ("floor_keep", 0.2),
            ("ai_strength", 1.0),
            ("model_m", 0.0),
            ("level_l", 0.0),
        ]);
        let n = eval(&keep_expr, &s, "keep", "keep").unwrap();
        assert!((n.value - 1.0).abs() < 1e-9);
    }

    #[test]
    fn free_vars_collects_names() {
        let e = Expr::Add {
            terms: vec![
                Expr::Var { name: "a".into() },
                Expr::Mul {
                    factors: vec![
                        Expr::Var { name: "b".into() },
                        Expr::Var { name: "a".into() },
                    ],
                },
            ],
        };
        assert_eq!(
            free_vars(&e),
            BTreeSet::from(["a".to_string(), "b".to_string()])
        );
    }
}
