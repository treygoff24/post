// Start large suites first so their process tests do not leave a long tail.
// run({files}) preserves this order; the Node CLI sorts filenames again.
import { statSync } from "node:fs";
import { run } from "node:test";
import { tap } from "node:test/reporters";

const files = process.argv.slice(2);
if (!files.length) throw new Error("test-node needs test files");
files.sort((a, b) => statSync(b).size - statSync(a).size || a.localeCompare(b));
run({ files, concurrency: Number(process.env.POST_NODE_TEST_JOBS || 4) })
  .on("test:fail", () => { process.exitCode = 1; })
  .on("error", (error) => { console.error(error); process.exitCode = 1; })
  .compose(tap)
  .pipe(process.stdout);
