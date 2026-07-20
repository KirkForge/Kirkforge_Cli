import * as vscode from 'vscode';

export interface LspQueryRequest {
  query: 'symbol' | 'type' | 'diagnostics';
  symbol?: string;
  file?: string;
}

export interface LspQueryResponse {
  results: unknown[];
}

export class LspBridge {
  constructor(private readonly workspaceRoot: string) {}

  async query(req: LspQueryRequest): Promise<LspQueryResponse> {
    switch (req.query) {
      case 'symbol':
        if (!req.symbol) {
          return { results: [] };
        }
        const symbols = await vscode.commands.executeCommand<vscode.SymbolInformation[]>(
          'vscode.executeWorkspaceSymbolProvider',
          req.symbol
        );
        return { results: symbols ?? [] };
      case 'type': {
        if (!req.file) {
          return { results: [] };
        }
        const uri = vscode.Uri.file(this.joinPath(req.file));
        const definitions = await vscode.commands.executeCommand<vscode.Location[] | vscode.LocationLink[]>(
          'vscode.executeTypeDefinitionProvider',
          uri,
          new vscode.Position(0, 0)
        ) ?? [];
        return { results: definitions ?? [] };
      }
      case 'diagnostics': {
        const all = vscode.languages.getDiagnostics();
        const target = req.file ? this.joinPath(req.file) : undefined;
        const filtered = target
          ? all.filter(([uri]) => uri.fsPath === target)
          : all;
        return {
          results: filtered.map(([uri, diagnostics]) => ({
            file: uri.fsPath,
            diagnostics: diagnostics.map((d) => ({
              message: d.message,
              severity: d.severity,
              range: d.range,
            })),
          })),
        };
      }
      default:
        return { results: [] };
    }
  }

  private joinPath(relative: string): string {
    return this.workspaceRoot.replace(/[/\\]$/, '') + '/' + relative.replace(/^[/\\]+/, '');
  }
}
