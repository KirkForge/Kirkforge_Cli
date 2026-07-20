import { ChildProcessWithoutNullStreams, spawn } from 'child_process';
import { EventEmitter } from 'events';
import { BridgeEvent, parseEvent } from './protocol';

export interface BridgeOptions {
  binaryPath: string;
  cwd: string;
  outputFormat: 'ndjson';
}

export class KirkForgeBridge extends EventEmitter {
  private child: ChildProcessWithoutNullStreams | undefined;
  private buffer = '';

  constructor(private readonly options: BridgeOptions) {
    super();
  }

  start(): void {
    if (this.child) {
      return;
    }
    this.child = spawn(
      this.options.binaryPath,
      ['run', '--non-interactive', '--output-format', this.options.outputFormat],
      { cwd: this.options.cwd, env: process.env }
    );
    this.child.stdout.on('data', (chunk: Buffer) => {
      this.buffer += chunk.toString('utf-8');
      this.flush();
    });
    this.child.stderr.on('data', (chunk: Buffer) => {
      this.emit('stderr', chunk.toString('utf-8'));
    });
    this.child.on('exit', (code) => this.emit('exit', code));
    this.child.on('error', (err) => this.emit('error', err));
  }

  stop(): void {
    this.child?.kill('SIGTERM');
    this.child = undefined;
    this.buffer = '';
  }

  writeLine(line: string): void {
    this.child?.stdin.write(line + '\n');
  }

  private flush(): void {
    const lines = this.buffer.split('\n');
    this.buffer = lines.pop() ?? '';
    for (const line of lines) {
      if (!line.trim()) {
        continue;
      }
      const event = parseEvent(line);
      if (event) {
        this.emit('event', event);
      } else {
        this.emit('unparseable', line);
      }
    }
  }
}
