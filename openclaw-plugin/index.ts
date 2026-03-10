/**
 * Cortex Memory Plugin for OpenClaw
 *
 * Persistent memory engine with 4-tier architecture:
 * - Working → Episodic → Semantic → Procedural
 * - Bayesian belief system (self-correcting user understanding)
 * - People graph (cross-channel identity resolution)
 * - Multi-signal retrieval (similarity + temporal + salience + social)
 *
 * Spawns cortex-mcp-server as a child process, communicates via JSON-RPC stdio.
 */

import { Type } from "@sinclair/typebox";
import type { OpenClawPluginApi } from "openclaw/plugin-sdk";
import { spawn, type ChildProcess } from "node:child_process";
import { createInterface } from "node:readline";
import { homedir } from "node:os";
import { existsSync } from "node:fs";
import { join } from "node:path";

// ============================================================================
// Types
// ============================================================================

type CortexConfig = {
  dbPath: string;
  binaryPath: string;
  autoCapture: boolean;
  autoRecall: boolean;
  topK: number;
};

// ============================================================================
// Cortex MCP Client — spawns binary, talks JSON-RPC
// ============================================================================

class CortexClient {
  private process: ChildProcess | null = null;
  private requestId = 0;
  private pending = new Map<
    number,
    { resolve: (v: any) => void; reject: (e: Error) => void }
  >();
  private ready = false;

  constructor(
    private binaryPath: string,
    private dbPath: string,
  ) {}

  async start(): Promise<void> {
    this.process = spawn(this.binaryPath, [this.dbPath], {
      stdio: ["pipe", "pipe", "pipe"],
    });

    if (!this.process.stdout || !this.process.stdin) {
      throw new Error("Failed to spawn cortex-mcp-server");
    }

    const rl = createInterface({ input: this.process.stdout });
    rl.on("line", (line) => {
      try {
        const resp = JSON.parse(line);
        const pending = this.pending.get(resp.id);
        if (pending) {
          this.pending.delete(resp.id);
          if (resp.error) {
            pending.reject(new Error(resp.error.message));
          } else {
            pending.resolve(resp.result);
          }
        }
      } catch {
        // ignore non-JSON lines
      }
    });

    this.process.on("exit", () => {
      this.ready = false;
      for (const [, p] of this.pending) {
        p.reject(new Error("cortex-mcp-server exited"));
      }
      this.pending.clear();
    });

    // Initialize MCP protocol
    await this.request("initialize", {
      protocolVersion: "2024-11-05",
      capabilities: {},
      clientInfo: { name: "openclaw-cortex", version: "0.1.0" },
    });
    this.ready = true;
  }

  async stop(): Promise<void> {
    if (this.process) {
      this.process.kill();
      this.process = null;
    }
  }

  private request(method: string, params: any = {}): Promise<any> {
    return new Promise((resolve, reject) => {
      if (!this.process?.stdin) {
        return reject(new Error("cortex-mcp-server not running"));
      }
      const id = ++this.requestId;
      this.pending.set(id, { resolve, reject });
      const msg = JSON.stringify({ jsonrpc: "2.0", id, method, params });
      this.process.stdin.write(msg + "\n");
    });
  }

  async callTool(name: string, args: Record<string, unknown>): Promise<string> {
    const result = await this.request("tools/call", { name, arguments: args });
    const content = result?.content;
    if (Array.isArray(content) && content.length > 0) {
      return content[0].text || "";
    }
    return JSON.stringify(result);
  }
}

// ============================================================================
// Config Parser
// ============================================================================

function findBinary(configured?: string): string {
  if (configured && existsSync(configured)) return configured;

  const candidates = [
    join(homedir(), ".local", "bin", "cortex-mcp-server"),
    join(homedir(), ".cargo", "bin", "cortex-mcp-server"),
    "/usr/local/bin/cortex-mcp-server",
  ];

  for (const path of candidates) {
    if (existsSync(path)) return path;
  }

  // Fallback to PATH
  return "cortex-mcp-server";
}

function parseConfig(raw: unknown): CortexConfig {
  const cfg = (raw && typeof raw === "object" ? raw : {}) as Record<
    string,
    unknown
  >;
  return {
    dbPath:
      typeof cfg.dbPath === "string" && cfg.dbPath
        ? cfg.dbPath
        : join(homedir(), ".cortex", "memory.db"),
    binaryPath: findBinary(
      typeof cfg.binaryPath === "string" ? cfg.binaryPath : undefined,
    ),
    autoCapture: cfg.autoCapture !== false,
    autoRecall: cfg.autoRecall !== false,
    topK: typeof cfg.topK === "number" ? cfg.topK : 10,
  };
}

// ============================================================================
// Plugin Definition
// ============================================================================

const cortexPlugin = {
  id: "cortex-memory",
  name: "Memory (Cortex)",
  description:
    "Cortex persistent memory engine — 4-tier architecture with Bayesian beliefs, people graph, and multi-signal retrieval. Local-first, zero-cloud, Rust-native.",
  kind: "memory" as const,
  configSchema: {
    parse: parseConfig,
  },

  register(api: OpenClawPluginApi) {
    const cfg = parseConfig(api.pluginConfig);
    const client = new CortexClient(cfg.binaryPath, cfg.dbPath);
    let started = false;

    async function ensureStarted() {
      if (!started) {
        await client.start();
        started = true;
      }
    }

    api.logger.info(
      `cortex-memory: registered (db: ${cfg.dbPath}, binary: ${cfg.binaryPath}, autoRecall: ${cfg.autoRecall}, autoCapture: ${cfg.autoCapture})`,
    );

    // ── memory_search ────────────────────────────────────────────────────

    api.registerTool(
      {
        name: "memory_search",
        label: "Memory Search",
        description:
          "Search through persistent memories. Returns results ranked by similarity, recency, salience, and social context. Use when you need context about the user, their preferences, or past interactions.",
        parameters: Type.Object({
          query: Type.String({ description: "What to search for" }),
          limit: Type.Optional(
            Type.Number({
              description: `Max results (default: ${cfg.topK})`,
            }),
          ),
          channel: Type.Optional(
            Type.String({ description: "Filter by channel" }),
          ),
          person_id: Type.Optional(
            Type.String({ description: "Filter by person UUID" }),
          ),
        }),
        async execute(_toolCallId, params) {
          const { query, limit, channel, person_id } = params as {
            query: string;
            limit?: number;
            channel?: string;
            person_id?: string;
          };
          try {
            await ensureStarted();
            const result = await client.callTool("memory_search", {
              query,
              limit: limit ?? cfg.topK,
              ...(channel && { channel }),
              ...(person_id && { person_id }),
            });
            const parsed = JSON.parse(result);
            const memories = parsed.results || [];
            if (memories.length === 0) {
              return {
                content: [
                  { type: "text", text: "No relevant memories found." },
                ],
                details: { count: 0 },
              };
            }
            const text = memories
              .map(
                (r: any, i: number) =>
                  `${i + 1}. [${r.tier}] ${r.text} (score: ${r.score}, ${r.created_at})`,
              )
              .join("\n");
            return {
              content: [
                {
                  type: "text",
                  text: `Found ${memories.length} memories:\n\n${text}`,
                },
              ],
              details: { count: memories.length, memories },
            };
          } catch (err) {
            return {
              content: [
                {
                  type: "text",
                  text: `Memory search failed: ${String(err)}`,
                },
              ],
              details: { error: String(err) },
            };
          }
        },
      },
      { name: "memory_search" },
    );

    // ── memory_store ─────────────────────────────────────────────────────

    api.registerTool(
      {
        name: "memory_store",
        label: "Memory Store",
        description:
          "Save important information to persistent memory. Use for preferences, facts, decisions, and anything worth remembering across sessions.",
        parameters: Type.Object({
          text: Type.String({ description: "Information to remember" }),
          channel: Type.Optional(
            Type.String({
              description: 'Source channel (default: "openclaw")',
            }),
          ),
          user_id: Type.Optional(
            Type.String({ description: "User ID in the channel" }),
          ),
          salience: Type.Optional(
            Type.Number({ description: "Importance 0.0-1.0" }),
          ),
        }),
        async execute(_toolCallId, params) {
          const { text, channel, user_id, salience } = params as {
            text: string;
            channel?: string;
            user_id?: string;
            salience?: number;
          };
          try {
            await ensureStarted();
            const result = await client.callTool("memory_ingest", {
              text,
              channel: channel || "openclaw",
              ...(user_id && { user_id }),
              ...(salience != null && { salience }),
            });
            return {
              content: [{ type: "text", text: `Stored: ${result}` }],
              details: JSON.parse(result),
            };
          } catch (err) {
            return {
              content: [
                { type: "text", text: `Memory store failed: ${String(err)}` },
              ],
              details: { error: String(err) },
            };
          }
        },
      },
      { name: "memory_store" },
    );

    // ── memory_context ───────────────────────────────────────────────────

    api.registerTool(
      {
        name: "memory_get",
        label: "Memory Context",
        description:
          "Get a comprehensive context summary from all memory tiers. Returns user profile, recent episodes, beliefs, and relationships — ready for injection into system prompts.",
        parameters: Type.Object({
          max_tokens: Type.Optional(
            Type.Number({ description: "Max context length (default: 2000)" }),
          ),
          channel: Type.Optional(
            Type.String({ description: "Filter by channel" }),
          ),
          person_id: Type.Optional(
            Type.String({ description: "Filter by person UUID" }),
          ),
        }),
        async execute(_toolCallId, params) {
          const { max_tokens, channel, person_id } = params as {
            max_tokens?: number;
            channel?: string;
            person_id?: string;
          };
          try {
            await ensureStarted();
            const result = await client.callTool("memory_context", {
              max_tokens: max_tokens ?? 2000,
              ...(channel && { channel }),
              ...(person_id && { person_id }),
            });
            return {
              content: [{ type: "text", text: result }],
            };
          } catch (err) {
            return {
              content: [
                {
                  type: "text",
                  text: `Memory context failed: ${String(err)}`,
                },
              ],
            };
          }
        },
      },
      { name: "memory_get" },
    );

    // ── belief_observe ───────────────────────────────────────────────────

    api.registerTool(
      {
        name: "belief_observe",
        label: "Belief Update",
        description:
          "Update a belief based on new evidence. Beliefs are probabilistic — supporting evidence increases confidence, contradicting evidence decreases it. Use to track things learned about the user over time.",
        parameters: Type.Object({
          key: Type.String({
            description:
              'Belief key (e.g. "user_prefers_dark_mode", "user_is_developer")',
          }),
          supports: Type.Boolean({
            description: "true = supporting evidence, false = contradicting",
          }),
          strength: Type.Optional(
            Type.Number({ description: "Evidence strength 0.0-1.0 (default: 0.5)" }),
          ),
        }),
        async execute(_toolCallId, params) {
          const { key, supports, strength } = params as {
            key: string;
            supports: boolean;
            strength?: number;
          };
          try {
            await ensureStarted();
            const result = await client.callTool("belief_observe", {
              key,
              supports,
              ...(strength != null && { strength }),
            });
            return {
              content: [{ type: "text", text: `Belief updated: ${result}` }],
              details: JSON.parse(result),
            };
          } catch (err) {
            return {
              content: [
                { type: "text", text: `Belief update failed: ${String(err)}` },
              ],
            };
          }
        },
      },
      { name: "belief_observe" },
    );

    // ── fact_add ─────────────────────────────────────────────────────────

    api.registerTool(
      {
        name: "fact_add",
        label: "Fact Store",
        description:
          'Store a semantic fact as a subject-predicate-object triple. Use for structured knowledge like "User works_at Google" or "User speaks Chinese".',
        parameters: Type.Object({
          subject: Type.String({ description: "Subject of the fact" }),
          predicate: Type.String({
            description:
              'Relationship (e.g. "works_at", "lives_in", "speaks")',
          }),
          object: Type.String({ description: "Object of the fact" }),
          confidence: Type.Optional(
            Type.Number({ description: "Confidence 0.0-1.0 (default: 0.8)" }),
          ),
        }),
        async execute(_toolCallId, params) {
          const { subject, predicate, object, confidence } = params as {
            subject: string;
            predicate: string;
            object: string;
            confidence?: number;
          };
          try {
            await ensureStarted();
            const result = await client.callTool("fact_add", {
              subject,
              predicate,
              object,
              ...(confidence != null && { confidence }),
              channel: "openclaw",
            });
            return {
              content: [{ type: "text", text: `Fact stored: ${result}` }],
              details: JSON.parse(result),
            };
          } catch (err) {
            return {
              content: [
                { type: "text", text: `Fact store failed: ${String(err)}` },
              ],
            };
          }
        },
      },
      { name: "fact_add" },
    );

    // ── preference_set ───────────────────────────────────────────────────

    api.registerTool(
      {
        name: "preference_set",
        label: "Preference Set",
        description:
          "Store a user preference. Use for language, communication style, timezone, favorite tools, etc.",
        parameters: Type.Object({
          key: Type.String({
            description:
              'Preference key (e.g. "language", "code_style", "timezone")',
          }),
          value: Type.String({ description: "Preference value" }),
          confidence: Type.Optional(
            Type.Number({ description: "Confidence 0.0-1.0 (default: 0.9)" }),
          ),
        }),
        async execute(_toolCallId, params) {
          const { key, value, confidence } = params as {
            key: string;
            value: string;
            confidence?: number;
          };
          try {
            await ensureStarted();
            const result = await client.callTool("preference_set", {
              key,
              value,
              ...(confidence != null && { confidence }),
            });
            return {
              content: [
                { type: "text", text: `Preference saved: ${result}` },
              ],
              details: JSON.parse(result),
            };
          } catch (err) {
            return {
              content: [
                {
                  type: "text",
                  text: `Preference save failed: ${String(err)}`,
                },
              ],
            };
          }
        },
      },
      { name: "preference_set" },
    );

    // ── person_resolve ───────────────────────────────────────────────────

    api.registerTool(
      {
        name: "person_resolve",
        label: "Person Resolve",
        description:
          "Resolve or create a person identity. Links a channel-specific user ID to a cross-channel person profile.",
        parameters: Type.Object({
          name: Type.String({ description: "Display name" }),
          channel: Type.String({
            description: 'Channel (e.g. "telegram", "slack")',
          }),
          channel_user_id: Type.String({
            description: "User ID in that channel",
          }),
        }),
        async execute(_toolCallId, params) {
          const { name, channel, channel_user_id } = params as {
            name: string;
            channel: string;
            channel_user_id: string;
          };
          try {
            await ensureStarted();
            const result = await client.callTool("person_resolve", {
              name,
              channel,
              channel_user_id,
            });
            return {
              content: [{ type: "text", text: `Person: ${result}` }],
              details: JSON.parse(result),
            };
          } catch (err) {
            return {
              content: [
                {
                  type: "text",
                  text: `Person resolve failed: ${String(err)}`,
                },
              ],
            };
          }
        },
      },
      { name: "person_resolve" },
    );

    // ── Auto-recall hook ─────────────────────────────────────────────────

    if (cfg.autoRecall) {
      api.registerHook(
        ["agent:beforeTurn"],
        async (event) => {
          try {
            await ensureStarted();
            const context = await client.callTool("memory_context", {
              max_tokens: 1500,
              channel: (event as any).channel || undefined,
            });
            if (context && context.trim()) {
              return {
                inject: {
                  role: "system",
                  content: context,
                },
              };
            }
          } catch {
            // Silently fail — don't block the agent
          }
          return {};
        },
        { label: "cortex-auto-recall" },
      );
    }

    // ── Auto-capture hook ────────────────────────────────────────────────

    if (cfg.autoCapture) {
      api.registerHook(
        ["agent:afterTurn"],
        async (event) => {
          try {
            await ensureStarted();
            const messages = (event as any).messages;
            if (!messages || messages.length === 0) return {};

            // Find the last user message
            const lastUser = [...messages]
              .reverse()
              .find((m: any) => m.role === "user");
            if (!lastUser?.content) return {};

            const text =
              typeof lastUser.content === "string"
                ? lastUser.content
                : JSON.stringify(lastUser.content);

            // Only store if substantial (>20 chars)
            if (text.length > 20) {
              await client.callTool("memory_ingest", {
                text: text.slice(0, 2000),
                channel: (event as any).channel || "openclaw",
              });
            }
          } catch {
            // Silently fail
          }
          return {};
        },
        { label: "cortex-auto-capture" },
      );
    }

    // ── CLI ──────────────────────────────────────────────────────────────

    api.registerCli(
      ({ program }) => {
        const cmd = program
          .command("cortex")
          .description("Cortex memory engine");

        cmd
          .command("search <query>")
          .description("Search memories")
          .option("-l, --limit <n>", "Max results", "10")
          .action(async (query: string, opts: { limit: string }) => {
            await ensureStarted();
            const result = await client.callTool("memory_search", {
              query,
              limit: parseInt(opts.limit, 10),
            });
            console.log(result);
          });

        cmd
          .command("context")
          .description("Get full memory context")
          .action(async () => {
            await ensureStarted();
            const result = await client.callTool("memory_context", {
              max_tokens: 3000,
            });
            console.log(result);
          });

        cmd
          .command("beliefs")
          .description("List current beliefs")
          .option("-t, --threshold <n>", "Min confidence", "0.5")
          .action(async (opts: { threshold: string }) => {
            await ensureStarted();
            const result = await client.callTool("belief_list", {
              threshold: parseFloat(opts.threshold),
            });
            console.log(result);
          });

        cmd
          .command("store <text>")
          .description("Store a memory")
          .action(async (text: string) => {
            await ensureStarted();
            const result = await client.callTool("memory_ingest", {
              text,
              channel: "cli",
            });
            console.log(result);
          });
      },
      { commands: ["cortex"] },
    );

    // ── Graceful shutdown ────────────────────────────────────────────────

    api.registerHook(
      ["agent:shutdown"],
      async () => {
        await client.stop();
        return {};
      },
      { label: "cortex-shutdown" },
    );
  },
};

export default cortexPlugin;
