_store = {}

def put(ns, key, val):
    _store.setdefault(ns, {})[key] = val

def get(ns, key):
    return _store.get(ns, {}).get(key)

def all_(ns):
    return list(_store.get(ns, {}).values())
