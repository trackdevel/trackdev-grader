/**
 * Infix formula text ⇄ `grade_core` Expr AST.
 *
 * The WASM engine evaluates the `expr` AST; the `infix` string on each
 * formula is display-only. This parser lets the UI accept edits as plain
 * infix text and regenerate a matching AST, keeping the two in sync.
 *
 * Grammar (matches `grade_core::formula::Expr` exactly):
 *   expr   := term (('+' | '-') term)*
 *   term   := unary (('*' | '/') unary)*
 *   unary  := '-' unary | atom
 *   atom   := number | ident | ident '(' expr (',' expr)* ')' | '(' expr ')'
 *   funcs  := min(...), max(...) with ≥2 args; clamp(x, lo, hi)
 */

export type Expr =
  | { op: "num"; value: number }
  | { op: "var"; name: string }
  | { op: "add"; terms: Expr[] }
  | { op: "sub"; lhs: Expr; rhs: Expr }
  | { op: "mul"; factors: Expr[] }
  | { op: "div"; num: Expr; den: Expr }
  | { op: "min"; args: Expr[] }
  | { op: "max"; args: Expr[] }
  | { op: "clamp"; x: Expr; lo: Expr; hi: Expr };

type Token =
  | { kind: "num"; value: number; pos: number }
  | { kind: "ident"; name: string; pos: number }
  | { kind: "punct"; ch: string; pos: number };

const IDENT_START = /[A-Za-z_]/;
const IDENT_CONT = /[A-Za-z0-9_]/;
const NUM_RE = /^\d+(\.\d+)?([eE][+-]?\d+)?/;

function tokenize(src: string): Token[] {
  const out: Token[] = [];
  let i = 0;
  while (i < src.length) {
    const ch = src[i];
    if (ch === " " || ch === "\t" || ch === "\n" || ch === "\r") {
      i += 1;
      continue;
    }
    if (IDENT_START.test(ch)) {
      let j = i + 1;
      while (j < src.length && IDENT_CONT.test(src[j])) j += 1;
      out.push({ kind: "ident", name: src.slice(i, j), pos: i });
      i = j;
      continue;
    }
    const numMatch = NUM_RE.exec(src.slice(i));
    if (numMatch) {
      out.push({ kind: "num", value: Number(numMatch[0]), pos: i });
      i += numMatch[0].length;
      continue;
    }
    if ("+-*/(),".includes(ch)) {
      out.push({ kind: "punct", ch, pos: i });
      i += 1;
      continue;
    }
    throw new Error(`Unexpected character '${ch}' at position ${i}`);
  }
  return out;
}

/** Parse infix formula text into an Expr AST. Throws Error on bad input. */
export function parseInfix(src: string): Expr {
  const tokens = tokenize(src);
  let pos = 0;

  const peek = (): Token | undefined => tokens[pos];
  const isPunct = (ch: string): boolean => {
    const t = tokens[pos];
    return t?.kind === "punct" && t.ch === ch;
  };
  const expectPunct = (ch: string): void => {
    if (!isPunct(ch)) {
      const t = tokens[pos];
      throw new Error(
        t
          ? `Expected '${ch}' at position ${t.pos}`
          : `Expected '${ch}' but formula ended`,
      );
    }
    pos += 1;
  };

  function parseExpr(): Expr {
    let node = parseTerm();
    for (;;) {
      if (isPunct("+")) {
        pos += 1;
        const rhs = parseTerm();
        node =
          node.op === "add"
            ? { op: "add", terms: [...node.terms, rhs] }
            : { op: "add", terms: [node, rhs] };
      } else if (isPunct("-")) {
        pos += 1;
        node = { op: "sub", lhs: node, rhs: parseTerm() };
      } else {
        return node;
      }
    }
  }

  function parseTerm(): Expr {
    let node = parseUnary();
    for (;;) {
      if (isPunct("*")) {
        pos += 1;
        const rhs = parseUnary();
        node =
          node.op === "mul"
            ? { op: "mul", factors: [...node.factors, rhs] }
            : { op: "mul", factors: [node, rhs] };
      } else if (isPunct("/")) {
        pos += 1;
        node = { op: "div", num: node, den: parseUnary() };
      } else {
        return node;
      }
    }
  }

  function parseUnary(): Expr {
    if (isPunct("-")) {
      pos += 1;
      const inner = parseUnary();
      if (inner.op === "num") return { op: "num", value: -inner.value };
      return { op: "sub", lhs: { op: "num", value: 0 }, rhs: inner };
    }
    if (isPunct("+")) {
      pos += 1;
      return parseUnary();
    }
    return parseAtom();
  }

  function parseAtom(): Expr {
    const t = peek();
    if (!t) throw new Error("Unexpected end of formula");
    if (t.kind === "num") {
      pos += 1;
      return { op: "num", value: t.value };
    }
    if (t.kind === "ident") {
      pos += 1;
      if (!isPunct("(")) return { op: "var", name: t.name };
      pos += 1; // consume '('
      const args: Expr[] = [parseExpr()];
      while (isPunct(",")) {
        pos += 1;
        args.push(parseExpr());
      }
      expectPunct(")");
      return makeCall(t.name, args, t.pos);
    }
    if (t.kind === "punct" && t.ch === "(") {
      pos += 1;
      const inner = parseExpr();
      expectPunct(")");
      return inner;
    }
    throw new Error(`Unexpected '${t.ch}' at position ${t.pos}`);
  }

  const root = parseExpr();
  const trailing = peek();
  if (trailing) {
    const what = trailing.kind === "punct" ? `'${trailing.ch}'` : "token";
    throw new Error(`Unexpected ${what} at position ${trailing.pos}`);
  }
  return root;
}

function makeCall(name: string, args: Expr[], at: number): Expr {
  switch (name) {
    case "min":
    case "max":
      if (args.length < 2) {
        throw new Error(`${name}() needs at least 2 arguments (position ${at})`);
      }
      return { op: name, args };
    case "clamp":
      if (args.length !== 3) {
        throw new Error(`clamp() needs exactly 3 arguments: x, lo, hi (position ${at})`);
      }
      return { op: "clamp", x: args[0], lo: args[1], hi: args[2] };
    default:
      throw new Error(
        `Unknown function '${name}' (position ${at}); available: min, max, clamp`,
      );
  }
}

/** Precedence: additive=1, multiplicative=2, atoms=3. */
function precOf(e: Expr): number {
  switch (e.op) {
    case "add":
    case "sub":
      return 1;
    case "mul":
    case "div":
      return 2;
    default:
      return 3;
  }
}

/** Render an Expr AST back to canonical infix text. */
export function exprToInfix(e: Expr): string {
  return print(e, 0);
}

function print(e: Expr, parentPrec: number): string {
  const prec = precOf(e);
  const wrap = (s: string): string => (prec < parentPrec ? `(${s})` : s);
  switch (e.op) {
    case "num":
      return e.value < 0 && parentPrec > 1 ? `(${e.value})` : String(e.value);
    case "var":
      return e.name;
    case "add":
      return wrap(e.terms.map((t) => print(t, 1)).join(" + "));
    case "sub":
      return wrap(`${print(e.lhs, 1)} - ${print(e.rhs, 2)}`);
    case "mul":
      return wrap(e.factors.map((f) => print(f, 2)).join(" * "));
    case "div":
      return wrap(`${print(e.num, 2)} / ${print(e.den, 3)}`);
    case "min":
    case "max":
      return `${e.op}(${e.args.map((a) => print(a, 0)).join(", ")})`;
    case "clamp":
      return `clamp(${print(e.x, 0)}, ${print(e.lo, 0)}, ${print(e.hi, 0)})`;
  }
}
