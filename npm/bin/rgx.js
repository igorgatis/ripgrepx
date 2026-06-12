#!/usr/bin/env node
// Launcher: exec the native `rgx` binary that postinstall placed next to this file.
"use strict";

const { spawnSync } = require("child_process");
const path = require("path");
const fs = require("fs");

const bin = path.join(__dirname, process.platform === "win32" ? "rgx.exe" : "rgx");
if (!fs.existsSync(bin)) {
  console.error(
    "ripgrepx: native binary missing — reinstall, and make sure npm install scripts are enabled " +
      "(not --ignore-scripts)."
  );
  process.exit(1);
}

const r = spawnSync(bin, process.argv.slice(2), { stdio: "inherit" });
if (r.error) {
  console.error(`ripgrepx: ${r.error.message}`);
  process.exit(1);
}
process.exit(r.status === null ? 1 : r.status);
