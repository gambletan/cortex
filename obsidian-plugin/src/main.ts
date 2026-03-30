import { Plugin, ItemView, WorkspaceLeaf, MarkdownView } from "obsidian";
import { ChildProcess, spawn } from "child_process";

const VIEW_TYPE_CORTEX = "cortex-memory-browser";

class CortexMemoryView extends ItemView {
  getViewType(): string {
    return VIEW_TYPE_CORTEX;
  }

  getDisplayText(): string {
    return "Cortex Memory";
  }

  async onOpen(): Promise<void> {
    const container = this.containerEl.children[1];
    container.empty();
    container.createEl("h4", { text: "Cortex Memory Browser" });
    container.createEl("p", { text: "Loading memories..." });
    // TODO: Render memory list from MCP server via JSON-RPC
    // TODO: Add search/filter UI
    // TODO: Add memory detail panel
  }

  async onClose(): Promise<void> {
    // TODO: Cleanup any listeners
  }
}

export default class CortexMemoryPlugin extends Plugin {
  private mcpProcess: ChildProcess | null = null;
  private requestId = 0;

  async onload(): Promise<void> {
    this.startMcpServer();

    this.registerView(VIEW_TYPE_CORTEX, (leaf: WorkspaceLeaf) => {
      return new CortexMemoryView(leaf);
    });

    this.addCommand({
      id: "open-memory-browser",
      name: "Memory Browser",
      callback: () => {
        this.activateView();
      },
    });

    this.addCommand({
      id: "inject-context",
      name: "Inject Context",
      editorCallback: async (editor) => {
        // TODO: Call memory_context via JSON-RPC and insert result at cursor
        const context = await this.callMcp("memory_context", {});
        if (context) {
          editor.replaceSelection(context);
        }
      },
    });
  }

  async onunload(): Promise<void> {
    if (this.mcpProcess) {
      this.mcpProcess.kill();
      this.mcpProcess = null;
    }
  }

  private startMcpServer(): void {
    try {
      // Look for cortex-mcp-server in PATH or common install locations
      const binaryName = "cortex-mcp-server";
      this.mcpProcess = spawn(binaryName, [], {
        stdio: ["pipe", "pipe", "pipe"],
      });

      this.mcpProcess.on("error", (err: Error) => {
        console.error("[cortex-memory] Failed to start MCP server:", err.message);
        // TODO: Show notice to user with install instructions
      });

      this.mcpProcess.on("exit", (code: number | null) => {
        console.log("[cortex-memory] MCP server exited with code:", code);
        this.mcpProcess = null;
      });

      // TODO: Set up JSON-RPC message framing on stdout/stdin
    } catch (err) {
      console.error("[cortex-memory] Error spawning MCP server:", err);
    }
  }

  private async callMcp(method: string, params: Record<string, unknown>): Promise<string | null> {
    if (!this.mcpProcess || !this.mcpProcess.stdin || !this.mcpProcess.stdout) {
      console.error("[cortex-memory] MCP server not running");
      return null;
    }

    const id = ++this.requestId;
    const request = JSON.stringify({
      jsonrpc: "2.0",
      id,
      method,
      params,
    });

    // TODO: Implement proper JSON-RPC request/response handling with
    // content-length framing and response correlation by ID
    this.mcpProcess.stdin.write(request + "\n");

    // Placeholder: actual implementation needs to await the matching response
    return null;
  }

  private async activateView(): Promise<void> {
    const existing = this.app.workspace.getLeavesOfType(VIEW_TYPE_CORTEX);
    if (existing.length > 0) {
      this.app.workspace.revealLeaf(existing[0]);
      return;
    }

    const leaf = this.app.workspace.getRightLeaf(false);
    if (leaf) {
      await leaf.setViewState({
        type: VIEW_TYPE_CORTEX,
        active: true,
      });
      this.app.workspace.revealLeaf(leaf);
    }
  }
}
