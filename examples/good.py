import os


def read_env(name, default=None):
    value = os.environ.get(name, default)
    if value is None:
        return default
    return value
