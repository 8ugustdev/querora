/* SPDX-License-Identifier: Apache-2.0 */
/**
 * Simple no-bundler build: strip TS types to dist/ using the runtime's
 * type stripping (node >= 22.6 --experimental-strip-types; >=23 native).
 * Kept as plain copy so the sidecar stays dependency-light.
 */
import { cpSync, mkdirSync } from "node:fs";
mkdirSync("dist", { recursive: true });
cpSync("src", "dist", { recursive: true });
console.log("sidecar built → dist/ (type-stripped at runtime)");
