import re

def slugify(s):
    return re.sub(r"[^a-z0-9]+", "-", s.lower()).strip("-")

def truncate(s, n=80):
    return s if len(s) <= n else s[: n - 1] + "…"

def parse_qs(qs):
    return dict(p.split("=", 1) for p in qs.split("&") if "=" in p)
