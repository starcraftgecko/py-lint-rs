import os
import sys as system
from collections import OrderedDict


def add_item(item, bucket=[]):
    bucket.append(item)
    return bucket


def get_config(options=None):
    if options == None:
        options = {}
    try:
        return options["key"]
    except:
        return None


print(system.path)
