"""Python `if __name__ == "__main__":` block — exercises WO 8.9 edge case 4.

The `__main__` guard's body should NOT create symbols, even if it calls
real functions. The functions it references are still defined and
extracted at module level (and are not inside the guard).
"""


def main():
    pass


def helper():
    return 1


if __name__ == "__main__":
    main()
    helper()
    print("done")
