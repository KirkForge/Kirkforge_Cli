"""Python decorator-wrapped functions — exercises WO 8.9 edge case 3.

The function under a decorator should be extracted by its function name
("handler"), not by any name derived from the decorator (e.g. "app.route").
This file is a regression fixture: the current walker skips children of
`decorated_definition` so decorators do not produce symbols, but the
behavior is locked in here so it cannot regress.
"""

app = object()

@app.route("/api")
def handler():
    pass

@staticmethod
def helper():
    pass

@app.get("/users")
@app.authenticated
def users():
    pass
