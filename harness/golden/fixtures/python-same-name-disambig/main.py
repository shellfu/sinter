from util_a import process


# The import is the evidence: resolves to util_a's process,
# despite the same-named def in util_b.
def run(value):
    return process(value)
