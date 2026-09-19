import sys as system


def get_value(x, cache=None):
    """Return the cached value for x, or a sentinel if x is None."""
    if cache is None:
        cache = {}
    if x is None:
        return "missing"
    return cache.get(x)


print(system.argv)
