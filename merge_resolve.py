#!/usr/bin/env python3
"""Union-resolve 3-way merge conflicts: keep HEAD + incoming sides (drop base)."""
import sys, re

def resolve_file(path):
    with open(path, 'r') as f:
        text = f.read()

    # Match conflict block. Base section may be empty.
    # Format: <<<<<<< HEAD\n<head>\n||||||| <sha>\n<base>\n=======\n<incoming>\n>>>>>>> <branch>
    # When base is empty: ||||||| <sha>\n=======\n (no content between)
    pattern = re.compile(
        r'<<<<<<< HEAD\n(.*?)\n\|\|\|\|\|\|\| [^\n]*\n(.*?)=======\n(.*?)\n>>>>>>> [^\n]*',
        re.DOTALL
    )
    def resolve(m):
        head = m.group(1)
        incoming = m.group(3)
        return head.rstrip('\n') + '\n' + incoming.rstrip('\n')

    new_text, count = pattern.subn(resolve, text)
    print(f"  {path}: {count} conflict(s) resolved")
    with open(path, 'w') as f:
        f.write(new_text + '\n')

if __name__ == '__main__':
    for p in sys.argv[1:]:
        resolve_file(p)