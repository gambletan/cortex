/* tslint:disable */
/* eslint-disable */

/**
 * In-memory Cortex engine for browser use.
 */
export class CortexWasm {
    free(): void;
    [Symbol.dispose](): void;
    /**
     * Add a fact (subject-predicate-object).
     */
    add_fact(subject: string, predicate: string, object: string, confidence: number): void;
    /**
     * Get memory count.
     */
    count(): number;
    /**
     * Get all beliefs as JSON.
     */
    get_beliefs(): string;
    /**
     * Ingest a memory. Returns the memory ID.
     */
    ingest(text: string, channel: string): string;
    /**
     * Create a new in-memory Cortex instance.
     */
    constructor();
    /**
     * Observe evidence for a belief. Returns new probability.
     */
    observe_belief(key: string, supports: boolean, strength: number): number;
    /**
     * Query facts by entity. Returns JSON array.
     */
    query_facts(entity: string): string;
    /**
     * Search memories by query. Returns JSON array of results.
     */
    search(query: string, limit: number): string;
    /**
     * Get stats as JSON.
     */
    stats(): string;
}

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_cortexwasm_free: (a: number, b: number) => void;
    readonly cortexwasm_add_fact: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => void;
    readonly cortexwasm_count: (a: number) => number;
    readonly cortexwasm_get_beliefs: (a: number, b: number) => void;
    readonly cortexwasm_ingest: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly cortexwasm_new: () => number;
    readonly cortexwasm_observe_belief: (a: number, b: number, c: number, d: number, e: number) => number;
    readonly cortexwasm_query_facts: (a: number, b: number, c: number, d: number) => void;
    readonly cortexwasm_search: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly cortexwasm_stats: (a: number, b: number) => void;
    readonly __wbindgen_export: (a: number) => void;
    readonly __wbindgen_export2: (a: number, b: number) => number;
    readonly __wbindgen_export3: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_add_to_stack_pointer: (a: number) => number;
    readonly __wbindgen_export4: (a: number, b: number, c: number) => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
