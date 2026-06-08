// Node parity harness: load sql.js + engine.js against a snapshot DB passed as
// argv[2], recompute, and print the parity result as JSON. Exit non-zero if
// parity (or the keep unit checks) fail. Driven by tests/engine_parity.rs under
// `--features node-tests`.

import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const asset = (f) => fileURLToPath(new URL('../assets/' + f, import.meta.url));

const snapshotPath = process.argv[2];
if (!snapshotPath) {
  console.error('usage: parity.mjs <snapshot.db>');
  process.exit(2);
}

const sqlMod = require(asset('sql-wasm.js'));
const initSqlJs = sqlMod.default || sqlMod;
const wasmBinary = readFileSync(asset('sql-wasm.wasm'));
const SQL = await initSqlJs({ wasmBinary });

const db = new SQL.Database(new Uint8Array(readFileSync(snapshotPath)));

// Load engine.js (a classic script) into a function scope and grab GradeEngine.
const engineSrc = readFileSync(asset('engine.js'), 'utf8');
// eslint-disable-next-line no-new-func
const GradeEngine = new Function(engineSrc + '\nreturn GradeEngine;')();

// keep unit checks mirror modulation.rs tests.
const approx = (a, b) => Math.abs(a - b) < 1e-9;
const keepOk =
  approx(GradeEngine.keep(1, 1, 1, 0.2), 0.2) &&
  approx(GradeEngine.keep(0, 0, 1, 0.2), 1.0) &&
  approx(GradeEngine.keep(1, 1, 0, 0.2), 1.0);

const knobs = GradeEngine.knobsFromTables(db);
GradeEngine.recompute(db, knobs);
const result = GradeEngine.checkParity(db, knobs, knobs.decimals);
result.keepOk = keepOk;

console.log(JSON.stringify(result));
process.exit(result.ok && keepOk ? 0 : 1);
