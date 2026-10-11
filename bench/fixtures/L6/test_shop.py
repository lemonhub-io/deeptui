import unittest

import cart
import discount
from catalog import CATALOG


def state():
    return {"cart": [], "wishlist": [], "orders": []}


class TestShop(unittest.TestCase):
    def test_add_merges_lines(self):
        s = state()
        cart.add_to_cart(s, CATALOG["A1"], 1)
        cart.add_to_cart(s, CATALOG["A1"], 2)
        self.assertEqual(s["cart"], [{"sku": "A1", "qty": 3}])

    def test_coupons_compound_in_order(self):
        # 100 - 10% = 90; 90 - 5 = 85
        self.assertEqual(
            discount.apply_coupons(100.0, [("percent", 10), ("flat", 5)]), 85.0)
        # order matters: 100 - 5 = 95; 95 - 10% = 85.5
        self.assertEqual(
            discount.apply_coupons(100.0, [("flat", 5), ("percent", 10)]), 85.5)

    # TODO: add wishlist tests


if __name__ == "__main__":
    unittest.main()
