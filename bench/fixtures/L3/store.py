import json
import os
import threading


class Store:
    def __init__(self, path):
        self.path = path
        self._lock = threading.Lock()
        if not os.path.exists(path):
            self._write([])
        self._posts = self._read()
        self._next = max([p["id"] for p in self._posts], default=0) + 1

    def _read(self):
        with open(self.path) as f:
            return json.load(f)

    def _write(self, posts):
        with open(self.path, "w") as f:
            json.dump(posts, f)

    def all(self):
        return list(self._posts)

    def get(self, pid):
        return next((p for p in self._posts if p["id"] == pid), None)

    def add(self, post):
        with self._lock:
            post = dict(post, id=self._next)
            self._next += 1
            self._posts.append(post)
            self._write(self._posts)
            return post
