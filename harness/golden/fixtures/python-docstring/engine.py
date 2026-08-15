# This comment is NOT the doc; the docstring below is.
def rebuild(graph):
    """Rebuilds the derived cache from graph facts."""
    return graph


class Planner:
    """Plans incremental work batches."""

    def plan(self):
        """Produces the next batch."""
        return rebuild(self)
