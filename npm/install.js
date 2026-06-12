#!/usr/bin/env node
// postinstall: download the prebuilt `rgx` binary for this platform from the matching GitHub release.
"use strict";

const fs = require("fs");
const os = require("os");
const path = require("path");
const https = require("https");
const { execFileSync } = require("child_process");

const { version } = require("./package.json");
const REPO = "igorgatis/ripgrepx";

// node platform+arch -> release target triple. Linux x64 uses the static musl build so it runs on any
// distro regardless of glibc version; arm64 linux uses gnu (built on a recent glibc).
const TARGETS = {
  "darwin arm64": "aarch64-apple-darwin",
  "darwin x64": "x86_64-apple-darwin",
  "linux x64": "x86_64-unknown-linux-musl",
  "linux arm64": "aarch64-unknown-linux-gnu",
  "win32 x64": "x86_64-pc-windows-msvc",
  "win32 arm64": "aarch64-pc-windows-msvc",
};

function pick() {
  const key = `${process.platform} ${process.arch}`;
  const target = TARGETS[key];
  if (!target) {
    console.error(
      `ripgrepx: no prebuilt binary for ${key}. Build from source: https://github.com/${REPO}`
    );
    process.exit(1);
  }
  const win = process.platform === "win32";
  return { target, ext: win ? "zip" : "tar.gz", bin: win ? "rgx.exe" : "rgx" };
}

function download(url, dest, redirects = 0) {
  return new Promise((resolve, reject) => {
    https
      .get(url, { headers: { "User-Agent": "ripgrepx-installer" } }, (res) => {
        if ([301, 302, 307, 308].includes(res.statusCode) && res.headers.location) {
          res.resume();
          if (redirects > 5) return reject(new Error("too many redirects"));
          return resolve(download(res.headers.location, dest, redirects + 1));
        }
        if (res.statusCode !== 200) {
          res.resume();
          return reject(new Error(`HTTP ${res.statusCode} for ${url}`));
        }
        const f = fs.createWriteStream(dest);
        res.pipe(f);
        f.on("finish", () => f.close(() => resolve()));
        f.on("error", reject);
      })
      .on("error", reject);
  });
}

async function main() {
  const { target, ext, bin } = pick();
  const tag = `v${version}`;
  const asset = `rgx-${tag}-${target}.${ext}`;
  const url = `https://github.com/${REPO}/releases/download/${tag}/${asset}`;
  const binDir = path.join(__dirname, "bin");
  fs.mkdirSync(binDir, { recursive: true });
  const tmp = path.join(os.tmpdir(), `${asset}.${process.pid}`);

  console.error(`ripgrepx: downloading ${asset}`);
  await download(url, tmp);
  // The system tar handles both .tar.gz and .zip (bsdtar ships on macOS and Windows 10+, GNU tar on Linux).
  execFileSync("tar", ["-xf", tmp, "-C", binDir], { stdio: "inherit" });
  fs.rmSync(tmp, { force: true });

  const out = path.join(binDir, bin);
  if (!fs.existsSync(out)) throw new Error(`archive did not contain ${bin}`);
  if (process.platform !== "win32") fs.chmodSync(out, 0o755);
  console.error(`ripgrepx: installed ${bin}`);
}

main().catch((e) => {
  console.error(`ripgrepx: install failed: ${e.message}`);
  process.exit(1);
});
