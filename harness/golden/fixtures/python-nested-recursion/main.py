# Recurses at top level.
def count(n):
    if n:
        return count(n - 1)
    return 0


def outer(tree):
    def walk(node):
        for child in node:
            walk(child)
        return visit(node)

    def visit(node):
        return walk(node)

    return walk(tree)
