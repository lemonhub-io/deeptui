def index(req):
    return {"status": 200, "body": {"msg": "hello"}}

def health(req):
    return {"status": 200, "body": {"ok": True}}

ROUTES = {
    ("GET", "/"): index,
    ("GET", "/health"): health,
}
