#!/usr/bin/env bash
set -euo pipefail

REPO="gambletan/cortex"
INSTALL_DIR="${HOME}/.local/bin"
LITE=false
IDE=""

usage() {
  cat <<EOF
Usage: install.sh [OPTIONS]

Install cortex-mcp-server from GitHub Releases.

Options:
  --lite              Download lite version (no embedding model, <5MB)
  --ide <target>      Auto-configure MCP for IDE: claude|cursor|windsurf|all
  --dir <path>        Install directory (default: ~/.local/bin)
  -h, --help          Show this help

Examples:
  curl -fsSL https://raw.githubusercontent.com/${REPO}/main/install.sh | bash
  curl -fsSL https://raw.githubusercontent.com/${REPO}/main/install.sh | bash -s -- --lite --ide claude
EOF
  exit 0
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --lite) LITE=true; shift ;;
    --ide) IDE="$2"; shift 2 ;;
    --dir) INSTALL_DIR="$2"; shift 2 ;;
    -h|--help) usage ;;
    *) echo "Unknown option: $1"; usage ;;
  esac
done

# Detect OS and architecture
OS="$(uname -s)"
ARCH="$(uname -m)"

case "${OS}" in
  Darwin) OS_SUFFIX="darwin" ;;
  Linux)  OS_SUFFIX="linux" ;;
  *) echo "Unsupported OS: ${OS}"; exit 1 ;;
esac

case "${ARCH}" in
  arm64|aarch64) ARCH_SUFFIX="arm64" ;;
  x86_64)        ARCH_SUFFIX="x86_64" ;;
  *) echo "Unsupported architecture: ${ARCH}"; exit 1 ;;
esac

SUFFIX="${OS_SUFFIX}-${ARCH_SUFFIX}"
if [ "${LITE}" = true ]; then
  TARBALL="cortex-mcp-server-lite-${SUFFIX}.tar.gz"
else
  TARBALL="cortex-mcp-server-${SUFFIX}.tar.gz"
fi

# Get latest release tag
echo "Fetching latest release..."
LATEST=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | sed -E 's/.*"([^"]+)".*/\1/')
if [ -z "${LATEST}" ]; then
  echo "Failed to fetch latest release. Check https://github.com/${REPO}/releases"
  exit 1
fi

DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${LATEST}/${TARBALL}"

echo "Downloading ${TARBALL} (${LATEST})..."
mkdir -p "${INSTALL_DIR}"
TMP=$(mktemp -d)
trap "rm -rf ${TMP}" EXIT

curl -fsSL "${DOWNLOAD_URL}" -o "${TMP}/${TARBALL}"
tar xzf "${TMP}/${TARBALL}" -C "${TMP}"
mv "${TMP}/cortex-mcp-server" "${INSTALL_DIR}/cortex-mcp-server"
chmod +x "${INSTALL_DIR}/cortex-mcp-server"

echo "Installed cortex-mcp-server to ${INSTALL_DIR}/cortex-mcp-server"

# Verify install
if "${INSTALL_DIR}/cortex-mcp-server" info >/dev/null 2>&1; then
  echo "Verification: OK"
else
  echo "Verification: binary runs but info command may need a DB"
fi

# IDE configuration
configure_ide() {
  local ide="$1"
  local binary="${INSTALL_DIR}/cortex-mcp-server"
  local db_path="${HOME}/.cortex/memory.db"

  case "${ide}" in
    claude)
      local config_dir="${HOME}/.claude"
      local config_file="${config_dir}/claude_desktop_config.json"
      mkdir -p "${config_dir}"
      if [ -f "${config_file}" ]; then
        # Merge using python3 (available on macOS and most Linux)
        python3 -c "
import json, sys
try:
    with open('${config_file}') as f:
        config = json.load(f)
except (json.JSONDecodeError, FileNotFoundError):
    config = {}
config.setdefault('mcpServers', {})
config['mcpServers']['cortex-memory'] = {
    'command': '${binary}',
    'args': ['${db_path}']
}
with open('${config_file}', 'w') as f:
    json.dump(config, f, indent=2)
print('Configured Claude Code MCP: ${config_file}')
"
      else
        cat > "${config_file}" <<CONF
{
  "mcpServers": {
    "cortex-memory": {
      "command": "${binary}",
      "args": ["${db_path}"]
    }
  }
}
CONF
        echo "Created Claude Code MCP config: ${config_file}"
      fi
      ;;
    cursor)
      local config_file="${HOME}/.cursor/mcp.json"
      mkdir -p "$(dirname "${config_file}")"
      if [ -f "${config_file}" ]; then
        python3 -c "
import json
try:
    with open('${config_file}') as f:
        config = json.load(f)
except (json.JSONDecodeError, FileNotFoundError):
    config = {}
config.setdefault('mcpServers', {})
config['mcpServers']['cortex-memory'] = {
    'command': '${binary}',
    'args': ['${db_path}']
}
with open('${config_file}', 'w') as f:
    json.dump(config, f, indent=2)
print('Configured Cursor MCP: ${config_file}')
"
      else
        cat > "${config_file}" <<CONF
{
  "mcpServers": {
    "cortex-memory": {
      "command": "${binary}",
      "args": ["${db_path}"]
    }
  }
}
CONF
        echo "Created Cursor MCP config: ${config_file}"
      fi
      ;;
    windsurf)
      local config_file="${HOME}/.windsurf/mcp.json"
      mkdir -p "$(dirname "${config_file}")"
      if [ -f "${config_file}" ]; then
        python3 -c "
import json
try:
    with open('${config_file}') as f:
        config = json.load(f)
except (json.JSONDecodeError, FileNotFoundError):
    config = {}
config.setdefault('mcpServers', {})
config['mcpServers']['cortex-memory'] = {
    'command': '${binary}',
    'args': ['${db_path}']
}
with open('${config_file}', 'w') as f:
    json.dump(config, f, indent=2)
print('Configured Windsurf MCP: ${config_file}')
"
      else
        cat > "${config_file}" <<CONF
{
  "mcpServers": {
    "cortex-memory": {
      "command": "${binary}",
      "args": ["${db_path}"]
    }
  }
}
CONF
        echo "Created Windsurf MCP config: ${config_file}"
      fi
      ;;
    *)
      echo "Unknown IDE: ${ide}"
      return 1
      ;;
  esac
}

if [ -n "${IDE}" ]; then
  if [ "${IDE}" = "all" ]; then
    for target in claude cursor windsurf; do
      configure_ide "${target}" || true
    done
  else
    configure_ide "${IDE}"
  fi
fi

# Check PATH
if ! echo "${PATH}" | grep -q "${INSTALL_DIR}"; then
  echo ""
  echo "NOTE: ${INSTALL_DIR} is not in your PATH."
  echo "Add this to your shell profile:"
  echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
fi

echo ""
echo "Done! If you find Cortex useful, please star the repo:"
echo "  https://github.com/${REPO} ⭐"
