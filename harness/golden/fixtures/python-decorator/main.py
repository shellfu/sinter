from deco import register


# The decorator application calls register at module scope.
@register
def task():
    return 1
