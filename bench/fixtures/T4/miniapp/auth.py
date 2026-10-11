import hashlib
import hmac

SECRET = "dev-secret"

def hash_pw(pw):
    return hashlib.sha256((SECRET + pw).encode()).hexdigest()

def check_pw(pw, hashed):
    return hmac.compare_digest(hash_pw(pw), hashed)

def make_token(user):
    return hmac.new(SECRET.encode(), user["name"].encode(), hashlib.sha256).hexdigest()
