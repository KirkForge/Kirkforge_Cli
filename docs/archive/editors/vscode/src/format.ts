export function formatTodoHtml(items: { text: string; done: boolean; in_progress?: boolean }[]): string {
  return items
    .map((it) => {
      const state = it.done ? 'completed' : it.in_progress ? 'in_progress' : 'pending';
      const color = state === 'completed' ? 'green' : state === 'in_progress' ? '#c8a800' : 'gray';
      const checkbox = it.done ? '\u2611' : it.in_progress ? '\u25A0' : '\u2610';
      return `<div style="color:${color}">${checkbox} ${escapeHtml(it.text)}</div>`;
    })
    .join('\n');
}

export function escapeHtml(text: string): string {
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;');
}

export function truncate(s: string, max: number): string {
  return s.length > max ? s.slice(0, max) + '...' : s;
}