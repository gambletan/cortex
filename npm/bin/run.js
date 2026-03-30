#!/usr/bin/env node
"use strict";

const path = require("path");
const fs = require("fs");
const { spawn } = require("child_process");

const BINARY_NAME = "cortex-mcp-server";
const BINARY_PATH = path.join(__dirname, BINARY_NAME);

if (!fs.existsSync(BINARY_PATH)) {
  console.error(
    `[cortex-memory] Binary not found at ${BINARY_PATH}\n` +
    `\n` +
    `The postinstall script may have failed. Try reinstalling:\n` +
    `  npm install -g cortex-memory\n` +
    `\n` +
    `Or download the binary manually from:\n` +
    `  https://github.com/gambletan/cortex/releases/latest`
  );
  process.exit(1);
}

const args = process.argv.slice(2);

const child = spawn(BINARY_PATH, args, {
  stdio: "inherit",
});

child.on("error", (err) => {
  console.error(`[cortex-memory] Failed to start ${BINARY_NAME}: ${err.message}`);
  process.exit(1);
});

child.on("exit", (code, signal) => {
  process.exit(signal ? 1 : (code ?? 1));
});
