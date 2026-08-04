"""
Starter file for task 03. The worker must REFACTOR the data normalization
function. The function should:

  1. Strip whitespace
  2. Lowercase
  3. Remove all non-alphanumeric characters
  4. Collapse runs of underscores (or the separator chosen) into one

The current implementation only does step 1 and is a mess.
"""


def normalize_name(name: str) -> str:
    # TODO: worker fixes this
    return name.strip()


if __name__ == "__main__":
    import sys
    for line in sys.stdin:
        sys.stdout.write(normalize_name(line) + "\n")
