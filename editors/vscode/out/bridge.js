"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.KirkForgeBridge = void 0;
const child_process_1 = require("child_process");
const events_1 = require("events");
const protocol_1 = require("./protocol");
class KirkForgeBridge extends events_1.EventEmitter {
    options;
    child;
    buffer = '';
    constructor(options) {
        super();
        this.options = options;
    }
    start() {
        if (this.child) {
            return;
        }
        this.child = (0, child_process_1.spawn)(this.options.binaryPath, ['run', '--non-interactive', '--output', this.options.outputFormat], { cwd: this.options.cwd, env: process.env });
        this.child.stdout.on('data', (chunk) => {
            this.buffer += chunk.toString('utf-8');
            this.flush();
        });
        this.child.stderr.on('data', (chunk) => {
            this.emit('stderr', chunk.toString('utf-8'));
        });
        this.child.on('exit', (code) => this.emit('exit', code));
        this.child.on('error', (err) => this.emit('error', err));
    }
    stop() {
        this.child?.kill('SIGTERM');
        this.child = undefined;
        this.buffer = '';
    }
    writeLine(line) {
        this.child?.stdin.write(line + '\n');
    }
    sendPrompt(text) {
        this.writeLine(JSON.stringify({ type: 'prompt', text }));
    }
    sendApproval(toolCallId, approved) {
        this.writeLine(JSON.stringify({ type: 'approval', id: toolCallId, approved }));
    }
    flush() {
        const lines = this.buffer.split('\n');
        this.buffer = lines.pop() ?? '';
        for (const line of lines) {
            if (!line.trim()) {
                continue;
            }
            const event = (0, protocol_1.parseEvent)(line);
            if (event) {
                this.emit('event', event);
            }
            else {
                this.emit('unparseable', line);
            }
        }
    }
}
exports.KirkForgeBridge = KirkForgeBridge;
//# sourceMappingURL=bridge.js.map