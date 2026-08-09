// NDJSON protocol v1 types for the KirkForge VS Code extension bridge.

export interface NdjsonEvent {
  type: string;
}

export interface TurnStartEvent extends NdjsonEvent {
  type: 'turn_start';
  id: string;
  timestamp: string;
  protocol_version?: string;
}

export interface MessageEvent extends NdjsonEvent {
  type: 'message';
  role: 'user' | 'assistant';
  content: string;
}

export interface TokenEvent extends NdjsonEvent {
  type: 'token';
  content: string;
}

export interface ToolCallEvent extends NdjsonEvent {
  type: 'tool_call';
  name: string;
  arguments: Record<string, unknown>;
}

export interface ToolResultEvent extends NdjsonEvent {
  type: 'tool_result';
  name: string;
  success: boolean;
  output?: string;
  error?: string;
}

export interface EditEvent extends NdjsonEvent {
  type: 'edit';
  path: string;
  old_string?: string;
  new_string?: string;
}

export interface TodoUpdateEvent extends NdjsonEvent {
  type: 'todo_update';
  items: { text: string; done: boolean; in_progress?: boolean }[];
}

export interface DoneEvent extends NdjsonEvent {
  type: 'done';
  finish_reason: string;
  usage?: {
    prompt_tokens: number;
    completion_tokens: number;
    total_tokens: number;
  };
}

export interface DiagnosticsEvent extends NdjsonEvent {
  type: 'diagnostics';
  uri: string;
  diagnostics: { message: string; severity: number; range: unknown }[];
}

export type BridgeEvent =
  | TurnStartEvent
  | MessageEvent
  | TokenEvent
  | ToolCallEvent
  | ToolResultEvent
  | EditEvent
  | TodoUpdateEvent
  | DoneEvent
  | DiagnosticsEvent;

export function parseEvent(line: string): BridgeEvent | undefined {
  try {
    const obj = JSON.parse(line) as Record<string, unknown>;
    if (typeof obj.type !== 'string') {
      return undefined;
    }
    switch (obj.type) {
      case 'turn_start':
      case 'message':
      case 'token':
      case 'tool_call':
      case 'tool_result':
      case 'edit':
      case 'todo_update':
      case 'done':
      case 'diagnostics':
        return obj as unknown as BridgeEvent;
      default:
        return undefined;
    }
  } catch {
    return undefined;
  }
}