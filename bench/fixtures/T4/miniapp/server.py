from router import ROUTES
from auth import check_pw, make_token

USERS = {}

def handle(req):
    """Dispatch a request dict {method, path, body, headers} to a route."""
    if req.get("method") == "POST" and req.get("path") == "/login":
        u = USERS.get(req["body"].get("user"))
        if u and check_pw(req["body"].get("pw", ""), u["pw"]):
            return {"status": 200, "body": {"token": make_token(u)}}
        return {"status": 403, "body": {"error": "denied"}}
    fn = ROUTES.get((req.get("method"), req.get("path")))
    if fn is None:
        return {"status": 404, "body": {"error": "not found"}}
    return fn(req)
