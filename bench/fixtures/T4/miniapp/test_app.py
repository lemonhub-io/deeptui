from server import handle, USERS
from auth import hash_pw

USERS["a"] = {"name": "a", "pw": hash_pw("x")}
r = handle({"method": "GET", "path": "/health"})
assert r["status"] == 200
r = handle({"method": "GET", "path": "/nope"})
assert r["status"] == 404
print("app ok")
