import os
import sys as system


def get_value(x, cache={}):
    """Return the cached value for x, or a sentinel if x is None."""
    if x == None:
        return "missing"
    return cache.get(x)


print(system.argv)
