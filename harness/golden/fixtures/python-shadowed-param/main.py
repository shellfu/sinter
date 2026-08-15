# Module-level function sharing a name with a param and a local below.
def handler(evt):
    return evt


# The param shadows the module-level handler: no edge to it.
def via(handler):
    return handler("x")


# The local rebind shadows it too: no edge.
def run(data):
    handler = data
    return handler()
