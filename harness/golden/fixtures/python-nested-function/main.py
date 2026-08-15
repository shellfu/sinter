# Called from inside the nested function.
def leaf():
    return 1


def outer():
    # Nested function gets its own node and call scope.
    def inner():
        return leaf()

    return inner()
