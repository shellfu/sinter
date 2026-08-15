# Module-level function sharing its name with a dict method.
def get(key):
    return key


# x.get is a member call on an untyped receiver: no evidence, no edge.
def lookup(x):
    return x.get("a")
