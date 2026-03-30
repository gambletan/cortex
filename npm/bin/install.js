#!/usr/bin/env node
"use strict";

const https = require("https");
const fs = require("fs");
const path = require("path");
const os = require("os");
const { execSync } = require("child_process");

const REPO = "gambletan/cortex";
const BINARY_NAME = "cortex-mcp-server";
const BIN_DIR = path.join(__dirname);
const BINARY_PATH = path.join(BIN_DIR, BINARY_NAME);
const PKG_VERSION = require("../package.json").version;

function getPlatformSuffix() {
  const platform = os.platform();
  const arch = os.arch();

  const platformMap = {
    darwin: "darwin",
    linux: "linux",
  };

  const archMap = {
    arm64: "arm64",
    x64: "x86_64",
  };

  const osSuffix = platformMap[platform];
  const archSuffix = archMap[arch];

  if (!osSuffix) {
    throw new Error(
      `Unsupported platform: ${platform}. Only darwin and linux are supported.`
    );
  }
  if (!archSuffix) {
    throw new Error(
      `Unsupported architecture: ${arch}. Only arm64 and x64 are supported.`
    );
  }

  return `${osSuffix}-${archSuffix}`;
}

const TRUSTED_HOSTS = ["api.github.com", "github.com", "objects.githubusercontent.com"];
const MAX_REDIRECTS = 5;

function httpsGet(url, redirectCount = 0) {
  return new Promise((resolve, reject) => {
    if (redirectCount > MAX_REDIRECTS) {
      reject(new Error(`Too many redirects (max ${MAX_REDIRECTS})`));
      return;
    }
    const request = https.get(url, { headers: { "User-Agent": "cortex-memory-npm" } }, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        const redirectUrl = new URL(res.headers.location);
        if (!TRUSTED_HOSTS.includes(redirectUrl.hostname)) {
          reject(new Error(`Untrusted redirect host: ${redirectUrl.hostname}`));
          return;
        }
        httpsGet(res.headers.location, redirectCount + 1).then(resolve, reject);
        return;
      }
      if (res.statusCode !== 200) {
        reject(new Error(`HTTP ${res.statusCode} fetching ${url}`));
        return;
      }
      const chunks = [];
      res.on("data", (chunk) => chunks.push(chunk));
      res.on("end", () => resolve(Buffer.concat(chunks)));
      res.on("error", reject);
    });
    request.on("error", reject);
    request.setTimeout(60000, () => {
      request.destroy();
      reject(new Error(`Request timed out: ${url}`));
    });
  });
}

async function fetchRelease() {
  // Pin to the version in package.json for reproducible installs
  const tag = `v${PKG_VERSION}`;
  const url = `https://api.github.com/repos/${REPO}/releases/tags/${tag}`;
  const data = await httpsGet(url);
  try {
    return JSON.parse(data.toString());
  } catch {
    throw new Error(
      `Failed to parse GitHub API response for v${PKG_VERSION}. ` +
      `This may be due to rate limiting or a missing release tag. ` +
      `Try again in a few minutes, or download manually from: ` +
      `https://github.com/${REPO}/releases/tag/v${PKG_VERSION}`
    );
  }
}

async function install() {
  const suffix = getPlatformSuffix();
  const assetName = `${BINARY_NAME}-${suffix}.tar.gz`;

  console.log(`[cortex-memory] Detected platform: ${suffix}`);
  console.log(`[cortex-memory] Fetching latest release from ${REPO}...`);

  const release = await fetchRelease();
  const asset = release.assets.find((a) => a.name === assetName);

  if (!asset) {
    // Try lite variant as fallback (linux-arm64 only has lite)
    const liteAssetName = `${BINARY_NAME}-lite-${suffix}.tar.gz`;
    const liteAsset = release.assets.find((a) => a.name === liteAssetName);

    if (!liteAsset) {
      const available = release.assets.map((a) => a.name).join(", ");
      throw new Error(
        `No binary found for ${suffix}.\n` +
        `Looked for: ${assetName} or ${liteAssetName}\n` +
        `Available: ${available}\n` +
        `Release: ${release.tag_name}`
      );
    }

    console.log(`[cortex-memory] Full binary not available for ${suffix}, using lite variant.`);
    console.log(`[cortex-memory] Downloading ${liteAssetName} (${release.tag_name})...`);
    await downloadAndExtract(liteAsset.browser_download_url);
    return;
  }

  console.log(`[cortex-memory] Downloading ${assetName} (${release.tag_name})...`);
  await downloadAndExtract(asset.browser_download_url);
}

async function downloadAndExtract(downloadUrl) {
  const tarball = await httpsGet(downloadUrl);

  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "cortex-"));
  const tarPath = path.join(tmpDir, "binary.tar.gz");

  try {
    fs.writeFileSync(tarPath, tarball);
    execSync(`tar xzf "${tarPath}" -C "${tmpDir}"`, { stdio: "pipe" });

    const extractedBinary = path.join(tmpDir, BINARY_NAME);
    if (!fs.existsSync(extractedBinary)) {
      throw new Error(
        `Binary not found after extraction. Expected: ${BINARY_NAME} in archive.`
      );
    }

    fs.copyFileSync(extractedBinary, BINARY_PATH);
    fs.chmodSync(BINARY_PATH, 0o755);

    console.log(`[cortex-memory] Installed ${BINARY_NAME} to ${BINARY_PATH}`);
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
}

install().catch((err) => {
  console.error(`[cortex-memory] Installation failed: ${err.message}`);
  console.error(
    "[cortex-memory] You can manually download the binary from:"
  );
  console.error(`  https://github.com/${REPO}/releases/latest`);
  process.exit(1);
});
