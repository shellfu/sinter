from pkg import run


# Imports through the package __init__ re-export.
def start():
    return run()
