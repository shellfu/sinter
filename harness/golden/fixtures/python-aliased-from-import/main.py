def run(graph):
    from pkg.mod import cluster as _cluster, score_all as _score_all
    _score_all(graph)
    return _cluster(graph, resolution=2.0)
