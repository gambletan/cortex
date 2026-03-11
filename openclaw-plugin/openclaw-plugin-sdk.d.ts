declare module "openclaw/plugin-sdk" {
  export interface OpenClawPluginApi {
    pluginConfig: unknown;
    logger: {
      info(msg: string): void;
      warn(msg: string): void;
      error(msg: string): void;
    };
    registerTool(
      definition: {
        name: string;
        label: string;
        description: string;
        parameters: unknown;
        execute(toolCallId: string, params: unknown): Promise<{
          content: Array<{ type: string; text: string }>;
          details?: unknown;
        }>;
      },
      opts: { name: string },
    ): void;
    registerHook(
      events: string[],
      handler: (event: unknown) => Promise<Record<string, unknown>>,
      opts: { label: string },
    ): void;
    registerCli(
      setup: (ctx: { program: any }) => void,
      opts: { commands: string[] },
    ): void;
  }
}
