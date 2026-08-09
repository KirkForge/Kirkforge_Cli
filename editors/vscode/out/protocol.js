"use strict";
// NDJSON protocol v1 types for the KirkForge VS Code extension bridge.
Object.defineProperty(exports, "__esModule", { value: true });
exports.parseEvent = parseEvent;
function parseEvent(line) {
    try {
        const obj = JSON.parse(line);
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
                return obj;
            default:
                return undefined;
        }
    }
    catch {
        return undefined;
    }
}
//# sourceMappingURL=protocol.js.map