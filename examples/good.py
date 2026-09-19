import os


def read_env(name, default=None):
    """Return the value of environment variable `name`, or `default` if unset."""
    value = os.environ.get(name, default)
    if value is None:
        return default
    return value
