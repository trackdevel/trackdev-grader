//! Shared data structures for survival parsing.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement {
    pub raw_text: String,
    /// 1-based source line of first token
    pub start_line: u32,
    /// 1-based source line of last token
    pub end_line: u32,
    pub statement_type: String,
    pub method_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariableDecl {
    pub name: String,
    pub line: u32,
    pub kind: VarKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarKind {
    Parameter,
    Local,
    ForVar,
    Catch,
    Resource,
    Lambda,
}

impl VarKind {
    pub fn as_str(self) -> &'static str {
        match self {
            VarKind::Parameter => "parameter",
            VarKind::Local => "local",
            VarKind::ForVar => "for_var",
            VarKind::Catch => "catch",
            VarKind::Resource => "resource",
            VarKind::Lambda => "lambda",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Method {
    pub name: String,
    pub start_line: u32,
    pub end_line: u32,
    pub statements: Vec<Statement>,
    pub variables: Vec<VariableDecl>,
}

#[derive(Debug, Clone)]
pub struct ParseResult {
    pub file_path: String,
    pub language: String,
    pub methods: Vec<Method>,
    pub class_level_statements: Vec<Statement>,
}

impl ParseResult {
    pub fn new(file_path: impl Into<String>, language: impl Into<String>) -> Self {
        Self {
            file_path: file_path.into(),
            language: language.into(),
            methods: Vec::new(),
            class_level_statements: Vec::new(),
        }
    }

    pub fn all_statements(&self) -> Vec<&Statement> {
        let mut out: Vec<&Statement> = self.class_level_statements.iter().collect();
        for m in &self.methods {
            out.extend(m.statements.iter());
        }
        out
    }
}
