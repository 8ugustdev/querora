/* SPDX-License-Identifier: Apache-2.0 */
/**
 * Prepare the querora-mcp shim as a tauri externalBin: build it and copy
 * to src-tauri/binaries/querora-mcp-<triple>. tauri places it next to the
 * main app binary in the bundle (Contents/MacOS), where
 * agents::mcp_shim_path() finds it.
 */
import { execSync } from "node:child_process";
import { cpSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..");

const profile = process.argv.includes("--release") ? "release" : "debug"; // debug works on beta toolchains; CI release uses --release
execSync(profile === "release" ? "cargo build --release -p querora-mcp" : "cargo build -p querora-mcp", { cwd: root, stdio: "inherit" });
const triple = execSync("rustc -vV | grep host | cut -d' ' -f2", { cwd: root }).toString().trim();
const dest = join(root, "apps", "desktop", "src-tauri", "binaries");
mkdirSync(dest, { recursive: true });
cpSync(
  join(root, "target", profile, "querora-mcp"),
  join(dest, `querora-mcp-${triple}`),
);
cpSync(
  join(root, "target", profile, "querora-mcp"),
  join(dest, "querora-mcp"),
);
console.log(`shim prepared for ${triple}`);
