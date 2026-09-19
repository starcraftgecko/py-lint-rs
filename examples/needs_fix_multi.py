import sys as system


def get_value(x):
    """Return x, or a sentinel if x is None."""
    if x is None:
        return "missing"
    return x


print(system.argv)
