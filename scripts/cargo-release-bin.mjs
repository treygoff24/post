// Cargo owns target-directory precedence (config, environment, defaults).
// Resolve it rather than guessing where the release build wrote its binary.
import { execFileSync } from "node:child_process";
import path from "node:path";
import { pathToFileURL } from "node:url";

export function cargoReleaseBin(cwd = process.cwd()) {
  const metadata = JSON.parse(execFileSync("cargo", ["metadata", "--no-deps", "--format-version", "1"], {
    cwd, encoding: "utf8", stdio: ["ignore", "pipe", "inherit"],
  }));
  if (typeof metadata.target_directory !== "string" || !path.isAbsolute(metadata.target_directory)) {
    throw new Error("cargo metadata did not return an absolute target_directory");
  }
  return path.join(metadata.target_directory, "release", "post");
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  console.log(cargoReleaseBin());
}
