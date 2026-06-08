# Vendored browser assets

These files are committed verbatim and embedded (base64) into the emitted
`grading.html` so the page runs from `file://` with **zero network**. Do not
hand-edit them. To refresh, re-pull from the pinned URLs below and update the
SHA-256 sums.

| File | Library | Version | Source URL | SHA-256 |
|---|---|---|---|---|
| `sql-wasm.js` | sql.js | 1.12.0 (pinned) | `https://unpkg.com/sql.js@1.12.0/dist/sql-wasm.js` | `43a1f4a4e43a3869ae290c55e67ffe166652bdc5aba5561e0e04eed7bf9651ee` |
| `sql-wasm.wasm` | sql.js | 1.12.0 (pinned) | `https://unpkg.com/sql.js@1.12.0/dist/sql-wasm.wasm` | `083460b3e9d428ebbbbaa03918ba55da33d810e0fb3470d4b5d8677b462b2c2b` |
| `mathjs.min.js` | math.js | 14.9.1 (resolved from `@14`) | `https://unpkg.com/mathjs@14/lib/browser/math.js` | `eb2977cb81d52fcbbf7892f805b9821583faa9ac0ccfd7d5a2e782ffe5dc9493` |

## Licenses

- **sql.js** — MIT. WebAssembly SQLite compiled by the sql.js project.
  Initialized in the page via `initSqlJs({ wasmBinary })` (decoded base64 →
  `Uint8Array`) so no `locateFile`/`fetch` of the `.wasm` is attempted.
- **math.js** — Apache-2.0. Used only by the non-authoritative "formula box"
  for ad-hoc what-if expressions. The bundled `lib/browser/math.js` UMD build
  carries its own `math.js.LICENSE.txt` header.

## Verify

```sh
cd crates/grading_html/assets
sha256sum -c <<'EOF'
43a1f4a4e43a3869ae290c55e67ffe166652bdc5aba5561e0e04eed7bf9651ee  sql-wasm.js
083460b3e9d428ebbbbaa03918ba55da33d810e0fb3470d4b5d8677b462b2c2b  sql-wasm.wasm
eb2977cb81d52fcbbf7892f805b9821583faa9ac0ccfd7d5a2e782ffe5dc9493  mathjs.min.js
EOF
```
