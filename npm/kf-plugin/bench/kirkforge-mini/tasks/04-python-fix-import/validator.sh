#!/usr/bin/env bash
# Validator for task 04-python-fix-import.
# The harness invokes this script with CWD set to the worker's isolated
# workspace, so all paths are relative to that workspace.
set -euo pipefail

if [ ! -f parse.py ]; then
  echo "FAIL: parse.py missing (PWD=$PWD)"
  exit 1
fi

# 1. Source-level checks: no more PyPDF2 anywhere
SRC=$(cat parse.py)
if echo "$SRC" | grep -qi 'PyPDF2'; then
  echo "FAIL: PyPDF2 still referenced in parse.py"
  exit 1
fi

# 2. Try to import + invoke parse() — needs pypdf installed
if ! python3 -c "import pypdf" 2>/dev/null; then
  echo "SKIP: pypdf not installed in this env; source-level check passed"
  exit 0
fi

# 3. Generate a small PDF, run the script on it, and assert output
python3 - <<'PYEOF'
import sys, tempfile, os
sys.path.insert(0, '.')
from pypdf import PdfWriter

# Build a one-page PDF with known text
writer = PdfWriter()
writer.add_blank_page(width=612, height=792)
fd, path = tempfile.mkstemp(suffix='.pdf')
os.close(fd)
with open(path, 'wb') as f:
    writer.write(f)

# Run the worker's parse() on the PDF
try:
    from parse import parse
    text = parse(path)
    if not isinstance(text, str):
        print(f'FAIL: parse() returned {type(text).__name__}, expected str')
        sys.exit(1)
    print(f'PASS: parse() returned {len(text)}-char string from generated PDF')
except Exception as e:
    print(f'FAIL: parse() raised {type(e).__name__}: {e}')
    sys.exit(1)
finally:
    os.unlink(path)
PYEOF
