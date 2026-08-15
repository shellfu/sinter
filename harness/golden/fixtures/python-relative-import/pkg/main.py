from .util import helper


# Calls the sibling-module helper via a relative import.
def run():
    return helper("x")
