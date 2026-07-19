import { test } from 'node:test';
import assert from 'node:assert';

interface FakeTerminal {
  show: () => void;
  processId: Promise<number | undefined>;
}

interface VscodeCall {
  type: string;
  opts?: Record<string, unknown>;
  message?: string;
}

function makeVscode(options?: {
  workspaceFolders?: { uri: { fsPath: string } }[];
  terminal?: FakeTerminal;
  binaryPath?: string;
}): { vscode: any; calls: VscodeCall[]; terminal: FakeTerminal } {
  const calls: VscodeCall[] = [];
  const terminal: FakeTerminal = options?.terminal ?? {
    show: () => {},
    processId: Promise.resolve(1234),
  };
  const workspaceFolders = options?.workspaceFolders ?? [
    { uri: { fsPath: '/home/user/project' } },
  ];

  const vscode = {
    commands: {
      registerCommand: (_command: string, _handler: () => void) => ({
        dispose: () => {},
      }),
    },
    window: {
      createTerminal: (opts: Record<string, unknown>) => {
        calls.push({ type: 'createTerminal', opts });
        return terminal;
      },
      showWarningMessage: (message: string) => {
        calls.push({ type: 'warn', message });
        return Promise.resolve(undefined);
      },
    },
    workspace: {
      workspaceFolders,
      getConfiguration: (section: string) => ({
        get: (key: string, defaultValue: unknown) => {
          if (section === 'kirkforge' && key === 'binaryPath') {
            return options?.binaryPath ?? 'kirkforge';
          }
          return defaultValue;
        },
      }),
    },
    ConfigurationTarget: { Global: 1, Workspace: 2, WorkspaceFolder: 3 },
  };

  return { vscode, calls, terminal };
}

const ext = require('../extension');

test('activateWithApi registers kirkforge.start command', () => {
  const { vscode } = makeVscode();
  const context: any = { subscriptions: [] };

  ext.activateWithApi(context, vscode);

  assert.strictEqual(context.subscriptions.length, 1);
});

test('startKirkForge opens terminal with workspace root and default binary', async () => {
  const { vscode, calls } = makeVscode();

  await ext.startKirkForge(vscode);

  assert.strictEqual(calls.length, 1);
  assert.deepStrictEqual(calls[0], {
    type: 'createTerminal',
    opts: {
      name: 'KirkForge',
      cwd: '/home/user/project',
      shellPath: 'kirkforge',
      shellArgs: ['run'],
    },
  });
});

test('startKirkForge warns when no workspace is open', async () => {
  const { vscode, calls } = makeVscode({ workspaceFolders: [] });

  await ext.startKirkForge(vscode);

  assert.strictEqual(calls.length, 1);
  assert.strictEqual(calls[0].type, 'warn');
  assert.ok(
    calls[0].message?.includes('workspace folder'),
    `expected workspace-folder warning, got: ${calls[0].message}`
  );
});

test('startKirkForge uses configured binaryPath', async () => {
  const { vscode, calls } = makeVscode({ binaryPath: '/opt/kirkforge/bin/kirkforge' });

  await ext.startKirkForge(vscode);

  assert.strictEqual(calls[0].opts?.shellPath, '/opt/kirkforge/bin/kirkforge');
});
