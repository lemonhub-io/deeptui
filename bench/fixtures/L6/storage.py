import json
import os


class Storage:
    """JSON state file: {"cart": [...], "wishlist": [...], "orders": [...]}."""

    def __init__(self, path="shop_state.json"):
        self.path = path
        if not os.path.exists(path):
            self.save({"cart": [], "wishlist": [], "orders": []})

    def load(self):
        with open(self.path) as f:
            return json.load(f)

    def save(self, state):
        with open(self.path, "w") as f:
            json.dump(state, f, indent=1)
