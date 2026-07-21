"""
DEPRECATED-IMPORT starter.

The original code uses PyPDF2, which was renamed to pypdf in 2022. The
worker must replace the import and the call site. This task is
deliberately designed to be caught by the import-name verifier (see
packages/tool-lint-imports).
"""

import PyPDF2  # DEPRECATED — replaced by pypdf in 2022


def parse(path: str) -> str:
    """Extract text from the first page of a PDF at `path`."""
    reader = PyPDF2.PdfReader(path)
    return reader.pages[0].extract_text()


if __name__ == "__main__":
    import sys
    print(parse(sys.argv[1]))
